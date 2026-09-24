use super::*;
use bitcoin::{
    absolute, psbt::raw, transaction, Amount, ScriptBuf, Transaction, TxIn, TxOut, Witness,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const XPUB: &str = "tpubD6NzVbkrYhZ4Wzt8snb1hHKjrMidYf5xQBsjMqshTmRQDhF12fEyHWCaBWXCZ3UUaaRPfbPP4AvFSQdSoqijQRsg1wb4xE2XbYGFUai3ME3";

struct Mock<F> {
    respond: F,
    calls: AtomicUsize,
}

#[async_trait]
impl<F: Fn(&[u8]) -> Vec<u8> + Send + Sync> Transport for Mock<F> {
    async fn exchange(&self, request: &[u8]) -> Result<Vec<u8>, Error> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok((self.respond)(request))
    }
}

fn mock<F: Fn(&[u8]) -> Vec<u8> + Send + Sync>(respond: F) -> ThunderDen<Mock<F>> {
    ThunderDen::new(
        Mock {
            respond,
            calls: AtomicUsize::new(0),
        },
        Network::Regtest,
    )
    .unwrap()
}

fn reply(request: &[u8], result: Vec<Value>) -> Vec<Value> {
    let request: Value = serde_cbor::from_slice(request).unwrap();
    let request = array(&request, 5).unwrap();
    vec![
        uint(3),
        request[1].clone(),
        request[2].clone(),
        bytes(&[1, 2, 3, 4]),
        Value::Text("development".into()),
        request[3].clone(),
        uint(0),
        Value::Array(result),
    ]
}

#[tokio::test]
async fn metadata_is_cached_and_futures_are_send() {
    fn send_sync<T: Send + Sync>(_: &T) {}
    let client = Arc::new(mock(|request| {
        serde_cbor::to_vec(&reply(request, vec![])).unwrap()
    }));
    send_sync(&client);
    let task: Arc<dyn HWI + Send + Sync> = client.clone();
    let fp = tokio::spawn(async move { task.get_master_fingerprint().await })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fp, Fingerprint::from([1, 2, 3, 4]));
    assert!(matches!(
        client.get_version().await,
        Err(Error::UnsupportedVersion)
    ));
    assert_eq!(client.get_master_fingerprint().await.unwrap(), fp);
    assert_eq!(client.transport.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn reject_mismatched_and_malformed_replies() {
    for field in [0, 1, 2, 3, 5, 6, 7] {
        let client = mock(move |request| {
            let mut value = reply(request, vec![]);
            value[field] = match field {
                0 => uint(2),
                1 => bytes(&[0; 16]),
                2 => Value::Text("main".into()),
                3 => bytes(&[0; 32]),
                5 => uint(4),
                6 => uint(99),
                _ => Value::Array(vec![uint(1)]),
            };
            serde_cbor::to_vec(&value).unwrap()
        });
        assert!(client.get_master_fingerprint().await.is_err());
        assert!(client.state.lock().await.info.is_none());
    }
    for mode in 0..4 {
        let client = mock(move |request| {
            let mut value = serde_cbor::to_vec(&reply(request, vec![])).unwrap();
            match mode {
                0 => {
                    value.splice(0..1, [0x98, 8]);
                }
                1 => value.push(0),
                2 => {
                    value.pop();
                }
                _ => value.resize(MAX_REPLY + 1, 0),
            }
            value
        });
        assert!(client.get_master_fingerprint().await.is_err());
    }
    let client = mock(|request| {
        let mut value = reply(request, vec![]);
        value[6] = uint(1);
        serde_cbor::to_vec(&value).unwrap()
    });
    assert!(matches!(
        client.get_master_fingerprint().await,
        Err(Error::UserRefused)
    ));
}

#[tokio::test]
async fn missing_keys_and_proof_do_not_send() {
    let client = mock(|_| panic!("Unexpected exchange"));
    assert!(!client
        .is_wallet_registered("test", "invalid")
        .await
        .unwrap());
    assert!(client.with_wallet("test", "wpkh(not-a-key)", None).is_err());
    let policy = format!("tr({}/<0;1>/*)", XPUB);
    let client = mock(|_| panic!("Unexpected exchange"))
        .with_wallet("test", &policy, None)
        .unwrap();
    assert!(!client.is_wallet_registered("test", &policy).await.unwrap());
    assert!(matches!(
        client
            .display_address(&AddressScript::Miniscript {
                index: 0,
                change: false
            })
            .await,
        Err(Error::MissingPolicy)
    ));
    assert_eq!(format!("{:?}", client), "ThunderDen { network: Regtest }");
}

#[tokio::test]
async fn address_reply_and_index_are_checked() {
    let script = ScriptBuf::new();
    for (address, valid) in [
        (Address::p2wsh(&script, Network::Regtest).to_string(), true),
        (Address::p2wsh(&script, Network::Bitcoin).to_string(), false),
        ("invalid address".into(), false),
    ] {
        let client = mock(move |request| {
            serde_cbor::to_vec(&reply(request, vec![Value::Text(address.clone())])).unwrap()
        })
        .with_wallet("test", &format!("tr({}/<0;1>/*)", XPUB), Some([0x42; 32]))
        .unwrap();
        assert!(matches!(
            client
                .display_address(&AddressScript::Miniscript {
                    index: 1 << 31,
                    change: false,
                })
                .await,
            Err(Error::UnsupportedInput)
        ));
        assert_eq!(client.transport.calls.load(Ordering::Relaxed), 0);
        assert_eq!(
            client
                .display_address(&AddressScript::Miniscript {
                    index: 0,
                    change: false,
                })
                .await
                .is_ok(),
            valid
        );
    }
}

#[tokio::test]
async fn http_transport_rejects_nonlocal_endpoints() {
    for url in [
        "invalid URL",
        "http://example.com/exchange",
        "https://127.0.0.1:32123/exchange",
        "http://user:password@127.0.0.1:32123/exchange",
        "http://127.0.0.1:32123/exchange#token",
        "http://127.0.0.1:32123/exchange?query",
    ] {
        assert!(HttpTransport::connect(url).await.is_err());
    }
}

#[tokio::test]
async fn http_probe_identifies_bridge_without_an_optical_exchange() {
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
    };
    for (status, body, found) in [
        (200, "thunderden-qr-bridge", true),
        (200, "another service", false),
        (503, "thunderden-qr-bridge", false),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/exchange", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line, "GET /info HTTP/1.1\r\n");
            while line != "\r\n" {
                line.clear();
                assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
            }
            stream.write_all(format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
            ).as_bytes()).await.unwrap();
        });
        assert_eq!(HttpTransport::connect(&endpoint).await.is_ok(), found);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn bad_signing_replies_leave_the_psbt_untouched() {
    let mut original = Psbt::from_unsigned_tx(Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn::default()],
        output: vec![TxOut {
            value: Amount::from_sat(1000),
            script_pubkey: ScriptBuf::new(),
        }],
    })
    .unwrap();
    let field = raw::Key {
        type_value: 0xee,
        key: vec![1],
    };
    original.unknown.insert(field.clone(), vec![1]);
    for case in 0..5 {
        let mut signed = original.clone();
        signed.inputs[0].tap_key_sig =
            Some(bitcoin::taproot::Signature::from_slice(&[1; 64]).unwrap());
        match case {
            0 => signed.unsigned_tx.output[0].value = Amount::from_sat(999),
            1 => {
                signed.unknown.insert(field.clone(), vec![2]);
            }
            2 => signed.inputs[0].tap_key_sig = None,
            _ => {}
        }
        let client = mock(move |request| {
            serde_cbor::to_vec(&reply(
                request,
                vec![
                    bytes(&signed.serialize()),
                    uint(if case == 3 { 2 } else { 1 }),
                    uint(if case == 4 { 2 } else { 0 }),
                ],
            ))
            .unwrap()
        })
        .with_wallet("test", &format!("tr({}/<0;1>/*)", XPUB), Some([0x42; 32]))
        .unwrap();
        let mut psbt = original.clone();
        assert!(client.sign_tx(&mut psbt).await.is_err());
        assert_eq!(psbt, original);
    }
}

#[tokio::test]
async fn stripped_previous_witness_keeps_the_original_transaction() {
    let previous = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            witness: Witness::from_slice(&[vec![1, 2]]),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(100_000),
            script_pubkey: ScriptBuf::new(),
        }],
    };
    let mut original = Psbt::from_unsigned_tx(Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: bitcoin::OutPoint::new(previous.compute_txid(), 0),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(99_000),
            script_pubkey: ScriptBuf::new(),
        }],
    })
    .unwrap();
    original.inputs[0].non_witness_utxo = Some(previous.clone());
    original.inputs[0].witness_utxo = Some(previous.output[0].clone());

    for changed in [false, true] {
        let mut signed = original.clone();
        signed.inputs[0].non_witness_utxo.as_mut().unwrap().input[0].witness = if changed {
            Witness::from_slice(&[vec![3]])
        } else {
            Witness::new()
        };
        signed.inputs[0].tap_key_sig =
            Some(bitcoin::taproot::Signature::from_slice(&[1; 64]).unwrap());
        let client = mock(move |request| {
            serde_cbor::to_vec(&reply(
                request,
                vec![bytes(&signed.serialize()), uint(1), uint(0)],
            ))
            .unwrap()
        })
        .with_wallet("test", &format!("tr({}/<0;1>/*)", XPUB), Some([0x42; 32]))
        .unwrap();
        let mut psbt = original.clone();
        if changed {
            assert!(client.sign_tx(&mut psbt).await.is_err());
            assert_eq!(psbt, original);
        } else {
            client.sign_tx(&mut psbt).await.unwrap();
            assert_eq!(psbt.inputs[0].non_witness_utxo, Some(previous.clone()));
            assert!(psbt.inputs[0].tap_key_sig.is_some());
        }
    }
}

#[test]
fn policy_extraction_preserves_repeated_keys_and_proof_binding() {
    let text = format!("wsh(multi(2,{}/<0;1>/*,{}/<2;3>/*))", XPUB, XPUB);
    let wallet = Wallet::new("test".into(), &text).unwrap();
    let Value::Array(value) = wallet.value() else {
        panic!()
    };
    assert_eq!(
        value[1],
        Value::Text("wsh(multi(2,@0/<0;1>/*,@0/<2;3>/*))".into())
    );
    assert_eq!(array(&value[2], 1).unwrap()[0], Value::Text(XPUB.into()));
    assert_ne!(
        wallet.id(),
        Wallet::new("renamed".into(), &text).unwrap().id()
    );
    assert_eq!(
        wallet.id(),
        Wallet::new("test".into(), &(text + "#badchecksum"))
            .unwrap()
            .id()
    ); // Checksum validation belongs to the caller, as with the other adapters.
}
