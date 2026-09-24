//! Public-fixture integration with the real C++ signer. Run with TD_RUNNER set
//! and `cargo test --features thunderden --test thunderden_native -- --ignored`.
#![cfg(feature = "thunderden")]

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    str::FromStr,
    sync::Mutex,
};

use async_hwi::{
    thunderden::{ThunderDen, Transport},
    AddressScript, Error, HWI,
};
use async_trait::async_trait;
use bitcoin::{
    bip32::DerivationPath,
    hex::{DisplayHex, FromHex},
    psbt::Psbt,
    Network,
};

struct Runner {
    child: Child,
    pipes: Mutex<(ChildStdin, BufReader<ChildStdout>)>,
}

impl Runner {
    fn new(mode: &str) -> Self {
        let mut child = Command::new(
            std::env::var("TD_RUNNER").expect("Set TD_RUNNER to the public-fixture runner"),
        )
        .arg(mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            pipes: Mutex::new((input, output)),
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[async_trait]
impl Transport for Runner {
    async fn exchange(&self, request: &[u8]) -> Result<Vec<u8>, Error> {
        let mut pipes = self.pipes.lock().unwrap();
        writeln!(pipes.0, "{}", request.as_hex()).unwrap();
        pipes.0.flush().unwrap();
        let mut line = String::new();
        pipes.1.read_line(&mut line).unwrap();
        Vec::<u8>::from_hex(line.trim()).map_err(|_| Error::Unexpected("Invalid fixture reply"))
    }
}

#[tokio::test]
#[ignore = "requires the Thunder Den C++ public-fixture runner"]
async fn register_display_and_sign_with_two_keys() {
    let runner = std::env::var("TD_RUNNER").expect("Set TD_RUNNER");
    let fixtures = Command::new(&runner).arg("--fixtures").output().unwrap();
    assert!(fixtures.status.success());
    let fixture: serde_json::Value = serde_json::from_slice(&fixtures.stdout).unwrap();
    let descriptor = fixture["descriptor"].as_str().unwrap();
    let name = fixture["name"].as_str().unwrap();
    let path = DerivationPath::from_str("m/48h/1h/0h/2h").unwrap();
    let alice = ThunderDen::new(Runner::new("--alice"), Network::Regtest).unwrap();
    let bob = ThunderDen::new(Runner::new("--bob"), Network::Regtest).unwrap();
    let akey = alice.get_extended_pubkey(&path).await.unwrap();
    let bkey = bob.get_extended_pubkey(&path).await.unwrap();
    assert_ne!(akey, bkey);
    assert_eq!(
        alice.get_master_fingerprint().await.unwrap().to_string(),
        fixture["alice_fingerprint"].as_str().unwrap()
    );
    assert!(matches!(
        alice.get_version().await,
        Err(Error::UnsupportedVersion)
    ));
    let aproof = alice
        .register_wallet(name, descriptor)
        .await
        .unwrap()
        .unwrap();
    let bproof = bob
        .register_wallet(name, descriptor)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(aproof, bproof);
    let alice = alice.with_wallet(name, descriptor, Some(aproof)).unwrap();
    let bob = bob.with_wallet(name, descriptor, Some(bproof)).unwrap();
    assert!(alice.is_wallet_registered(name, descriptor).await.unwrap());
    assert!(!alice
        .is_wallet_registered("renamed", descriptor)
        .await
        .unwrap());
    for index in [0, 17] {
        for change in [false, true] {
            let address = AddressScript::Miniscript { index, change };
            alice.display_address(&address).await.unwrap();
            bob.display_address(&address).await.unwrap();
        }
    }
    alice
        .display_address(&AddressScript::P2TR(
            DerivationPath::from_str("m/86h/1h/0h/1/7").unwrap(),
        ))
        .await
        .unwrap();
    for tx in fixture["transactions"].as_array().unwrap() {
        let raw = Vec::<u8>::from_hex(tx["psbt_hex"].as_str().unwrap()).unwrap();
        let mut psbt = Psbt::deserialize(&raw).unwrap();
        let original = psbt.unsigned_tx.clone();
        alice.sign_tx(&mut psbt).await.unwrap();
        bob.sign_tx(&mut psbt).await.unwrap();
        assert_eq!(psbt.unsigned_tx, original);
        let mut check = Command::new(&runner)
            .arg("--verify")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(check.stdin.take().unwrap(), "{}", psbt.serialize().as_hex()).unwrap();
        assert!(check.wait().unwrap().success());
    }
    let restarted = ThunderDen::new(Runner::new("--alice"), Network::Regtest)
        .unwrap()
        .with_wallet(name, descriptor, Some(aproof))
        .unwrap();
    restarted
        .display_address(&AddressScript::Miniscript {
            index: 3,
            change: false,
        })
        .await
        .unwrap();
    let wrong = ThunderDen::new(Runner::new("--alice"), Network::Regtest)
        .unwrap()
        .with_wallet(name, descriptor, Some(bproof))
        .unwrap();
    assert!(wrong
        .display_address(&AddressScript::Miniscript {
            index: 3,
            change: false
        })
        .await
        .is_err());
    let declined = ThunderDen::new(Runner::new("--decline"), Network::Regtest).unwrap();
    assert!(matches!(
        declined.get_extended_pubkey(&path).await,
        Err(Error::UserRefused)
    ));
    let wrong_network = ThunderDen::new(Runner::new("--alice"), Network::Bitcoin).unwrap();
    assert!(matches!(
        wrong_network.get_master_fingerprint().await,
        Err(Error::NetworkMismatch)
    ));
}
