//! Thunder Den's user-operated QR interface, via a local companion.
//!
//! Policies must be validated public descriptors supplied by the caller.
mod http;
#[cfg(test)]
mod tests;

pub use http::HttpTransport;

use std::{
    convert::{TryFrom, TryInto},
    fmt,
    str::FromStr,
};

use async_trait::async_trait;
use bitcoin::{
    bip32::{DerivationPath, Fingerprint, Xpub},
    consensus::encode::{serialize, VarInt},
    hashes::{sha256, Hash},
    psbt::Psbt,
    Address, Network,
};
use serde_cbor::Value;
use tokio::sync::Mutex;

use crate::{utils, AddressScript, DeviceKind, Error, Version, HWI};

const MAX_REQUEST: usize = 1024 * 1024 + 65536;
const MAX_REPLY: usize = 2 * 1024 * 1024 + 65536;

#[async_trait]
pub trait Transport {
    async fn exchange(&self, request: &[u8]) -> Result<Vec<u8>, Error>;
}

struct State {
    counter: u128,
    info: Option<(Fingerprint, String)>,
}

pub struct ThunderDen<T> {
    transport: T,
    network: Network,
    state: Mutex<State>,
    wallet: Option<(Wallet, Option<[u8; 32]>)>,
}

impl<T> fmt::Debug for ThunderDen<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThunderDen")
            .field("network", &self.network)
            .finish()
    }
}

impl<T: Transport + Send + Sync> ThunderDen<T> {
    pub fn new(transport: T, network: Network) -> Result<Self, Error> {
        let mut nonce = [0; 16];
        getrandom::getrandom(&mut nonce)
            .map_err(|_| Error::Device("OS random source failed".into()))?;
        Ok(Self {
            transport,
            network,
            state: Mutex::new(State {
                counter: u128::from_be_bytes(nonce),
                info: None,
            }),
            wallet: None,
        })
    }

    pub fn with_wallet(
        mut self,
        name: impl Into<String>,
        policy: &str,
        hmac: Option<[u8; 32]>,
    ) -> Result<Self, Error> {
        self.wallet = Some((Wallet::new(name.into(), policy)?, hmac));
        Ok(self)
    }

    async fn request(&self, operation: u8, arguments: Vec<Value>) -> Result<Value, Error> {
        // One exchange per client. Holding the async mutex also serializes callers
        // through the HWI trait's shared-reference methods.
        let mut state = self.state.lock().await;
        state.counter = state
            .counter
            .checked_add(1)
            .ok_or(Error::Unexpected("Request counter exhausted"))?;
        let id = state.counter.to_be_bytes();
        let request = serde_cbor::to_vec(&Value::Array(vec![
            uint(3),
            bytes(&id),
            Value::Text(self.network.to_core_arg().into()),
            uint(operation.into()),
            Value::Array(arguments),
        ]))
        .map_err(|_| Error::Unexpected("Cannot encode Thunder Den request"))?;
        if request.len() > MAX_REQUEST {
            return Err(Error::UnsupportedInput);
        }
        let raw = self.transport.exchange(&request).await?;
        if raw.len() > MAX_REPLY {
            return Err(Error::Unexpected("Thunder Den reply too large"));
        }
        let value: Value = serde_cbor::from_slice(&raw)
            .map_err(|_| Error::Unexpected("Invalid Thunder Den CBOR"))?;
        if serde_cbor::to_vec(&value).map_err(|_| Error::Unexpected("Invalid Thunder Den CBOR"))?
            != raw
        {
            return Err(Error::Unexpected("Non-canonical Thunder Den reply"));
        }
        let reply = array(&value, 8)?;
        if number(&reply[0])? != 3
            || data(&reply[1], 16)? != id
            || number(&reply[5])? != u64::from(operation)
        {
            return Err(Error::Unexpected("Stale or mismatched Thunder Den reply"));
        }
        if text(&reply[2])? != self.network.to_core_arg() {
            return Err(Error::NetworkMismatch);
        }
        let fingerprint = Fingerprint::from(
            <[u8; 4]>::try_from(data(&reply[3], 4)?)
                .map_err(|_| Error::Unexpected("Invalid fingerprint"))?,
        );
        if state
            .info
            .as_ref()
            .is_some_and(|info| info.0 != fingerprint)
        {
            return Err(Error::Unexpected("Thunder Den fingerprint changed"));
        }
        let version = text(&reply[4])?;
        match number(&reply[6])? {
            0 => {}
            status => {
                array(&reply[7], 0)?;
                return Err(match status {
                    1 => Error::UserRefused,
                    2 => Error::Device(
                        "Thunder Den rejected the request, policy, proof or transaction".into(),
                    ),
                    4 => Error::NetworkMismatch,
                    5 => Error::UnimplementedMethod,
                    _ => Error::Unexpected("Unknown Thunder Den status"),
                });
            }
        }
        let count = [0, 2, 2, 1, 3]
            .get(usize::from(operation))
            .copied()
            .ok_or(Error::UnimplementedMethod)?;
        array(&reply[7], count)?;
        state.info = Some((fingerprint, version.to_string()));
        Ok(reply[7].clone())
    }

    async fn info(&self) -> Result<(Fingerprint, String), Error> {
        let cached = self.state.lock().await.info.clone();
        if let Some(info) = cached {
            return Ok(info);
        }
        self.request(0, vec![]).await?;
        self.state
            .lock()
            .await
            .info
            .clone()
            .ok_or(Error::Unexpected("Missing Thunder Den information"))
    }

    async fn show_address(
        &self,
        wallet: &Wallet,
        proof: &[u8; 32],
        change: bool,
        index: u32,
    ) -> Result<(), Error> {
        if index >= (1 << 31) {
            return Err(Error::UnsupportedInput);
        }
        let result = self
            .request(
                3,
                vec![
                    wallet.value(),
                    bytes(proof),
                    uint(change.into()),
                    uint(index.into()),
                ],
            )
            .await?;
        Address::from_str(text(&array(&result, 1)?[0])?)
            .map_err(|_| Error::Unexpected("Invalid Thunder Den address"))?
            .require_network(self.network)
            .map_err(|_| Error::NetworkMismatch)?;
        Ok(())
    }
}

impl<T: 'static + Transport + Send + Sync> From<ThunderDen<T>> for Box<dyn HWI + Send> {
    fn from(device: ThunderDen<T>) -> Self {
        Box::new(device)
    }
}

#[async_trait]
impl<T: Transport + Send + Sync> HWI for ThunderDen<T> {
    fn device_kind(&self) -> DeviceKind {
        DeviceKind::ThunderDen
    }

    async fn get_version(&self) -> Result<Version, Error> {
        // Unreleased build labels are not fabricated into a semantic version.
        crate::parse_version(&self.info().await?.1)
    }

    async fn get_master_fingerprint(&self) -> Result<Fingerprint, Error> {
        Ok(self.info().await?.0)
    }

    async fn get_extended_pubkey(&self, path: &DerivationPath) -> Result<Xpub, Error> {
        if path.len() > 32 {
            return Err(Error::UnsupportedInput);
        }
        let steps: Vec<Value> = path
            .into_iter()
            .map(|step| uint(u32::from(*step).into()))
            .collect();
        let result = self.request(1, vec![Value::Array(steps), uint(1)]).await?;
        let result = array(&result, 2)?;
        for (actual, expected) in array(&result[0], path.len())?.iter().zip(path) {
            if number(actual)? != u64::from(u32::from(*expected)) {
                return Err(Error::Unexpected("Thunder Den xpub path mismatch"));
            }
        }
        let xpub = Xpub::from_str(text(&result[1])?)
            .map_err(|_| Error::Unexpected("Invalid Thunder Den xpub"))?;
        if xpub.network != self.network.into() {
            return Err(Error::NetworkMismatch);
        }
        if usize::from(xpub.depth) != path.len()
            || path
                .into_iter()
                .last()
                .is_some_and(|step| *step != xpub.child_number)
        {
            return Err(Error::Unexpected("Thunder Den xpub origin mismatch"));
        }
        Ok(xpub)
    }

    async fn register_wallet(&self, name: &str, policy: &str) -> Result<Option<[u8; 32]>, Error> {
        let wallet = Wallet::new(name.into(), policy)?;
        if name.is_empty() {
            return Err(Error::UnsupportedInput);
        }
        let result = self.request(2, vec![wallet.value()]).await?;
        let result = array(&result, 2)?;
        if data(&result[0], 32)? != wallet.id() {
            return Err(Error::Unexpected("Thunder Den wallet ID mismatch"));
        }
        Ok(Some(data(&result[1], 32)?.try_into().map_err(|_| {
            Error::Unexpected("Invalid registration proof")
        })?))
    }

    async fn is_wallet_registered(&self, name: &str, policy: &str) -> Result<bool, Error> {
        match &self.wallet {
            Some((wallet, Some(_))) => Ok(wallet.id() == Wallet::new(name.into(), policy)?.id()),
            _ => Ok(false),
        }
    }

    async fn display_address(&self, script: &AddressScript) -> Result<(), Error> {
        match script {
            AddressScript::Miniscript { index, change } => {
                let (wallet, proof) = self.wallet.as_ref().ok_or(Error::MissingPolicy)?;
                self.show_address(wallet, &wallet.proof(*proof)?, *change, *index)
                    .await
            }
            AddressScript::P2TR(path) => {
                let steps = utils::bip86_path_child_numbers(path.clone())?;
                if steps[3] != crate::RECV_INDEX && steps[3] != crate::CHANGE_INDEX {
                    return Err(Error::Bip86ChangeIndex);
                }
                let account = DerivationPath::from(&steps[..3]);
                let xpub = self.get_extended_pubkey(&account).await?;
                let fingerprint = self.get_master_fingerprint().await?;
                let wallet = Wallet::new(
                    String::new(),
                    &format!("tr([{}/{}]{}/**)", fingerprint, account, xpub),
                )?;
                self.show_address(
                    &wallet,
                    &[0; 32],
                    steps[3] == crate::CHANGE_INDEX,
                    steps[4].into(),
                )
                .await
            }
        }
    }

    async fn sign_tx(&self, psbt: &mut Psbt) -> Result<(), Error> {
        let (wallet, proof) = self.wallet.as_ref().ok_or(Error::MissingPolicy)?;
        let raw = psbt.serialize();
        if psbt.version != 0 || raw.len() > 1024 * 1024 {
            return Err(Error::UnsupportedInput);
        }
        let result = self
            .request(
                4,
                vec![wallet.value(), bytes(&wallet.proof(*proof)?), bytes(&raw)],
            )
            .await?;
        let result = array(&result, 3)?;
        let raw = match &result[0] {
            Value::Bytes(raw) if raw.len() <= 2 * 1024 * 1024 => raw,
            _ => return Err(Error::Unexpected("Invalid signed PSBT length")),
        };
        let signed =
            Psbt::deserialize(raw).map_err(|_| Error::Unexpected("Invalid signed PSBT"))?;
        if signed.version != 0 || signed.unsigned_tx != psbt.unsigned_tx {
            return Err(Error::Unexpected("Unsigned transaction changed"));
        }
        let added = number(&result[1])?;
        if added == 0 {
            return Err(Error::DeviceDidNotSign);
        }
        if number(&result[2])? > 1
            || signatures(&signed).checked_sub(signatures(psbt)) != Some(added)
        {
            return Err(Error::Unexpected("Invalid signing progress"));
        }
        // Both merge orders must agree: existing metadata and signatures cannot
        // be replaced by conflicting values. Commit to the caller only on success.
        let mut merged = psbt.clone();
        let mut reverse = signed.clone();
        merged
            .combine(signed)
            .map_err(|_| Error::Unexpected("PSBT merge failed"))?;
        reverse
            .combine(psbt.clone())
            .map_err(|_| Error::Unexpected("PSBT merge failed"))?;
        if merged != reverse {
            return Err(Error::Unexpected("Conflicting PSBT metadata"));
        }
        *psbt = merged;
        Ok(())
    }
}

struct Wallet {
    name: String,
    template: String,
    keys: Vec<String>,
}

impl Wallet {
    fn new(name: String, policy: &str) -> Result<Self, Error> {
        if name.len() > 64
            || name.trim() != name
            || policy.len() > 65536
            || !name
                .bytes()
                .chain(policy.bytes())
                .all(|b| (32..=126).contains(&b))
        {
            return Err(Error::UnsupportedInput);
        }
        let (template, keys) = utils::extract_keys_and_template::<String>(policy)?;
        if template.len() > 8192
            || keys.is_empty()
            || keys.len() > 32
            || keys.iter().any(|k| k.len() > 512)
        {
            return Err(Error::UnsupportedInput);
        }
        Ok(Self {
            name,
            template,
            keys,
        })
    }

    fn value(&self) -> Value {
        Value::Array(vec![
            Value::Text(self.name.clone()),
            Value::Text(self.template.clone()),
            Value::Array(self.keys.iter().cloned().map(Value::Text).collect()),
        ])
    }

    fn id(&self) -> [u8; 32] {
        fn hash(bytes: &[u8]) -> [u8; 32] {
            sha256::Hash::hash(bytes).to_byte_array()
        }
        fn root(keys: &[String]) -> [u8; 32] {
            if keys.len() == 1 {
                return hash(&[&[0], keys[0].as_bytes()].concat());
            }
            let split = 1usize << (keys.len() - 1).ilog2();
            hash(&[&[1][..], &root(&keys[..split]), &root(&keys[split..])].concat())
        }
        let mut bytes = vec![2, self.name.len() as u8];
        bytes.extend(self.name.as_bytes());
        bytes.extend(serialize(&VarInt(self.template.len() as u64)));
        bytes.extend(hash(self.template.as_bytes()));
        bytes.extend(serialize(&VarInt(self.keys.len() as u64)));
        bytes.extend(root(&self.keys));
        hash(&bytes)
    }

    fn proof(&self, hmac: Option<[u8; 32]>) -> Result<[u8; 32], Error> {
        hmac.or_else(|| self.name.is_empty().then_some([0; 32]))
            .ok_or(Error::MissingPolicy)
    }
}

fn signatures(psbt: &Psbt) -> u64 {
    psbt.inputs
        .iter()
        .map(|input| {
            (input.partial_sigs.len()
                + input.tap_script_sigs.len()
                + usize::from(input.tap_key_sig.is_some())) as u64
        })
        .sum()
}

fn uint(value: u64) -> Value {
    Value::Integer(value.into())
}
fn bytes(value: &[u8]) -> Value {
    Value::Bytes(value.to_vec())
}
fn number(value: &Value) -> Result<u64, Error> {
    match value {
        Value::Integer(n) => (*n)
            .try_into()
            .map_err(|_| Error::Unexpected("Invalid unsigned integer")),
        _ => Err(Error::Unexpected("Expected unsigned integer")),
    }
}
fn array(value: &Value, len: usize) -> Result<&[Value], Error> {
    match value {
        Value::Array(values) if values.len() == len => Ok(values),
        _ => Err(Error::Unexpected("Invalid Thunder Den array length")),
    }
}
fn data(value: &Value, len: usize) -> Result<&[u8], Error> {
    match value {
        Value::Bytes(bytes) if bytes.len() == len => Ok(bytes),
        _ => Err(Error::Unexpected("Invalid Thunder Den byte length")),
    }
}
fn text(value: &Value) -> Result<&str, Error> {
    match value {
        Value::Text(text) if text.len() <= 512 && text.bytes().all(|b| (32..=126).contains(&b)) => {
            Ok(text)
        }
        _ => Err(Error::Unexpected("Invalid Thunder Den text")),
    }
}
