# async-hwi

Current **Minimum Supported Rust Version**: v1.81 (1.85 if coldcard feature enabled)

```rust
/// HWI is the common Hardware Wallet Interface.
#[async_trait]
pub trait HWI: Debug {
    /// 0. Return the device kind
    fn device_kind(&self) -> DeviceKind;
    /// 1. Application version or OS version.
    async fn get_version(&self) -> Result<Version, Error>;
    /// 2. Get master fingerprint.
    async fn get_master_fingerprint(&self) -> Result<Fingerprint, Error>;
    /// 3. Get the xpub with the given derivation path.
    async fn get_extended_pubkey(&self, path: &DerivationPath) -> Result<Xpub, Error>;
    /// 4. Register a new wallet policy
    async fn register_wallet(&self, name: &str, policy: &str) -> Result<Option<[u8; 32]>, Error>;
    /// 5. Returns true if the wallet is registered
    async fn is_wallet_registered(&self, name: &str, policy: &str) -> Result<bool, HWIError>;
    /// 6. Display an address on the device screen
    async fn display_address(&self, script: &AddressScript) -> Result<(), Error>;
    /// 7. Sign a partially signed bitcoin transaction (PSBT).
    async fn sign_tx(&self, tx: &mut Psbt) -> Result<(), Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressScript {
    /// Must be a bip86 path.
    P2TR(DerivationPath),
    /// Miniscript requires the policy be loaded into the device.
    Miniscript { index: u32, change: bool },
}
```

## Supported devices

A Empty case means the method is unimplemented on the client or device side.

| device               | 1          | 2          | 3          | 4          | 5                    | 6          | 7          |
| -------------------- | ---------- | ---------- | ---------- | ---------- | -------------------- | ---------- | ---------- |
| BitBox02[^1]         | >= v9.15.0 | >= v9.15.0 | >= v9.15.0 | >= v9.15.0 | >= v9.15.0           | >= v9.15.0 | >= v9.15.0 |
| Coldcard[^2]         | >= v6.2.1X | >= v6.2.1X | >= v6.2.1X | >= v6.2.1X | >= v6.2.1X           | >= v6.2.1X | >= v6.2.1X |
| Jade[^3]             | >= v1.0.30 | >= v1.0.30 | >= v1.0.30 | >= v1.0.30 | >= v1.0.30           | >= v1.0.30 | >= v1.0.30 |
| Ledger Nano S/S+[^4] | >= v2.1.2  | >= v2.1.2  | >= v2.1.2  | >= v2.1.2  | *check hmac presence | >= v2.1.2  | >= v2.1.2  |
| Specter[^5]          |            | >= v1.8.0  | >= v1.8.0  | >= v1.8.0  |                      |            | >= v1.8.0  |
| Thunder Den[^6]      |            | yes        | yes        | yes        | *check proof presence | yes        | yes        |

[^1]: https://github.com/digitalbitbox/bitbox02-firmware
[^2]: https://github.com/alfred-hodler/rust-coldcard
[^3]: https://github.com/Blockstream/Jade
[^4]: https://github.com/LedgerHQ/app-bitcoin-new
[^5]: https://github.com/cryptoadvance/specter-diy
[^6]: https://github.com/bitcoinerlab/thunderden

## Thunder Den

The `thunderden` feature is enabled by default. Requests go through the local
[QR bridge](https://github.com/bitcoinerlab/thunderden-qr-bridge); the user scans
and approves on the signer. Start the bridge, then use the CLI:

```sh
cargo run -p async-hwi-cli -- --network regtest xpub get --path "m/48h/1h/0h/2h"
```

The CLI selects a running bridge before USB discovery. `device list` reports bridge
availability without scanning the offline signer. `THUNDERDEN_BRIDGE_URL` overrides
`http://127.0.0.1:32123/exchange`; only HTTP on `127.0.0.1` is accepted. This local
API trusts programs on the computer and requires no credentials. Library callers
use `HttpTransport::connect(url).await`, construct `ThunderDen` with that transport
and network, then use `with_wallet` with a validated public
descriptor and the proof returned by `register_wallet`. As with Ledger,
`is_wallet_registered` checks the supplied policy/proof locally. Descriptor
checksum validation and address derivation belong to the calling wallet.

Development build labels may return `UnsupportedVersion` from `get_version`.
The optional signer integration test is in `tests/thunderden_native.rs`.

## Service Module

The `service` module provides automatic device discovery and management with support
for multiple concurrent consumers. See [SERVICE.md](SERVICE.md) for detailed
documentation and usage examples.

## Contributing

If you use AI tools while contributing, read and follow the
[AI policy](AI_POLICY.md). Contributors are responsible for understanding and
explaining their own work in their own words.
