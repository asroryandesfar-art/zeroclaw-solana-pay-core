//! Black-box issuance and settlement against a deterministic local RPC server.
//! No Solana network or funds are involved.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use solpay::solana::ata::associated_token_address;
use solpay::solana::pubkey::parse_pubkey;

const BIN: &str = env!("CARGO_BIN_EXE_solpay");
const WALLET: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";
const MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";

fn run(args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(BIN);
    cmd.env_clear();
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let output = cmd.args(args).output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..at]);
            let length: usize = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse().ok())
                })
                .unwrap_or(0);
            if bytes.len() >= at + 4 + length {
                return String::from_utf8_lossy(&bytes[at + 4..at + 4 + length]).into_owned();
            }
        }
    }
    panic!("incomplete HTTP request")
}

#[test]
fn one_transaction_cannot_settle_two_invoices_and_repeat_is_idempotent() {
    let path = std::env::temp_dir().join(format!(
        "solpay-integration-{}-{}.sqlite",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = path.to_str().unwrap();
    let envs = [
        ("MERCHANT_WALLET", WALLET),
        ("INVOICE_DB", db),
        ("SOLANA_CLUSTER", "devnet"),
    ];
    let issue = || {
        let (code, stdout, stderr) =
            run(&["create-url", "--amount", "25", "--token", "USDC"], &envs);
        assert_eq!(code, 0, "{stderr}");
        let value: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(value["registered"], true);
        value
    };
    let first = issue();
    let second = issue();
    let first_ref = first["reference"].as_str().unwrap();
    let second_ref = second["reference"].as_str().unwrap();
    assert_ne!(first_ref, second_ref);

    let ata =
        associated_token_address(&parse_pubkey(WALLET).unwrap(), &parse_pubkey(MINT).unwrap())
            .to_string();
    let block_time = first["issued_at_unix"]
        .as_i64()
        .unwrap()
        .max(second["issued_at_unix"].as_i64().unwrap());
    let signature = bs58::encode([9u8; 64]).into_string();
    let mut transfer_data = vec![3u8];
    transfer_data.extend_from_slice(&25_000_000u64.to_le_bytes());
    let tx = json!({
        "slot": 42, "blockTime": block_time,
        "meta": {
            "err": null, "preBalances": [1000000,2039280,0,0,0],
            "postBalances": [995000,2039280,0,0,0],
            "preTokenBalances": [{"accountIndex":1,"mint":MINT,"owner":WALLET,"programId":"TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA","uiTokenAmount":{"amount":"0","decimals":6}}],
            "postTokenBalances": [{"accountIndex":1,"mint":MINT,"owner":WALLET,"programId":"TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA","uiTokenAmount":{"amount":"25000000","decimals":6}}]
        },
        "transaction": {"signatures":[signature.clone()],"message":{"accountKeys":["payer",ata,first_ref,second_ref,"TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"],
            "instructions":[{"programIdIndex":4,"accounts":[0,1,0,2],"data":bs58::encode(transfer_data).into_string()}]}}
    });
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut handled = 0;
        while handled < 8 && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request: Value = serde_json::from_str(&read_request(&mut stream)).unwrap();
                    let result = match request["method"].as_str().unwrap() {
                        "getGenesisHash" => json!("EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG"),
                        "getSignaturesForAddress" => {
                            json!([{"signature":signature.clone(),"confirmationStatus":"confirmed","err":null}])
                        }
                        "getTransaction" => tx.clone(),
                        other => panic!("unexpected RPC method {other}"),
                    };
                    let body = json!({"jsonrpc":"2.0","id":1,"result":result}).to_string();
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                    handled += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("local RPC server failed: {e}"),
            }
        }
        handled
    });

    let settle_env = [
        ("MERCHANT_WALLET", WALLET),
        ("INVOICE_DB", db),
        ("SOLANA_CLUSTER", "devnet"),
        ("SOLANA_RPC_PRIMARY", endpoint.as_str()),
        ("RPC_MAX_RETRIES", "0"),
        // A later policy change cannot invalidate an invoice already issued.
        ("MIN_CHARGE", "30"),
    ];
    let (code1, out1, err1) = run(&["settle", "--reference", first_ref], &settle_env);
    assert_eq!(code1, 0, "{err1}");
    let paid: Value = serde_json::from_str(&out1).unwrap();
    assert_eq!(paid["status"], "paid");
    assert_eq!(paid["newly_settled"], true);

    let (code2, out2, err2) = run(&["settle", "--reference", second_ref], &settle_env);
    assert_eq!(code2, 0, "{err2}");
    let replay: Value = serde_json::from_str(&out2).unwrap();
    assert_eq!(replay["status"], "mismatch");
    assert_eq!(replay["newly_settled"], false);

    let (code3, out3, err3) = run(&["settle", "--reference", first_ref], &settle_env);
    assert_eq!(code3, 0, "{err3}");
    let repeated: Value = serde_json::from_str(&out3).unwrap();
    assert_eq!(repeated["status"], "paid");
    assert_eq!(repeated["newly_settled"], false);
    assert_eq!(server.join().unwrap(), 8);
}
