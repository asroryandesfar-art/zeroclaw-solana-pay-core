//! Black-box MCP stdio and injection regression tests; no shell or live RPC.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_solpay");
const WALLET: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";

#[test]
fn mcp_process_creates_invoice_and_treats_metacharacters_as_data() {
    let directory = std::env::temp_dir().join(format!(
        "solpay-mcp-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let config = directory.join("solpay.env");
    let database = directory.join("invoices.sqlite");
    let marker = directory.join("injected");
    std::fs::write(&config, format!(
        "MERCHANT_WALLET=\"{WALLET}\"\nINVOICE_DB=\"{}\"\nSOLANA_CLUSTER=\"devnet\"\nSTORE_LABEL=\"$(touch {})\"\n",
        database.display(), marker.display()
    )).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut child = Command::new(BIN)
        .args(["mcp-serve", "--env-file", config.to_str().unwrap()])
        .env_clear()
        .env("HOME", directory.to_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"create_invoice","arguments":{"amount":"1","token":"USDC","order_id":"ORDER_1"}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"create_invoice","arguments":{"amount":"1; touch injected","token":"USDC","order_id":"ORDER_2"}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"settle_invoice","arguments":{"reference":format!("x; touch {}",marker.display())}}}),
    ];
    let stdin = child.stdin.as_mut().unwrap();
    for request in requests {
        writeln!(stdin, "{request}").unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 5); // notification has no response
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 2);
    let invoice: Value = serde_json::from_str(
        responses[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(invoice["registered"], true);
    assert_eq!(invoice["label"], format!("$(touch {})", marker.display()));
    assert_eq!(responses[3]["result"]["isError"], true);
    assert_eq!(responses[4]["result"]["isError"], true);
    assert!(
        !marker.exists(),
        "shell metacharacters executed unexpectedly"
    );
}
