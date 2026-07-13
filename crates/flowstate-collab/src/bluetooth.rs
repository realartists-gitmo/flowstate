//! Nearby rendezvous over Bluetooth Low Energy.
//!
//! BLE advertising payloads are intentionally only locators. The complete
//! signed advertisement is read from a GATT characteristic, avoiding the
//! 20–28 byte legacy advertising limits without truncating security data.

use std::{future::Future, pin::Pin, time::Duration};

use anyhow::{Result, bail};
use iroh::PublicKey;

use crate::discovery::{DiscoveryAdvertisement, DiscoveryTransport, RendezvousBackend};

pub const FLOWSTATE_SERVICE_UUID: u128 = 0x0198_f10e_7679_7cd0_961d_f107_2fc0_b002;
pub const FLOWSTATE_ADVERTISEMENT_CHARACTERISTIC_UUID: u128 = 0x0198_f10e_7679_7cd0_961d_f107_2fc0_b003;
const MAX_GATT_ADVERTISEMENT_BYTES: usize = 60 * 1024;

/// Common framing used by every native adapter. A length prefix lets clients
/// issue offset reads until the entire postcard value has arrived.
pub fn encode_gatt_advertisement(advertisement: &DiscoveryAdvertisement) -> Result<Vec<u8>> {
  let bytes = postcard::to_stdvec(advertisement)?;
  if bytes.len() > MAX_GATT_ADVERTISEMENT_BYTES {
    bail!("Bluetooth discovery advertisement is too large");
  }
  let len = u32::try_from(bytes.len()).expect("bounded advertisement length");
  let mut framed = Vec::with_capacity(4 + bytes.len());
  framed.extend_from_slice(&len.to_le_bytes());
  framed.extend_from_slice(&bytes);
  Ok(framed)
}

pub fn decode_gatt_advertisement(framed: &[u8]) -> Result<DiscoveryAdvertisement> {
  let header: [u8; 4] = framed
    .get(..4)
    .ok_or_else(|| anyhow::anyhow!("Bluetooth advertisement is missing its length"))?
    .try_into()
    .expect("four-byte slice");
  let len = usize::try_from(u32::from_le_bytes(header)).expect("u32 fits usize");
  if len > MAX_GATT_ADVERTISEMENT_BYTES || framed.len() != len + 4 {
    bail!("Bluetooth advertisement length is invalid");
  }
  postcard::from_bytes(&framed[4..]).map_err(Into::into)
}

#[cfg(target_os = "linux")]
mod linux {
  use std::{collections::BTreeMap, sync::Arc};

  use anyhow::{Context as _, ensure};
  use bluer::{
    Adapter, AdapterEvent, DiscoveryFilter, Uuid,
    adv::{Advertisement, AdvertisementHandle, Type},
    gatt::{
      local::{Application, ApplicationHandle, Characteristic, CharacteristicRead, Service},
      remote::CharacteristicReadRequest,
    },
  };
  use futures_util::{FutureExt as _, StreamExt as _};
  use tokio::sync::Mutex;

  use super::*;

  struct PublishedAdvertisement {
    identity: PublicKey,
    device_id: u128,
    document_fingerprint: [u8; 32],
    _application: ApplicationHandle,
    _advertisement: AdvertisementHandle,
  }

  /// BlueZ implementation used on Linux. Registration handles remain live for
  /// exactly as long as the signed advertisement is published.
  pub struct NativeBluetoothBackend {
    adapter: Adapter,
    published: Mutex<Option<PublishedAdvertisement>>,
    scan_lock: Mutex<()>,
    scan_duration: Duration,
  }

  impl NativeBluetoothBackend {
    pub async fn new(scan_duration: Duration) -> Result<Self> {
      let session = bluer::Session::new().await.context("connecting to BlueZ")?;
      let adapter = session
        .default_adapter()
        .await
        .context("finding the default Bluetooth adapter")?;
      if !adapter
        .is_powered()
        .await
        .context("reading Bluetooth power state")?
      {
        adapter
          .set_powered(true)
          .await
          .context("powering on Bluetooth adapter")?;
      }
      Ok(Self {
        adapter,
        published: Mutex::new(None),
        scan_lock: Mutex::new(()),
        scan_duration,
      })
    }

    async fn publish_inner(&self, advertisement: DiscoveryAdvertisement) -> Result<()> {
      // Most desktop adapters expose one legacy advertising slot. Release the
      // old GATT/advertisement pair before registering its replacement.
      *self.published.lock().await = None;
      let framed = Arc::new(encode_gatt_advertisement(&advertisement)?);
      let read_value = Arc::clone(&framed);
      let application = Application {
        services: vec![Service {
          uuid: service_uuid(),
          primary: true,
          characteristics: vec![Characteristic {
            uuid: characteristic_uuid(),
            read: Some(CharacteristicRead {
              read: true,
              fun: Box::new(move |request| {
                let value = Arc::clone(&read_value);
                async move {
                  let offset = usize::from(request.offset);
                  if offset > value.len() {
                    return Err(bluer::gatt::local::ReqError::InvalidOffset);
                  }
                  let payload = request.mtu.saturating_sub(1).max(1) as usize;
                  Ok(value[offset..value.len().min(offset.saturating_add(payload))].to_vec())
                }
                .boxed()
              }),
              ..Default::default()
            }),
            ..Default::default()
          }],
          ..Default::default()
        }],
        ..Default::default()
      };
      let application_handle = self
        .adapter
        .serve_gatt_application(application)
        .await
        .context("registering Flowstate Bluetooth GATT service")?;

      let mut service_data = BTreeMap::new();
      service_data.insert(service_uuid(), advertisement.document_fingerprint[..16].to_vec());
      let ble_advertisement = Advertisement {
        advertisement_type: Type::Peripheral,
        service_uuids: [service_uuid()].into_iter().collect(),
        service_data,
        discoverable: Some(true),
        local_name: Some("Flowstate".into()),
        ..Default::default()
      };
      let advertisement_handle = match self.adapter.advertise(ble_advertisement).await {
        Ok(handle) => handle,
        Err(error) => {
          drop(application_handle);
          return Err(error).context("advertising Flowstate Bluetooth service");
        },
      };
      *self.published.lock().await = Some(PublishedAdvertisement {
        identity: advertisement.identity,
        device_id: advertisement.device_id,
        document_fingerprint: advertisement.document_fingerprint,
        _application: application_handle,
        _advertisement: advertisement_handle,
      });
      Ok(())
    }

    async fn scan_inner(&self, document_fingerprint: [u8; 32]) -> Result<Vec<DiscoveryAdvertisement>> {
      let _scan_guard = self.scan_lock.lock().await;
      self
        .adapter
        .set_discovery_filter(DiscoveryFilter {
          uuids: [service_uuid()].into_iter().collect(),
          transport: bluer::DiscoveryTransport::Le,
          duplicate_data: false,
          ..Default::default()
        })
        .await
        .context("configuring Flowstate Bluetooth scan")?;
      let stream = self
        .adapter
        .discover_devices()
        .await
        .context("starting Flowstate Bluetooth scan")?;
      futures_util::pin_mut!(stream);
      let deadline = tokio::time::Instant::now() + self.scan_duration;
      let mut found = Vec::new();
      loop {
        let event = match tokio::time::timeout_at(deadline, stream.next()).await {
          Ok(Some(event)) => event,
          Ok(None) | Err(_) => break,
        };
        let AdapterEvent::DeviceAdded(address) = event else { continue };
        let device = self
          .adapter
          .device(address)
          .context("opening discovered Bluetooth device")?;
        if device.rssi().await.ok().flatten().is_none() {
          continue;
        }
        let service_data = device
          .service_data()
          .await
          .unwrap_or_default()
          .unwrap_or_default();
        if service_data.get(&service_uuid()).map(Vec::as_slice) != Some(&document_fingerprint[..16]) {
          continue;
        }
        let remaining = deadline
          .saturating_duration_since(tokio::time::Instant::now())
          .min(Duration::from_secs(3));
        if remaining.is_zero() {
          break;
        }
        match tokio::time::timeout(remaining, read_advertisement(&device)).await {
          Ok(Ok(advertisement)) if advertisement.document_fingerprint == document_fingerprint => found.push(advertisement),
          Ok(Ok(_)) => {},
          Ok(Err(error)) => tracing::debug!(%error, %address, "nearby Flowstate Bluetooth record could not be read"),
          Err(_) => tracing::debug!(%address, "nearby Flowstate Bluetooth read timed out"),
        }
      }
      Ok(found)
    }
  }

  impl RendezvousBackend for NativeBluetoothBackend {
    fn transport(&self) -> DiscoveryTransport {
      DiscoveryTransport::Bluetooth
    }

    fn publish(&self, advertisement: DiscoveryAdvertisement) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
      Box::pin(self.publish_inner(advertisement))
    }

    fn scan(&self, document_fingerprint: [u8; 32]) -> Pin<Box<dyn Future<Output = Result<Vec<DiscoveryAdvertisement>>> + Send + '_>> {
      Box::pin(self.scan_inner(document_fingerprint))
    }

    fn clear(
      &self,
      identity: PublicKey,
      device_id: u128,
      document_fingerprint: [u8; 32],
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
      Box::pin(async move {
        let mut published = self.published.lock().await;
        if published.as_ref().is_some_and(|current| {
          current.identity == identity && current.device_id == device_id && current.document_fingerprint == document_fingerprint
        }) {
          *published = None;
        }
        Ok(())
      })
    }
  }

  async fn read_advertisement(device: &bluer::Device) -> Result<DiscoveryAdvertisement> {
    if !device
      .is_connected()
      .await
      .context("reading Bluetooth connection state")?
    {
      device
        .connect()
        .await
        .context("connecting to nearby Flowstate")?;
    }
    let mut characteristic = None;
    for service in device
      .services()
      .await
      .context("enumerating nearby Flowstate services")?
    {
      if service.uuid().await? != service_uuid() {
        continue;
      }
      for candidate in service.characteristics().await? {
        if candidate.uuid().await? == characteristic_uuid() {
          characteristic = Some(candidate);
          break;
        }
      }
    }
    let characteristic = characteristic.context("Flowstate GATT characteristic is missing")?;
    let mut framed = Vec::new();
    loop {
      ensure!(
        framed.len() <= MAX_GATT_ADVERTISEMENT_BYTES + 4,
        "Bluetooth advertisement exceeded its maximum length"
      );
      let offset = u16::try_from(framed.len()).context("Bluetooth advertisement offset overflow")?;
      let chunk = characteristic
        .read_ext(&CharacteristicReadRequest {
          offset,
          ..Default::default()
        })
        .await
        .context("reading Flowstate GATT characteristic")?;
      ensure!(!chunk.is_empty(), "Bluetooth advertisement ended early");
      framed.extend_from_slice(&chunk);
      if let Some(expected) = expected_framed_len(&framed)?
        && framed.len() >= expected
      {
        ensure!(framed.len() == expected, "Bluetooth advertisement returned trailing bytes");
        break;
      }
    }
    decode_gatt_advertisement(&framed)
  }

  fn expected_framed_len(bytes: &[u8]) -> Result<Option<usize>> {
    let Some(header) = bytes.get(..4) else { return Ok(None) };
    let len = u32::from_le_bytes(header.try_into().expect("four-byte slice")) as usize;
    ensure!(
      len <= MAX_GATT_ADVERTISEMENT_BYTES,
      "Bluetooth advertisement declares an excessive length"
    );
    Ok(Some(len + 4))
  }

  fn service_uuid() -> Uuid {
    Uuid::from_u128(FLOWSTATE_SERVICE_UUID)
  }

  fn characteristic_uuid() -> Uuid {
    Uuid::from_u128(FLOWSTATE_ADVERTISEMENT_CHARACTERISTIC_UUID)
  }
}

#[cfg(target_os = "linux")]
pub use linux::NativeBluetoothBackend;

#[cfg(target_os = "windows")]
mod windows_backend {
  use std::collections::HashSet;

  use anyhow::{Context as _, ensure};
  use tokio::sync::Mutex;
  use windows::{
    Devices::Bluetooth::{
      Advertisement::{
        BluetoothLEAdvertisement, BluetoothLEAdvertisementFilter, BluetoothLEAdvertisementReceivedEventArgs, BluetoothLEAdvertisementWatcher,
        BluetoothLEScanningMode,
      },
      BluetoothCacheMode, BluetoothError, BluetoothLEDevice,
      GenericAttributeProfile::{
        GattCharacteristicProperties, GattCommunicationStatus, GattLocalCharacteristic, GattLocalCharacteristicParameters, GattProtectionLevel,
        GattServiceProvider, GattServiceProviderAdvertisingParameters,
      },
    },
    Foundation::TypedEventHandler,
    Storage::Streams::{DataReader, DataWriter, IBuffer},
    core::GUID,
  };

  use super::*;

  struct PublishedAdvertisement {
    identity: PublicKey,
    device_id: u128,
    document_fingerprint: [u8; 32],
    provider: GattServiceProvider,
    _characteristic: GattLocalCharacteristic,
  }

  pub struct NativeBluetoothBackend {
    published: Mutex<Option<PublishedAdvertisement>>,
    scan_lock: Mutex<()>,
    scan_duration: Duration,
  }

  impl NativeBluetoothBackend {
    pub async fn new(scan_duration: Duration) -> Result<Self> {
      // WinRT's activation factory initializes MTA usage on demand. Opening a
      // watcher here gives settings a synchronous, actionable platform error.
      BluetoothLEAdvertisementWatcher::new().context("opening Windows Bluetooth LE watcher")?;
      Ok(Self {
        published: Mutex::new(None),
        scan_lock: Mutex::new(()),
        scan_duration,
      })
    }

    async fn publish_inner(&self, advertisement: DiscoveryAdvertisement) -> Result<()> {
      if let Some(current) = self.published.lock().await.take() {
        let _ = current.provider.StopAdvertising();
      }

      let framed = encode_gatt_advertisement(&advertisement)?;
      let provider_result = GattServiceProvider::CreateAsync(service_guid())
        .context("creating Flowstate Windows GATT provider")?
        .await
        .context("creating Flowstate Windows GATT provider")?;
      ensure!(
        provider_result.Error()? == BluetoothError::Success,
        "Windows could not create the Flowstate GATT service"
      );
      let provider = provider_result.ServiceProvider()?;

      let parameters = GattLocalCharacteristicParameters::new()?;
      parameters.SetCharacteristicProperties(GattCharacteristicProperties::Read)?;
      parameters.SetReadProtectionLevel(GattProtectionLevel::Plain)?;
      parameters.SetUserDescription(&"Flowstate signed discovery record".into())?;
      {
        let framed_buffer = bytes_to_buffer(&framed)?;
        parameters.SetStaticValue(&framed_buffer)?;
      }
      let characteristic_result = provider
        .Service()?
        .CreateCharacteristicAsync(characteristic_guid(), &parameters)?
        .await?;
      ensure!(
        characteristic_result.Error()? == BluetoothError::Success,
        "Windows could not create the Flowstate GATT characteristic"
      );
      let characteristic = characteristic_result.Characteristic()?;

      let advertising = GattServiceProviderAdvertisingParameters::new()?;
      advertising.SetIsConnectable(true)?;
      advertising.SetIsDiscoverable(true)?;
      // ServiceData is optional on older Windows builds. The service UUID is
      // still filtered before connecting and the full signed document scope is
      // always checked after reading the characteristic.
      {
        let locator = bytes_to_buffer(&advertisement.document_fingerprint[..16])?;
        let _ = advertising.SetServiceData(&locator);
      }
      provider
        .StartAdvertisingWithParameters(&advertising)
        .context("starting Flowstate Windows Bluetooth advertisement")?;

      *self.published.lock().await = Some(PublishedAdvertisement {
        identity: advertisement.identity,
        device_id: advertisement.device_id,
        document_fingerprint: advertisement.document_fingerprint,
        provider,
        _characteristic: characteristic,
      });
      Ok(())
    }

    async fn scan_inner(&self, document_fingerprint: [u8; 32]) -> Result<Vec<DiscoveryAdvertisement>> {
      let _guard = self.scan_lock.lock().await;
      let filter_record = BluetoothLEAdvertisement::new()?;
      filter_record.ServiceUuids()?.Append(service_guid())?;
      let filter = BluetoothLEAdvertisementFilter::new()?;
      filter.SetAdvertisement(&filter_record)?;
      let watcher = BluetoothLEAdvertisementWatcher::Create(&filter)?;
      watcher.SetScanningMode(BluetoothLEScanningMode::Active)?;

      let (addresses_tx, addresses_rx) = async_channel::bounded(64);
      let received_token = {
        let received = TypedEventHandler::<BluetoothLEAdvertisementWatcher, BluetoothLEAdvertisementReceivedEventArgs>::new(move |_, args| {
          if let Ok(args) = args.ok()
            && let Ok(address) = args.BluetoothAddress()
          {
            let _ = addresses_tx.try_send(address);
          }
          Ok(())
        });
        watcher.Received(&received)?
      };
      watcher.Start().context("starting Windows Bluetooth scan")?;

      let deadline = tokio::time::Instant::now() + self.scan_duration;
      let mut seen = HashSet::new();
      let mut found = Vec::new();
      loop {
        let address = match tokio::time::timeout_at(deadline, addresses_rx.recv()).await {
          Ok(Ok(address)) => address,
          Ok(Err(_)) | Err(_) => break,
        };
        if !seen.insert(address) {
          continue;
        }
        let remaining = deadline
          .saturating_duration_since(tokio::time::Instant::now())
          .min(Duration::from_secs(3));
        if remaining.is_zero() {
          break;
        }
        match tokio::time::timeout(remaining, read_advertisement(address)).await {
          Ok(Ok(advertisement)) if advertisement.document_fingerprint == document_fingerprint => found.push(advertisement),
          Ok(Ok(_)) => {},
          Ok(Err(error)) => tracing::debug!(%error, address, "nearby Windows Flowstate Bluetooth record could not be read"),
          Err(_) => tracing::debug!(address, "nearby Windows Flowstate Bluetooth read timed out"),
        }
      }
      let _ = watcher.Stop();
      let _ = watcher.RemoveReceived(received_token);
      Ok(found)
    }
  }

  impl RendezvousBackend for NativeBluetoothBackend {
    fn transport(&self) -> DiscoveryTransport {
      DiscoveryTransport::Bluetooth
    }

    fn publish(&self, advertisement: DiscoveryAdvertisement) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
      Box::pin(self.publish_inner(advertisement))
    }

    fn scan(&self, document_fingerprint: [u8; 32]) -> Pin<Box<dyn Future<Output = Result<Vec<DiscoveryAdvertisement>>> + Send + '_>> {
      Box::pin(self.scan_inner(document_fingerprint))
    }

    fn clear(
      &self,
      identity: PublicKey,
      device_id: u128,
      document_fingerprint: [u8; 32],
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
      Box::pin(async move {
        let mut published = self.published.lock().await;
        if published.as_ref().is_some_and(|current| {
          current.identity == identity && current.device_id == device_id && current.document_fingerprint == document_fingerprint
        }) {
          if let Some(current) = published.take() {
            current.provider.StopAdvertising()?;
          }
        }
        Ok(())
      })
    }
  }

  async fn read_advertisement(address: u64) -> Result<DiscoveryAdvertisement> {
    let device = BluetoothLEDevice::FromBluetoothAddressAsync(address)?
      .await
      .context("opening nearby Windows Bluetooth device")?;
    let services = device
      .GetGattServicesForUuidWithCacheModeAsync(service_guid(), BluetoothCacheMode::Uncached)?
      .await?;
    ensure!(
      services.Status()? == GattCommunicationStatus::Success,
      "reading Flowstate GATT services failed"
    );
    let service = {
      let services = services.Services()?;
      ensure!(services.Size()? > 0, "Flowstate GATT service is missing");
      services.GetAt(0)?
    };
    let characteristics = service
      .GetCharacteristicsForUuidWithCacheModeAsync(characteristic_guid(), BluetoothCacheMode::Uncached)?
      .await?;
    ensure!(
      characteristics.Status()? == GattCommunicationStatus::Success,
      "reading Flowstate GATT characteristics failed"
    );
    let characteristic = {
      let characteristics = characteristics.Characteristics()?;
      ensure!(characteristics.Size()? > 0, "Flowstate GATT characteristic is missing");
      characteristics.GetAt(0)?
    };
    let value = characteristic
      .ReadValueWithCacheModeAsync(BluetoothCacheMode::Uncached)?
      .await?;
    ensure!(value.Status()? == GattCommunicationStatus::Success, "reading Flowstate GATT value failed");
    decode_gatt_advertisement(&buffer_to_bytes(value.Value()?)?)
  }

  fn bytes_to_buffer(bytes: &[u8]) -> Result<IBuffer> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(bytes)?;
    writer.DetachBuffer().map_err(Into::into)
  }

  fn buffer_to_bytes(buffer: IBuffer) -> Result<Vec<u8>> {
    let reader = DataReader::FromBuffer(&buffer)?;
    let mut bytes = vec![0; reader.UnconsumedBufferLength()? as usize];
    reader.ReadBytes(&mut bytes)?;
    Ok(bytes)
  }

  const fn service_guid() -> GUID {
    GUID::from_u128(FLOWSTATE_SERVICE_UUID)
  }

  const fn characteristic_guid() -> GUID {
    GUID::from_u128(FLOWSTATE_ADVERTISEMENT_CHARACTERISTIC_UUID)
  }
}

#[cfg(target_os = "windows")]
pub use windows_backend::NativeBluetoothBackend;

#[cfg(target_os = "macos")]
mod macos_backend {
  use std::{
    sync::{
      Arc, Mutex,
      atomic::{AtomicBool, Ordering},
    },
    thread,
  };

  use anyhow::{Context as _, ensure};
  use objc2::{
    AnyThread, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
  };
  use objc2_core_bluetooth::{
    CBAdvertisementDataServiceUUIDsKey, CBAttributePermissions, CBCentralManager, CBCentralManagerDelegate, CBCharacteristic,
    CBCharacteristicProperties, CBManagerState, CBMutableCharacteristic, CBMutableService, CBPeripheral, CBPeripheralDelegate,
    CBPeripheralManager, CBPeripheralManagerDelegate, CBService, CBUUID,
  };
  use objc2_foundation::{NSArray, NSData, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSString};

  use super::*;

  #[derive(Default)]
  struct MacDelegateState {
    powered_on: AtomicBool,
    scan_sender: Mutex<Option<async_channel::Sender<DiscoveryAdvertisement>>>,
    peripherals: Mutex<Vec<Retained<CBPeripheral>>>,
  }

  struct MacDelegateIvars {
    state: Arc<MacDelegateState>,
  }

  define_class!(
    // SAFETY: NSObject has no subclassing requirements and this class has no
    // Drop implementation. All mutable Rust state is synchronized.
    #[unsafe(super = NSObject)]
    #[thread_kind = AnyThread]
    #[ivars = MacDelegateIvars]
    struct MacBluetoothDelegate;

    // SAFETY: These delegate protocols impose callback signatures only.
    unsafe impl NSObjectProtocol for MacBluetoothDelegate {}

    unsafe impl CBCentralManagerDelegate for MacBluetoothDelegate {
      #[unsafe(method(centralManagerDidUpdateState:))]
      unsafe fn central_manager_did_update_state(&self, central: &CBCentralManager) {
        let powered = unsafe { central.state() } == CBManagerState::PoweredOn;
        self
          .ivars()
          .state
          .powered_on
          .store(powered, Ordering::Release);
      }

      #[unsafe(method(centralManager:didDiscoverPeripheral:advertisementData:RSSI:))]
      unsafe fn central_manager_did_discover(
        &self,
        central: &CBCentralManager,
        peripheral: &CBPeripheral,
        _advertisement_data: &NSDictionary<NSString, AnyObject>,
        _rssi: &NSNumber,
      ) {
        let retained = Retained::retain(peripheral);
        unsafe {
          peripheral.setDelegate(Some(ProtocolObject::from_ref(self)));
          central.connectPeripheral_options(peripheral, None);
        }
        self
          .ivars()
          .state
          .peripherals
          .lock()
          .expect("Bluetooth peripheral lock poisoned")
          .push(retained);
      }

      #[unsafe(method(centralManager:didConnectPeripheral:))]
      unsafe fn central_manager_did_connect(&self, _central: &CBCentralManager, peripheral: &CBPeripheral) {
        let services = NSArray::from_retained_slice(&[service_uuid()]);
        unsafe { peripheral.discoverServices(Some(&services)) };
      }
    }

    unsafe impl CBPeripheralDelegate for MacBluetoothDelegate {
      #[unsafe(method(peripheral:didDiscoverServices:))]
      unsafe fn peripheral_did_discover_services(&self, peripheral: &CBPeripheral, error: Option<&NSError>) {
        if error.is_some() {
          return;
        }
        let expected = service_uuid();
        if let Some(services) = unsafe { peripheral.services() } {
          for service in &*services {
            if unsafe { service.UUID() } == expected {
              let characteristics = NSArray::from_retained_slice(&[characteristic_uuid()]);
              unsafe { peripheral.discoverCharacteristics_forService(Some(&characteristics), service) };
            }
          }
        }
      }

      #[unsafe(method(peripheral:didDiscoverCharacteristicsForService:error:))]
      unsafe fn peripheral_did_discover_characteristics(&self, peripheral: &CBPeripheral, service: &CBService, error: Option<&NSError>) {
        if error.is_some() {
          return;
        }
        let expected = characteristic_uuid();
        if let Some(characteristics) = unsafe { service.characteristics() } {
          for characteristic in &*characteristics {
            if unsafe { characteristic.UUID() } == expected {
              unsafe { peripheral.readValueForCharacteristic(characteristic) };
            }
          }
        }
      }

      #[unsafe(method(peripheral:didUpdateValueForCharacteristic:error:))]
      unsafe fn peripheral_did_update_value(&self, _peripheral: &CBPeripheral, characteristic: &CBCharacteristic, error: Option<&NSError>) {
        if error.is_some() {
          return;
        }
        let Some(value) = (unsafe { characteristic.value() }) else { return };
        let bytes = unsafe { value.as_bytes_unchecked() };
        let Ok(advertisement) = decode_gatt_advertisement(bytes) else { return };
        if let Some(sender) = self
          .ivars()
          .state
          .scan_sender
          .lock()
          .expect("Bluetooth scan sender lock poisoned")
          .as_ref()
        {
          let _ = sender.try_send(advertisement);
        }
      }
    }

    unsafe impl CBPeripheralManagerDelegate for MacBluetoothDelegate {
      #[unsafe(method(peripheralManagerDidUpdateState:))]
      unsafe fn peripheral_manager_did_update_state(&self, peripheral: &CBPeripheralManager) {
        let powered = unsafe { peripheral.state() } == CBManagerState::PoweredOn;
        self
          .ivars()
          .state
          .powered_on
          .store(powered, Ordering::Release);
      }
    }
  );

  impl MacBluetoothDelegate {
    fn new(state: Arc<MacDelegateState>) -> Retained<Self> {
      let this = Self::alloc().set_ivars(MacDelegateIvars { state });
      unsafe { msg_send![super(this), init] }
    }
  }

  struct PublishedAdvertisement {
    identity: PublicKey,
    device_id: u128,
    document_fingerprint: [u8; 32],
    manager: Retained<CBPeripheralManager>,
    _delegate: Retained<MacBluetoothDelegate>,
    _service: Retained<CBMutableService>,
    _characteristic: Retained<CBMutableCharacteristic>,
    _advertisement: Retained<NSDictionary<NSString, AnyObject>>,
  }

  pub struct NativeBluetoothBackend {
    published: Mutex<Option<PublishedAdvertisement>>,
    scan_lock: tokio::sync::Mutex<()>,
    scan_duration: Duration,
  }

  impl NativeBluetoothBackend {
    pub async fn new(scan_duration: Duration) -> Result<Self> {
      Ok(Self {
        published: Mutex::new(None),
        scan_lock: tokio::sync::Mutex::new(()),
        scan_duration,
      })
    }

    fn publish_inner(&self, advertisement: DiscoveryAdvertisement) -> Result<()> {
      let mut published = self
        .published
        .lock()
        .expect("Bluetooth publication lock poisoned");
      if let Some(current) = published.take() {
        unsafe {
          current.manager.stopAdvertising();
          current.manager.removeAllServices();
          current.manager.setDelegate(None);
        }
      }

      let state = Arc::new(MacDelegateState::default());
      let delegate = MacBluetoothDelegate::new(Arc::clone(&state));
      let manager =
        unsafe { CBPeripheralManager::initWithDelegate_queue(CBPeripheralManager::alloc(), Some(ProtocolObject::from_ref(&*delegate)), None) };
      wait_for_power(&state, "macOS Bluetooth peripheral manager")?;

      let framed = NSData::from_vec(encode_gatt_advertisement(&advertisement)?);
      let characteristic = unsafe {
        CBMutableCharacteristic::initWithType_properties_value_permissions(
          CBMutableCharacteristic::alloc(),
          &characteristic_uuid(),
          CBCharacteristicProperties::Read,
          Some(&framed),
          CBAttributePermissions::Readable,
        )
      };
      let service = unsafe { CBMutableService::initWithType_primary(CBMutableService::alloc(), &service_uuid(), true) };
      let characteristics = NSArray::from_slice(&[&*characteristic]);
      unsafe { service.setCharacteristics(Some(&characteristics)) };
      unsafe { manager.addService(&service) };

      let advertised_services = NSArray::from_retained_slice(&[service_uuid()]);
      let advertised_services_object: &AnyObject = advertised_services.as_ref();
      let advertisement_data =
        unsafe { NSDictionary::<NSString, AnyObject>::from_slices(&[CBAdvertisementDataServiceUUIDsKey], &[advertised_services_object]) };
      unsafe { manager.startAdvertising(Some(&advertisement_data)) };

      *published = Some(PublishedAdvertisement {
        identity: advertisement.identity,
        device_id: advertisement.device_id,
        document_fingerprint: advertisement.document_fingerprint,
        manager,
        _delegate: delegate,
        _service: service,
        _characteristic: characteristic,
        _advertisement: advertisement_data,
      });
      Ok(())
    }

    async fn scan_inner(&self, document_fingerprint: [u8; 32]) -> Result<Vec<DiscoveryAdvertisement>> {
      let _guard = self.scan_lock.lock().await;
      let scan_duration = self.scan_duration;
      tokio::task::spawn_blocking(move || scan_blocking(document_fingerprint, scan_duration))
        .await
        .context("macOS Bluetooth scanner task stopped")?
    }
  }

  impl RendezvousBackend for NativeBluetoothBackend {
    fn transport(&self) -> DiscoveryTransport {
      DiscoveryTransport::Bluetooth
    }

    fn publish(&self, advertisement: DiscoveryAdvertisement) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
      Box::pin(async move { self.publish_inner(advertisement) })
    }

    fn scan(&self, document_fingerprint: [u8; 32]) -> Pin<Box<dyn Future<Output = Result<Vec<DiscoveryAdvertisement>>> + Send + '_>> {
      Box::pin(self.scan_inner(document_fingerprint))
    }

    fn clear(
      &self,
      identity: PublicKey,
      device_id: u128,
      document_fingerprint: [u8; 32],
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
      Box::pin(async move {
        let mut published = self
          .published
          .lock()
          .expect("Bluetooth publication lock poisoned");
        if published.as_ref().is_some_and(|current| {
          current.identity == identity && current.device_id == device_id && current.document_fingerprint == document_fingerprint
        }) && let Some(current) = published.take()
        {
          unsafe {
            current.manager.stopAdvertising();
            current.manager.removeAllServices();
            current.manager.setDelegate(None);
          }
        }
        Ok(())
      })
    }
  }

  fn scan_blocking(document_fingerprint: [u8; 32], duration: Duration) -> Result<Vec<DiscoveryAdvertisement>> {
    let state = Arc::new(MacDelegateState::default());
    let (sender, receiver) = async_channel::bounded(64);
    *state
      .scan_sender
      .lock()
      .expect("Bluetooth scan sender lock poisoned") = Some(sender);
    let delegate = MacBluetoothDelegate::new(Arc::clone(&state));
    let manager =
      unsafe { CBCentralManager::initWithDelegate_queue(CBCentralManager::alloc(), Some(ProtocolObject::from_ref(&*delegate)), None) };
    wait_for_power(&state, "macOS Bluetooth central manager")?;
    let services = NSArray::from_retained_slice(&[service_uuid()]);
    unsafe { manager.scanForPeripheralsWithServices_options(Some(&services), None) };
    thread::sleep(duration);
    unsafe { manager.stopScan() };
    for peripheral in state
      .peripherals
      .lock()
      .expect("Bluetooth peripheral lock poisoned")
      .iter()
    {
      unsafe { manager.cancelPeripheralConnection(peripheral) };
    }
    unsafe { manager.setDelegate(None) };

    let mut found = Vec::new();
    while let Ok(advertisement) = receiver.try_recv() {
      if advertisement.document_fingerprint == document_fingerprint {
        found.push(advertisement);
      }
    }
    Ok(found)
  }

  fn wait_for_power(state: &MacDelegateState, label: &str) -> Result<()> {
    for _ in 0..40 {
      if state.powered_on.load(Ordering::Acquire) {
        return Ok(());
      }
      thread::sleep(Duration::from_millis(50));
    }
    ensure!(false, "{label} is not powered on")
  }

  fn service_uuid() -> Retained<CBUUID> {
    let string = NSString::from_str(&uuid::Uuid::from_u128(FLOWSTATE_SERVICE_UUID).to_string());
    unsafe { CBUUID::UUIDWithString(&string) }
  }

  fn characteristic_uuid() -> Retained<CBUUID> {
    let string = NSString::from_str(&uuid::Uuid::from_u128(FLOWSTATE_ADVERTISEMENT_CHARACTERISTIC_UUID).to_string());
    unsafe { CBUUID::UUIDWithString(&string) }
  }
}

#[cfg(target_os = "macos")]
pub use macos_backend::NativeBluetoothBackend;

/// Other targets keep the wire format shared, but must provide their native
/// CoreBluetooth/WinRT host from the application target. This explicit error
/// avoids silently pretending that nearby discovery is active.
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub struct NativeBluetoothBackend;

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
impl NativeBluetoothBackend {
  pub async fn new(_scan_duration: Duration) -> Result<Self> {
    bail!("native Bluetooth rendezvous is not available in this build")
  }
}

#[cfg(test)]
mod tests {
  use iroh::{EndpointAddr, SecretKey};

  use crate::{SessionId, identity::PortableIdentitySecret};

  use super::*;

  #[test]
  fn gatt_framing_round_trips_signed_advertisements() {
    let identity = PortableIdentitySecret::generate();
    let advertisement = DiscoveryAdvertisement::issue(
      &identity,
      7,
      [8; 32],
      SessionId::from_bytes([9; 32]),
      EndpointAddr::new(SecretKey::generate().public()),
      100,
      identity.sign_profile(1, "Alex".into(), 0x334455, None),
    );
    let framed = encode_gatt_advertisement(&advertisement).unwrap();
    assert_eq!(decode_gatt_advertisement(&framed).unwrap(), advertisement);
    assert!(decode_gatt_advertisement(&framed[..framed.len() - 1]).is_err());
  }
}
