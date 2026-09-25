# Service Module

The `service` module provides a hardware wallet device discovery and management
service. It polls for connected hardware wallets every 2 seconds and maintains a
shared device map with support for multiple concurrent consumers via
reference-counted start/stop.

## Features

- Automatic device discovery and connection
- Support for multiple concurrent consumers
- Reference-counted service lifecycle management
- Asynchronous device operations via message passing
- BitBox02 pairing configuration support

## Core Types

### `HwiService<Message, Id>`

The main service struct that manages device discovery and maintains the device map.

```rust
use async_hwi::service::{HwiService, SigningDeviceMsg};
use bitcoin::Network;
use crossbeam::channel;

// Define your application message type
#[derive(Clone)]
enum AppMessage {
    Device(SigningDeviceMsg),
    // ... other app messages
}

impl From<SigningDeviceMsg> for AppMessage {
    fn from(msg: SigningDeviceMsg) -> Self {
        AppMessage::Device(msg)
    }
}

// Create the service
let service: HwiService<AppMessage> = HwiService::new(
    Network::Bitcoin,
    None, // Uses internal tokio runtime, or pass Some(handle) to use your own
);
```

### `SigningDevice<Message, Id>`

Represents a detected hardware wallet in one of three states:

- **`Supported`**: Device is ready for use
- **`Locked`**: Device requires unlocking or identification (e.g., PIN entry, pairing confirmation or QR exchange)
- **`Unsupported`**: Device detected but cannot be used (wrong version, wrong
network, etc.)

### `SigningDeviceMsg<Id>`

Messages emitted by the service when device state changes:

```rust
pub enum SigningDeviceMsg<Id = ()> {
    /// Error (None for polling errors, Some(id) for operation errors)
    Error(Option<Id>, String),
    /// Device map changed
    Update,
    /// Extended public key retrieved
    XPub(Id, Fingerprint, DerivationPath, Xpub),
    /// Device version retrieved
    Version(Id, Fingerprint, Version),
    /// Wallet registered with optional HMAC
    WalletRegistered(Id, Fingerprint, String, Option<[u8; 32]>),
    /// Wallet registration check result
    WalletIsRegistered(Id, Fingerprint, String, bool),
    /// Address displayed on device
    AddressDisplayed(Id, Fingerprint, AddressScript),
    /// Transaction signed
    TransactionSigned(Id, Fingerprint, Psbt),
}
```

## Usage

### Basic Setup

```rust
use async_hwi::service::{HwiService, SigningDevice, SigningDeviceMsg};
use bitcoin::Network;
use crossbeam::channel;
use std::sync::Arc;

#[derive(Clone)]
enum AppMessage {
    Device(SigningDeviceMsg),
}

impl From<SigningDeviceMsg> for AppMessage {
    fn from(msg: SigningDeviceMsg) -> Self {
        AppMessage::Device(msg)
    }
}

fn main() {
    // Create a channel for receiving device messages
    let (sender, receiver) = channel::unbounded();

    // Create the service
    let service: Arc<HwiService<AppMessage>> = Arc::new(
        HwiService::new(Network::Bitcoin, None)
    );

    // Start the service (reference counted)
    service.start(sender);

    // Process messages in your application loop
    loop {
        match receiver.recv() {
            Ok(AppMessage::Device(SigningDeviceMsg::Update)) => {
                // Device list changed, refresh UI
                let devices = service.list();
                for (id, device) in devices {
                    match device {
                        SigningDevice::Supported(supported) => {
                            println!("Ready: {} ({:?}) - {}",
                                id,
                                supported.kind(),
                                supported.fingerprint()
                            );
                        }
                        SigningDevice::Locked { id, kind, pairing_code, .. } => {
                            println!("Locked: {} ({:?})", id, kind);
                            if let Some(code) = pairing_code {
                                println!("  Pairing code: {}", code);
                            }
                        }
                        SigningDevice::Unsupported { id, kind, reason, .. } => {
                            println!("Unsupported: {} ({:?}) - {:?}", id, kind,reason);
                        }
                    }
                }
            }
            Ok(AppMessage::Device(SigningDeviceMsg::Error(id, err))) => {
                eprintln!("Error (id={:?}): {}", id, err);
            }
            Ok(AppMessage::Device(msg)) => {
                // Handle other device messages
                println!("Device message: {:?}", msg);
            }
            Err(_) => break,
        }
    }

    // Stop the service when done
    service.stop();
}
```

### Using Device Operations

Operations on `SupportedDevice` are asynchronous and return results via the message
channel:

```rust
use async_hwi::service::{SigningDevice, SigningDeviceMsg, SupportedDevice};
use bitcoin::bip32::DerivationPath;
use std::str::FromStr;

// Get a supported device from the service
let devices = service.list();
for (id, device) in devices {
    if let SigningDevice::Supported(supported) = device {
        // Request an extended public key
        // Results arrive via SigningDeviceMsg::XPub
        let path = DerivationPath::from_str("m/84'/0'/0'").unwrap();
        supported.get_extended_pubkey((), &path);

        // Register a wallet policy
        // Results arrive via SigningDeviceMsg::WalletRegistered
        supported.register_wallet(
            (),
            "My Wallet",
            "wsh(sortedmulti(2,@0/**,@1/**))"
        );

        // Check if wallet is registered
        // Results arrive via SigningDeviceMsg::WalletIsRegistered
        supported.is_wallet_registered(
            (),
            "My Wallet",
            "wsh(sortedmulti(2,@0/**,@1/**))"
        );

        // Display an address on the device
        // Results arrive via SigningDeviceMsg::AddressDisplayed
        use async_hwi::AddressScript;
        let path = DerivationPath::from_str("m/86'/0'/0'/0/0").unwrap();
        supported.display_address((), &AddressScript::P2TR(path));

        // Sign a PSBT
        // Results arrive via SigningDeviceMsg::TransactionSigned
        // supported.sign_tx((), psbt);
    }
}
```

### Using Request IDs

The `Id` type parameter allows tracking which request a response corresponds to:

```rust
use async_hwi::service::{HwiService, SigningDeviceMsg};

#[derive(Clone, Debug)]
struct RequestId(u64);

#[derive(Clone)]
enum AppMessage {
    Device(SigningDeviceMsg<RequestId>),
}

impl From<SigningDeviceMsg<RequestId>> for AppMessage {
    fn from(msg: SigningDeviceMsg<RequestId>) -> Self {
        AppMessage::Device(msg)
    }
}

// Create service with custom ID type
let service: HwiService<AppMessage, RequestId> = HwiService::new(Network::Bitcoin,
None);

// Later, when making requests:
// supported.get_extended_pubkey(RequestId(42), &path);

// When handling responses:
// SigningDeviceMsg::XPub(RequestId(42), fingerprint, path, xpub)
```

### BitBox02 Pairing Configuration

For BitBox02 devices, you can provide a noise configuration to persist pairing:

```rust
use async_hwi::bitbox::{NoiseConfig, NoiseConfigData, ConfigError};
use std::sync::Arc;

struct MyNoiseConfig {
    // Your storage implementation
}

impl bitbox_api::Threading for MyNoiseConfig {}

impl NoiseConfig for MyNoiseConfig {
    fn read_config(&self) -> Result<NoiseConfigData, ConfigError> {
        // Read from your storage
        todo!()
    }

    fn store_config(&self, data: &NoiseConfigData) -> Result<(), ConfigError> {
        // Write to your storage
        todo!()
    }
}

// Set the configuration before starting the service
let noise_config: Arc<dyn NoiseConfig> = Arc::new(MyNoiseConfig { /* ... */ });
service.set_bitbox_noise_config(noise_config);

// Start the service
service.start(sender);

// Later, if needed:
// service.clear_bitbox_noise_config();
```

### Multiple Consumers

The service supports multiple concurrent consumers with reference counting:

```rust
// First consumer starts the service
service.start(sender1.clone());

// Second consumer increments ref count (service already running)
service.start(sender2.clone());

// First consumer done - decrements ref count (service keeps running)
service.stop();

// Second consumer done - decrements ref count to 0, service stops
service.stop();
```

## Device States

### Supported Devices

A `SupportedDevice` provides access to:
- `device()` - The underlying `HWI` trait object
- `version()` - Device firmware version
- `fingerprint()` - Master key fingerprint
- `kind()` - Device type (Ledger, BitBox02, etc.)

### Locked Devices

Devices in the `Locked` state require user interaction:
- **BitBox02**: Requires pairing confirmation on device (displays pairing code)
- **Jade**: Requires PIN entry and blind oracle authentication
- **Thunder Den**: Requires one QR identification exchange with the offline device.
  See [QR sessions](#thunder-den-qr-sessions) below.

The service automatically attempts to unlock devices. Monitor
`SigningDeviceMsg::Update` for state transitions.

### Unsupported Devices

Devices may be unsupported for various reasons:

```rust
pub enum UnsupportedReason {
    /// Firmware version too old
    Version { minimal_supported_version: &'static str },
    /// Method not supported by device
    Method(&'static str),
    /// Device not part of wallet (fingerprint mismatch)
    NotPartOfWallet(Fingerprint),
    /// Device configured for different network
    WrongNetwork,
    /// Ledger: Bitcoin app not open
    AppIsNotOpen,
}
```

## Taproot Miniscript Compatibility

Check if a device supports Taproot Miniscript:

```rust
use async_hwi::service::is_compatible_with_tapminiscript;
use async_hwi::DeviceKind;

let compatible = is_compatible_with_tapminiscript(
    &DeviceKind::Ledger,
    Some(&version)
);
```

Minimum versions for Taproot Miniscript support:
- Ledger: v2.2.0
- Coldcard: v6.3.3
- BitBox02: v9.21.0
- Specter: All versions
- Thunder Den: v0.0.1

## Thunder Den QR Sessions

Thunder Den is an offline signer. This library communicates with the offline
device through the [Thunder Den QR bridge](https://github.com/bitcoinerlab/thunderden-qr-bridge),
a small HTTP server running on the same online computer as async-hwi. The user
opens the bridge's web page in a browser on that computer. The page displays
request QR codes on the screen and uses the computer's camera to scan reply QR
codes from the offline device running Thunder Den.

This lets the user run async-hwi and handle the QR exchange on one online
computer. The browser page handles camera capture and QR display, keeping that
work outside async-hwi so the adapter can focus on the protocol messages. The
user reviews and approves operations on the offline device, which also does the
signing.

When the `thunderden` feature is enabled, the service checks for the bridge as
part of its normal device discovery. It uses
`http://127.0.0.1:32123/exchange` by default. Set the `THUNDERDEN_BRIDGE_URL`
environment variable to use another bridge address.

The bridge creates a new session ID each time it starts. The adapter reads this
ID from the `X-Thunderden-Session` HTTP response header and sends it with later
requests. If the bridge server restarts, it creates a different session ID and
rejects requests that still use the old ID. The session ID is public and is
exchanged automatically.

When the service finds a bridge session that is not already in its list, it
creates a Thunder Den adapter and marks it as `Locked`
(`LockedDevice::ThunderDen`). This means the bridge is running, but the service
still needs to identify the offline device. It sends one `GET_INFO` request,
which the bridge displays as a QR code. The user scans it with the offline
device running Thunder Den, then uses the bridge's camera to scan the reply.
The reply contains the master fingerprint and application version. Once the
adapter accepts it, the service keeps that adapter and marks it as `Supported`.

If the offline device uses the wrong network, the service marks it as
`Unsupported` with the reason `UnsupportedReason::WrongNetwork`. If the user
cancels the identification request, the device stays `Locked`. The service does
not ask the user to scan again on every check. The user can restart the bridge
when they are ready to try again.

While the bridge stays available with the same session ID, the service keeps
the adapter and its cached fingerprint and version. Regular checks contact only
the HTTP server and need no QR scans. The `Supported` state means the signer was
identified earlier. Signing and address confirmation each need a new QR
exchange.

If the service loses contact with the bridge or finds a different session ID,
it removes the old device entry and cancels any identification request still
waiting for a reply. A late reply cannot add the old entry back. When the
service finds the bridge again, it starts identification again. Restarting the
bridge creates a new session ID even if the offline device still uses the same
key.

The bridge remembers the fingerprint from its first successful reply. A later
reply may have a different fingerprint if the user loads another seed or
passphrase. In that case, the bridge rejects the exchange and ends the session.
The adapter returns `DeviceDisconnected`, and the service removes the device
entry on its next check. The user must restart the bridge before identifying
the new signer. This prevents an existing adapter from silently switching keys.
