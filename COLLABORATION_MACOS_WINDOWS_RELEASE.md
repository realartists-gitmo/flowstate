# Collaboration discovery: macOS and Windows release work

This note describes the work required to release nearby Bluetooth discovery on
macOS and Windows, and records the remaining gaps in the broader collaboration
plan as of July 2026.

The shared, security-sensitive wire model already exists in
`flowstate-collab`:

- `DiscoveryAdvertisement` is signed, scoped to a document fingerprint, and
  expires.
- `RendezvousBackend` and `RendezvousSet` provide the common publish, scan, and
  clear lifecycle.
- BLE broadcasts only a short locator: the Flowstate service UUID plus the
  first 16 bytes of the document fingerprint.
- The complete postcard-encoded signed advertisement is length-prefixed and
  read from the Flowstate GATT characteristic. It is never truncated into the
  small advertising payload.
- Results must pass `eligible_advertisements`, including signature, expiry,
  document, deduplication, and standing-access checks, before reaching UI.

The Linux implementation in `crates/flowstate-collab/src/bluetooth.rs` is the
behavioral reference. Non-Linux builds currently return an explicit
"unavailable" error; they must not ship with nearby discovery presented as
active until the corresponding native host below is complete.

## macOS: CoreBluetooth host

Implement a `RendezvousBackend` using CoreBluetooth rather than attempting to
share the BlueZ implementation.

### Peripheral/publishing side

- Own one long-lived `CBPeripheralManager` on a queue whose callbacks can be
  bridged safely into the app's async runtime.
- Publish the Flowstate primary service UUID and one read-only characteristic
  using `FLOWSTATE_ADVERTISEMENT_CHARACTERISTIC_UUID`.
- Implement characteristic reads with CoreBluetooth's requested offset. Return
  the same length-prefixed bytes produced by `encode_gatt_advertisement` and
  reject invalid/out-of-range offsets.
- Advertise only the Flowstate service UUID and a compact fingerprint locator.
  Do not put a ticket, endpoint, profile, or complete signed record in the BLE
  advertising packet.
- Replace the current advertisement atomically enough that a profile/session
  refresh does not leave two native managers or stale GATT values alive.
- Stop advertising and remove the service when discovery is paused, the
  document closes, the session ends, or the application terminates.

Apple limits the keys accepted by `startAdvertising` and the space available
to advertisements, so the GATT indirection is required rather than optional:
<https://developer.apple.com/documentation/corebluetooth/cbperipheralmanager/startadvertising%28_%3A%29>.

### Central/scanning side

- Own a `CBCentralManager`, wait for the powered-on state, and scan specifically
  for the Flowstate service UUID.
- Read and check the compact document locator before connecting where the OS
  exposes it.
- Connect with a bounded timeout, discover only the Flowstate service and
  characteristic, and perform offset reads until the declared frame length is
  satisfied.
- Reject oversized, short, trailing, malformed, expired, incorrectly scoped,
  or incorrectly signed records through the shared decoder and eligibility
  gate.
- Cancel connections and scans promptly when the scan window closes. Do not
  retain peripherals solely because they were seen in an earlier scan.
- Define foreground behavior across sleep, wake, Bluetooth power changes, and
  app activation. Background discovery should remain disabled until it has a
  separately reviewed privacy and battery design.

CoreBluetooth's central manager is the native scan/connection surface:
<https://developer.apple.com/documentation/corebluetooth/cbcentralmanager>.

### macOS packaging and privacy

The current `crates/flowstate/assets/macos/Info.plist` registers the
`flowstate://` URL scheme but is not yet a release-complete application plist.

- Add a clear `NSBluetoothAlwaysUsageDescription`. Add the legacy peripheral
  usage key only if the minimum supported macOS version requires it.
- Confirm the hardened-runtime/App Sandbox Bluetooth entitlement requirements
  for the chosen distribution model and add only the necessary entitlement.
- Merge the URL registration and Bluetooth keys into the real bundle generated
  by the release packaging process; the repository plist must not remain an
  unused template.
- Verify incoming `flowstate://join` and `flowstate://oauth/dropbox` URLs both
  reach an already-running app and a cold launch.
- Sign with the release identity, notarize, staple, and test the final artifact
  outside the build machine's development permissions.
- Test both Apple Silicon and Intel if Intel remains supported.

## Windows: WinRT Bluetooth host

Implement a `RendezvousBackend` using
`Windows.Devices.Bluetooth.Advertisement` and
`Windows.Devices.Bluetooth.GenericAttributeProfile`.

### Publisher/GATT-provider side

- Create a `GattServiceProvider` for the Flowstate service and a read-only GATT
  characteristic for the framed signed advertisement.
- Handle read requests and offsets without blocking the WinRT callback thread.
  Complete or deferral-fail every request; never leave a request pending during
  shutdown.
- Advertise the service through the provider. If a separate
  `BluetoothLEAdvertisementPublisher` is needed for the compact locator, keep
  its payload within the platform limit and never place admission material in
  manufacturer/service data.
- Observe provider/publisher status changes and surface radio-off, permission,
  unsupported-adapter, and aborted states distinctly.
- Stop the provider and publisher when discovery pauses or the owning session
  closes. Recreate them after suspend/resume or adapter reset when appropriate.

Microsoft documents the publisher/watcher model and its small payload budget
here: <https://learn.microsoft.com/en-us/windows/apps/develop/devices-sensors/ble-beacon>.
The native GATT server surface is `GattServiceProvider`:
<https://learn.microsoft.com/en-us/uwp/api/windows.devices.bluetooth.genericattributeprofile.gattserviceprovider>.

### Watcher/client side

- Configure a `BluetoothLEAdvertisementWatcher` filter for the Flowstate service
  UUID and compact document locator.
- Deduplicate rotating/device addresses for the duration of one scan without
  treating a Bluetooth address as durable identity.
- Resolve a received address to `BluetoothLEDevice`, discover the Flowstate
  service and characteristic with bounded timeouts, and perform offset reads
  until the complete frame is available.
- Dispose WinRT event registrations and device/service/characteristic objects
  deterministically. Repeated scans must not accumulate callbacks.
- Send the decoded result through the shared eligibility gate. Windows pairing
  or OS device trust is not Flowstate authorization.

### Windows packaging and protocol registration

The existing PowerShell helper registers file associations and the
`flowstate://` protocol for an unpackaged development build. Release packaging
still needs a deliberate choice:

- For MSIX, declare the Bluetooth device capability and protocol/file
  extensions in `Package.appxmanifest`, then test activation routing through
  the packaged app lifecycle.
- For an unpackaged installer, install and uninstall the per-user protocol and
  file associations cleanly, quote the executable and URL argument safely, and
  test upgrade/repair behavior.
- Ensure only one running instance handles a URL activation, including a URL
  received while no window is active.
- Sign the executable and installer/MSIX and test on clean Windows 10 and
  Windows 11 systems without Visual Studio or developer mode.

The packaged Bluetooth capability requirement is documented with the Windows
BLE APIs: <https://learn.microsoft.com/en-us/windows/apps/develop/devices-sensors/ble-beacon>.

## Cross-platform acceptance matrix

Do not enable the nearby-discovery toggle by default on either platform until
all of these pass with release artifacts and physical devices:

- macOS publishes and Windows discovers; Windows publishes and macOS discovers.
- Each platform interoperates with the Linux/BlueZ reference implementation.
- Two Flowstate instances can publish and scan concurrently where the OS permits
  it, with a clear degraded state where the radio does not.
- Records larger than one negotiated ATT payload are reassembled correctly.
- Wrong-document locators are ignored before connection when possible and are
  always rejected after the full record is read.
- Tampered signatures, expired records, oversized frames, invalid offsets, and
  trailing bytes are rejected.
- Permission denied, radio disabled, no adapter, adapter reset, sleep/wake,
  application suspend/resume, and document/session closure do not leak tasks,
  event handlers, services, or advertisements.
- Discovery never displays an unverified or out-of-scope identity and never
  transports a session admission bearer.
- Battery and scan/connect rates remain acceptable during a long editing
  session.
- Accessibility text explains why Bluetooth is requested and allows discovery
  to remain disabled without impairing invite links.

## Other incomplete items from the collaboration plan

Yes. The following items are incomplete even though their lower-level models
now exist.

### Discovery product wiring

- `RendezvousSet`, `DropboxRendezvousBackend`, and the Linux Bluetooth backend
  are not yet constructed and owned by the normal application/session
  lifecycle. Starting a session does not currently publish automatically;
  opening a document does not scan automatically.
- There is no nearby/trusted-peer suggestion UI, accept/dismiss interaction,
  discovery pause control, per-document discovery status, or actionable error
  presentation.
- Advertisement renewal, expiry cleanup, retry/backoff, sleep/wake recovery,
  and shutdown cleanup need an application-level coordinator.
- Dropbox discovery needs a real settings/connect flow that begins PKCE, handles
  the `flowstate://oauth/dropbox` callback, persists refreshed credentials, and
  supports disconnect/revocation. The OAuth and storage primitives exist, but
  no user-facing flow invokes them.
- Dropbox checkpoint compare-and-swap exists at the transport level but is not
  connected to the document save/checkpoint pipeline. Conflict recovery must
  fetch the current revision, merge the CRDT/package state, and retry without
  silently overwriting another client's checkpoint.
- No Dropbox-account or physical-radio end-to-end test has run in this branch.

### Trust and identity UX

- Portable signing identity, signed profiles, safety codes, trust attestations,
  scope rules, and squads exist as models/settings.
- There is no product UI to verify a safety code, add/remove a trusted person,
  inspect key changes, manage squads/scopes/exclusions, revoke standing access,
  or resolve a renamed/reinstalled identity.
- Display name and color feed presence, but profile/avatar editing and profile
  conflict UX are not complete.

### Friendly invite discovery

- Copyable `flowstate://join` links, save/open `.flowinvite` files, readable
  invite summaries, and OS protocol registration are present.
- A hosted web landing page remains optional deployment work; it is not needed
  for the native link/file invitation flow.
- Release packaging still needs to prove custom-URL cold-launch and
  already-running activation on every supported OS.

### Durable collaboration surfaces

- Anchored comments, threads, replies, resolve/reopen, permissions, offline
  behavior, accessibility, export/import, notifications, and comment UI are not
  implemented.
- Revision records can contain author identity underneath, but
  `RuntimeRevisionInfo` and the revision browser still do not expose author
  attribution. Rich activity/provenance queries and UI are also absent.
- The larger participant/comment sidebar or popover remains intentionally
  deferred; the current roster and bottom participant indicator are not a
  replacement for that product surface.

These gaps should be tracked separately from the already working live session,
invite-link, presence, signed identity, trust-policy, Dropbox transport, and
Linux Bluetooth primitives so that release readiness is not inferred from the
existence of backend code alone.
