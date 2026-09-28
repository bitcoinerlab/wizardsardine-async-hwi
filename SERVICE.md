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
- **Thunder Den**: Exchange QR codes with the device running Thunder Den so the
  wallet app can read its master fingerprint and application version.
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

Thunder Den runs on a separate laptop and exchanges QR codes with your wallet
app. Run the [QR bridge](https://github.com/bitcoinerlab/thunderden-qr-bridge) on
the same computer as your wallet app. Its browser page shows requests and scans
replies. You review and approve operations on the device running Thunder Den.

### Connect Thunder Den

1. Start Thunder Den and choose the same Bitcoin network as your wallet app.
2. On the computer with your wallet app, start the bridge with Node.js 22 or later:

   ```sh
   npx @bitcoinerlab/thunderden-qr-bridge
   ```

3. The bridge opens its page in your browser automatically. If it does not,
   open the address printed in the terminal. Start the service, or open the
   device list in a wallet app that uses it. The service finds the bridge
   automatically and asks for the signer's master fingerprint and version.
   Follow the instructions on the bridge page to complete the QR exchange.

While waiting for the first reply from Thunder Den, the service lists it as
`Locked`. This means the initial QR exchange is still in progress. Once it
receives the fingerprint and version, it changes the entry to `Supported`.

The service remembers the fingerprint and version for that connection. Its
regular checks do not require another QR exchange. Signing a transaction or
checking an address each needs a new QR exchange.

### Bridge Address

The default address is `http://127.0.0.1:32123/exchange`. To use another port,
start the bridge with `--port PORT` and set `THUNDERDEN_BRIDGE_URL` to the
matching address before starting your wallet app. The bridge must run on the
same computer as the app; the transport accepts HTTP addresses on `127.0.0.1`.

### Applications with Their Own Discovery Loop

Call `ThunderDen::try_connect(network)` to check that the bridge is running and
create an adapter. This call does not display a QR code. The first call to
`get_master_fingerprint()` or `get_version()` starts the initial QR exchange and
waits for the reply from Thunder Den.

One reply supplies both the fingerprint and version. The adapter remembers them,
so asking it for either value again does not need another scan. Use `device.id()`
to recognise a connection you already have and keep its existing adapter. A new
adapter does not inherit the answers saved by an older one.

### Cancellation and Reconnection

- If you cancel the first exchange, the service leaves Thunder Den as `Locked`
  and does not repeatedly ask you to scan. Restart the bridge when ready to retry.
- If Thunder Den uses a different Bitcoin network, the service marks it as
  `Unsupported` with `UnsupportedReason::WrongNetwork`.
- If the bridge stops or restarts, the service removes its old device entry and
  cancels any pending initial QR exchange. It starts a fresh exchange when the
  bridge is available again. A late reply cannot restore the old entry.
- Restart the bridge before loading different recovery words or a different
  passphrase. If a reply has a different fingerprint from the first successful
  reply, the bridge ends the connection. The adapter returns
  `DeviceDisconnected`, and the service removes the entry on its next check.

The bridge creates a new session ID each time it starts. async-hwi reads it from
the `X-Thunderden-Session` header and includes it with later requests. This lets
the bridge reject requests from an old connection, even if the same keys are
still loaded in Thunder Den. The ID is exchanged automatically; users do not
need to enter or copy it.
