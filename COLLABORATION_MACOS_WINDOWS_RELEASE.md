# Collaboration release handoff: macOS and Windows

**Status date: 2026-07-13.** This document supersedes the July-2026 draft that
previously lived at this path. That draft's "other incomplete items" list is
obsolete: the discovery coordinator, Dropbox connect flow, checkpoint
compare-and-swap, anchored comments, trust/identity settings UI, and revision
author attribution are now all implemented, wired into the application
lifecycle, and verified on Linux (unit + integration tests, clippy-clean).

What remains is exactly two categories:

- **Yellow zone** — code that is written and cross-compile-checked for
  macOS/Windows but has never executed on those platforms. It needs a dev with
  the hardware to run, verify, and fix what only a live radio/OS can reveal.
- **Red zone** — packaging, signing, entitlements, and the physical-device
  acceptance matrix. Not started here by design; owned entirely by the
  macOS/Windows devs.

## Architecture you are inheriting (all verified on Linux)

- `crates/flowstate-collab/src/discovery.rs` — signed `DiscoveryAdvertisement`
  (Ed25519, domain-separated contexts, expiry, document-fingerprint scope),
  `RendezvousBackend` trait, and `eligible_advertisements` — the single
  verification/dedup gate every backend result must pass before UI.
- `crates/flowstate-collab/src/bluetooth.rs` — shared GATT frame codec
  (u32-LE length prefix, 60 KiB cap) plus three native backends: Linux/BlueZ
  (the behavioral reference), Windows/WinRT, macOS/CoreBluetooth.
- `crates/flowstate-collab/src/dropbox.rs` — PKCE OAuth, rendezvous sidecars,
  revision-conditional checkpoint writes (CAS). HTTP clients have bounded
  connect/read timeouts and a 256 MiB download cap.
- `crates/flowstate/src/collab/discovery_runtime.rs` — the application-level
  discovery coordinator (publish on session start, 30 s renewal, 90 s ad
  lifetime, scan-on-request, clear on close/shutdown).
- Comments, identity/trust settings, safety codes, and revision attribution
  are application-level and platform-neutral; nothing there needs macOS- or
  Windows-specific work beyond running the app.

Security posture (for your review, not rework): discovery ads never carry
admission bearers; admission is granted server-side only to identities in the
document's standing-access set, over the encrypted iroh channel
(`net/direct.rs`); comment delete/edit are author-gated in the CRDT runtime;
the Dropbox checkpoint merge trusts the Dropbox folder ACL as its boundary
(documented at `dropbox_checkpoint.rs`); `settings.toml` (identity seed +
Dropbox tokens) is written `0600` on Unix — see the Windows note below.

## Yellow zone: run and verify the native BLE backends

Both backends compile cleanly today (`aarch64-apple-darwin` and
`x86_64-pc-windows-msvc`). On a real machine, plain `cargo check`/`cargo test`
works natively — no probe rig needed. Neither backend has ever executed.

### macOS (`bluetooth.rs`, `macos_backend`) — rebuilt 2026-07-13, highest risk

The module as originally drafted had never compiled (the
`objc2-core-bluetooth/dispatch2` feature was never enabled). It was rebuilt:

- **Peripheral (publish) side runs on a dedicated actor thread**
  (`flowstate-ble-peripheral`): CoreBluetooth objects are `!Send`, so the
  backend only passes plain-data commands over a channel. All manager/service
  objects live and die on that thread.
- **Both managers get dedicated serial dispatch queues** — callbacks must not
  depend on the GPUI-owned main run loop (the scan side blocks its own thread).
- **`wait_for_power` distinguishes states**: fails fast on
  PoweredOff/Unauthorized/Unsupported, waits up to 15 s through
  Unknown/Resetting to cover the first-run permission prompt.

Verify on hardware, in roughly this order:

1. **Permission**: without `NSBluetoothAlwaysUsageDescription` in the bundle's
   Info.plist, modern macOS kills the process on first CoreBluetooth use. This
   is the first thing to wire (red zone below) before any run.
2. **Publish**: manager reaches PoweredOn on its queue; `addService` triggers
   no `didAdd…error`; the ad is visible to a BLE sniffer / a Linux peer with
   the Flowstate service UUID.
3. **Static characteristic long reads**: the signed record is set as the
   characteristic's static value; CoreBluetooth serves offset reads itself.
   Verify a Linux peer reads the full frame (records are typically 400–500
   bytes; ATT caps attribute values at 512 bytes — see the matrix item below).
4. **Scan**: 4 s scan window connects, discovers, reads, disconnects; the
   delegate feeds decoded records through `eligible_advertisements`.
5. **Teardown**: Clear command and backend drop stop advertising, remove
   services, and nil the delegate; repeated publish replaces cleanly (one
   peripheral-manager slot).
6. **Sleep/wake and Bluetooth toggling** while a session is live.

**Known macOS platform constraint (already handled, verify the behavior):**
CoreBluetooth cannot publish service data — macOS ads carry only the service
UUID, not the 16-byte document locator. The Linux scanner was changed
(2026-07-13) to connect-and-verify peers with a missing locator instead of
skipping them; the Windows watcher never used a locator prefilter. Verify a
macOS publisher is actually discovered by both. The post-read signed-record
check is the authoritative document scope test on every platform.

### Windows (`bluetooth.rs`, `windows_backend`) — written by the prior agent, API-verified only

The code compiles and its WinRT usage was reviewed; nothing has run. Verify:

1. **Provider**: `GattServiceProvider::CreateAsync` + characteristic creation
   succeed on a real adapter; `StartAdvertisingWithParameters` including the
   `SetServiceData` locator (best-effort on older builds — a failure there is
   deliberately ignored).
2. **Long reads**: `ReadValueWithCacheModeAsync(Uncached)` performs ATT Read
   Blob continuation internally; a truncated frame fails loud in the shared
   decoder. Verify against a Linux publisher with a large profile.
3. **Watcher hygiene**: repeated scans must not accumulate callbacks
   (`Stop` + `RemoveReceived` run per scan; confirm with a scan loop).
4. **Apartment/threading**: WinRT activation happens from tokio worker
   threads; `NativeBluetoothBackend::new` probes watcher creation so settings
   gets a synchronous, actionable platform error. Verify with radio off, no
   adapter, and after adapter reset.
5. **Packaged vs unpackaged**: BLE works unpackaged for development; under
   MSIX the `bluetooth` device capability is required (red zone).

### Dropbox end-to-end (all platforms; code done, no live-account run)

The PKCE begin → system browser → `flowstate://oauth/dropbox` callback →
token exchange → persisted credentials flow, plus disconnect and the
checkpoint CAS conflict-merge path, are implemented and unit-tested. Nobody
has run them against a live Dropbox app key. On each OS verify: custom-scheme
routing to a running instance AND to a cold launch; token refresh persistence;
two clients CAS-conflicting on one bound path and converging.

## Red zone: packaging, signing, and the acceptance matrix

### macOS packaging and privacy

`crates/flowstate/assets/macos/Info.plist` registers `flowstate://` but is not
a release-complete plist.

- Add `NSBluetoothAlwaysUsageDescription` (legacy peripheral key only if the
  minimum macOS requires it). Without it, CoreBluetooth use is fatal.
- Confirm hardened-runtime/App Sandbox Bluetooth entitlements for the chosen
  distribution model; add only what is necessary.
- Merge URL registration + Bluetooth keys into the real release bundle; the
  repository plist must not remain an unused template.
- Verify `flowstate://join` and `flowstate://oauth/dropbox` reach both a
  running app and a cold launch.
- Sign, notarize, staple; test the artifact outside the build machine.
  Apple Silicon and Intel if Intel remains supported.

### Windows packaging and protocol registration

The PowerShell helper registers associations for unpackaged dev builds only.

- MSIX: declare the `bluetooth` device capability plus protocol/file
  extensions in `Package.appxmanifest`; test activation routing through the
  packaged lifecycle.
- Unpackaged installer: clean install/uninstall of per-user protocol + file
  associations, safe quoting of the executable and URL argument,
  upgrade/repair behavior.
- Single-instance URL activation, including with no window open.
- Sign executable and installer/MSIX; test on clean Windows 10 and 11 without
  Visual Studio or developer mode.
- **Secrets at rest**: the Unix `0600` on `settings.toml` does not apply on
  Windows. Decide on DPAPI (and Keychain on macOS) for the identity signing
  seed and Dropbox tokens, or accept per-user-profile ACLs explicitly.

### Physical-device acceptance matrix

Do not enable the nearby-discovery toggle by default on either platform until
all of these pass with release artifacts and physical devices:

- macOS publishes and Windows discovers; Windows publishes and macOS
  discovers; each platform interoperates with the Linux/BlueZ reference.
- A macOS publisher (no service-data locator) is discovered by Linux and
  Windows scanners; wrong-document locators are skipped pre-connect where a
  locator exists, and every record is re-checked post-read.
- Records larger than one ATT payload are reassembled; records approaching
  the 512-byte ATT attribute cap are measured. If a real profile (long
  display name, many relay addresses) pushes a frame past 512 bytes, GATT
  reads will truncate at the OS level on some stacks — the decoder fails
  loudly, but delivery needs a design change (trim the endpoint address list,
  or move to an L2CAP channel). Measure before shipping.
- Tampered signatures, expired records, oversized frames, invalid offsets,
  and trailing bytes are rejected (the shared decoder tests cover the codec;
  verify the OS paths deliver bytes unmodified).
- Two Flowstate instances publishing/scanning concurrently where the OS
  permits, with a clear degraded state otherwise.
- Permission denied, radio disabled, no adapter, adapter reset, sleep/wake,
  suspend/resume, and document/session closure leak no tasks, handlers,
  services, or advertisements.
- Discovery never displays an unverified or out-of-scope identity and never
  transports a session admission bearer.
- Battery and scan/connect rates acceptable during a long editing session
  (current cadence: one BLE publish round-robin + Dropbox refresh per 30 s
  tick, 4 s scan windows on demand).
- Accessibility text explains why Bluetooth is requested; discovery can stay
  disabled without impairing invite links.

### Deliberate design decisions (do not re-litigate without cause)

- Dropbox folder membership **is** the checkpoint trust boundary; checkpoint
  packages are not additionally signed.
- Discovery admission requests carry a nonce but are not replay-tracked:
  a 30 s expiry, the encrypted direct channel, and the standing-access
  identity gate bound the exposure.
- Trust attestations have no expiry; revocation is removing the trusted key
  in settings. Key-change alerting is future UX, not release-blocking.
- Background (app-closed) BLE discovery stays out of scope until it has a
  separately reviewed privacy/battery design.
