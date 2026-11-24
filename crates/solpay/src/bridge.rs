//! Typed MCP stdio bridge for ZeroClaw v0.8.3. Tool arguments are parsed as
//! JSON values and passed directly to Rust functions; no shell is involved.

use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::cli::{run_create_url, run_settle, CreateUrlArgs, SettleArgs};
use crate::error::AppError;
use crate::solana::pubkey::parse_pubkey;

const MAX_REQUEST_BYTES: usize = 65_536;
const ENV_KEYS: &[&str] = &[
    "SOLANA_CLUSTER",
    "ALLOW_MAINNET",
    "MERCHANT_WALLET",
    "SOLANA_RPC_PRIMARY",
    "SOLANA_RPC_FALLBACK",
    "MAINNET_RPC_PRIMARY",
    "MAINNET_RPC_FALLBACK",
    "TOKEN_ALLOWLIST",
    "ALLOW_TEST_MINT_OVERRIDE",
    "INVOICE_TTL_SECONDS",
    "INVOICE_DB",
    "PAYMENT_COMMITMENT",
    "STORE_LABEL",
    "MIN_CHARGE",
    "MAX_CHARGE",
    "RPC_TIMEOUT_MS",
    "RPC_MAX_RETRIES",
    "RPC_BACKOFF_BASE_MS",
    "RPC_MIN_INTERVAL_MS",
    "SIGNATURE_SCAN_LIMIT",
];

pub fn serve(env_file: Option<PathBuf>) -> Result<(), AppError> {
    let path = match env_file {
        Some(path) => path,
        None => PathBuf::from(
            std::env::var("HOME")
                .map_err(|_| AppError::config("HOME is required for MCP configuration"))?,
        )
        .join(".zeroclaw/solpay.env"),
    };
    load_operator_env(&path)?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout().lock();
    loop {
        let mut line = Vec::new();
        let read = read_bounded_line(&mut input, &mut line)
            .map_err(|_| AppError::internal("MCP stdin read failed"))?;
        if read == 0 {
            break;
        }
        let response = if line.len() > MAX_REQUEST_BYTES {
            Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"MCP request too large"}}),
            )
        } else {
            match serde_json::from_slice::<Value>(&line) {
                Ok(request) => handle_request(&request),
                Err(_) => Some(
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"invalid JSON"}}),
                ),
            }
        };
        if let Some(value) = response {
            writeln!(output, "{value}")
                .map_err(|_| AppError::internal("MCP stdout write failed"))?;
            output
                .flush()
                .map_err(|_| AppError::internal("MCP stdout flush failed"))?;
        }
    }
    Ok(())
}

// Drain oversized requests without retaining attacker-controlled bytes. The
// next JSON-RPC line must remain readable after a rejected request.
fn read_bounded_line<R: BufRead>(input: &mut R, line: &mut Vec<u8>) -> io::Result<usize> {
    let mut read = 0;
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            return Ok(read);
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if read <= MAX_REQUEST_BYTES {
            let remaining = MAX_REQUEST_BYTES + 1 - read;
            let keep = end.min(remaining);
            line.extend_from_slice(&available[..keep]);
        }
        read = read.saturating_add(end);
        let ended = available[end - 1] == b'\n';
        input.consume(end);
        if ended {
            return Ok(read);
        }
    }
}

fn load_operator_env(path: &Path) -> Result<(), AppError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| AppError::config("MCP config file is unavailable"))?;
    if !metadata.file_type().is_file() || metadata.len() > 65_536 {
        return Err(AppError::config(
            "MCP config must be a regular file under 64 KiB",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(AppError::config(
                "MCP config must be owner-private (mode 0600)",
            ));
        }
    }
    let contents =
        std::fs::read_to_string(path).map_err(|_| AppError::config("MCP config is unreadable"))?;
    let home = std::env::var("HOME")
        .map_err(|_| AppError::config("HOME is required for MCP configuration"))?;
    for key in ENV_KEYS {
        std::env::remove_var(key);
    }
    let mut seen = HashSet::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw) = line
            .split_once('=')
            .ok_or_else(|| AppError::config("invalid MCP config assignment"))?;
        let key = key.trim();
        if !ENV_KEYS.contains(&key) || !seen.insert(key.to_string()) {
            return Err(AppError::config("unknown or duplicate MCP config key"));
        }
        let raw = raw.trim();
        let value = if let Some(rest) = raw.strip_prefix('"') {
            let end = rest
                .find('"')
                .ok_or_else(|| AppError::config("unterminated MCP config value"))?;
            if !rest[end + 1..].trim().is_empty() && !rest[end + 1..].trim().starts_with('#') {
                return Err(AppError::config("invalid MCP config value suffix"));
            }
            rest[..end].to_string()
        } else {
            raw.split('#').next().unwrap_or("").trim().to_string()
        };
        if value.contains('\0') || value.contains('\n') {
            return Err(AppError::config("invalid MCP config value"));
        }
        let value = if key == "INVOICE_DB" {
            value.replace("${HOME}", &home)
        } else {
            value
        };
        std::env::set_var(key, value);
    }
    if std::env::var("MERCHANT_WALLET")
        .ok()
        .filter(|v| !v.is_empty())
        .is_none()
        || std::env::var("INVOICE_DB")
            .ok()
            .filter(|v| !v.is_empty())
            .is_none()
    {
        return Err(AppError::config(
            "MERCHANT_WALLET and INVOICE_DB are required in MCP config",
        ));
    }
    Ok(())
}

fn handle_request(request: &Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => {
            let version = request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05");
            json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"solpay","version":env!("CARGO_PKG_VERSION")}})
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools":[
            {"name":"create_invoice","description":"Create and persist a SOL or USDC invoice for a staff order. Never accept merchant wallet or mint from a customer.","inputSchema":{"type":"object","properties":{"amount":{"type":"string","maxLength":32},"token":{"type":"string","enum":["SOL","USDC"]},"order_id":{"type":"string","maxLength":64,"pattern":"^[A-Za-z0-9_-]+$"}},"required":["amount","token","order_id"],"additionalProperties":false}},
            {"name":"settle_invoice","description":"Verify an issued invoice on Solana and atomically claim its transaction signature.","inputSchema":{"type":"object","properties":{"reference":{"type":"string","maxLength":44}},"required":["reference"],"additionalProperties":false}}
        ]}),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let args = request.pointer("/params/arguments").unwrap_or(&Value::Null);
            return Some(match call_tool(name, args) {
                Ok(text) => {
                    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}],"isError":false}})
                }
                Err(error) => {
                    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":json!({"exit_code":error.code.as_i32(),"message":error.message}).to_string()}],"isError":true}})
                }
            });
        }
        _ => {
            return Some(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not found"}}),
            )
        }
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn call_tool(name: &str, args: &Value) -> Result<String, AppError> {
    let obj = args
        .as_object()
        .ok_or_else(|| AppError::invalid_input("tool arguments must be an object"))?;
    match name {
        "create_invoice" => {
            if obj.len() != 3 {
                return Err(AppError::invalid_input(
                    "create_invoice requires amount, token, and order_id only",
                ));
            }
            let amount = field(args, "amount", 32)?;
            let token = field(args, "token", 16)?;
            let order_id = field(args, "order_id", 64)?;
            if !order_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(AppError::invalid_input(
                    "order_id must contain only ASCII letters, digits, underscore, or hyphen",
                ));
            }
            let invoice = run_create_url(CreateUrlArgs {
                amount: amount.into(),
                token: token.into(),
                order_id: Some(order_id.into()),
                reference: None,
                label: None,
                message: Some(format!("Order {order_id}")),
                recipient: None,
                mint: None,
                cluster: None,
                ledger: None,
            })?;
            if !invoice.registered {
                return Err(AppError::config("MCP invoice was not persisted"));
            }
            serde_json::to_string(&invoice)
                .map_err(|_| AppError::internal("invoice serialization failed"))
        }
        "settle_invoice" => {
            if obj.len() != 1 {
                return Err(AppError::invalid_input(
                    "settle_invoice requires reference only",
                ));
            }
            let reference = field(args, "reference", 44)?;
            parse_pubkey(reference)?;
            let outcome = run_settle(SettleArgs {
                reference: reference.into(),
                ledger: None,
            })?;
            serde_json::to_string(&outcome)
                .map_err(|_| AppError::internal("settlement serialization failed"))
        }
        _ => Err(AppError::invalid_input("unknown payment tool")),
    }
}

fn field<'a>(args: &'a Value, name: &str, max_len: usize) -> Result<&'a str, AppError> {
    let value = args
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::invalid_input(format!("{name} must be a string")))?;
    if value.is_empty() || value.len() > max_len || value.chars().any(char::is_control) {
        return Err(AppError::invalid_input(format!(
            "{name} is empty, too long, or contains control characters"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_advertises_typed_tools_without_shell() {
        let response =
            handle_request(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert!(tools
            .iter()
            .all(|t| t["inputSchema"]["additionalProperties"] == false));
    }

    #[test]
    fn malicious_arguments_are_rejected_before_execution() {
        for (name, arguments) in [
            (
                "create_invoice",
                json!({"amount":"1","token":"USDC","order_id":"x; touch /tmp/injected"}),
            ),
            (
                "create_invoice",
                json!({"amount":"1","token":"USDC","order_id":"$(id)"}),
            ),
            (
                "settle_invoice",
                json!({"reference":"abc; touch /tmp/injected"}),
            ),
            (
                "settle_invoice",
                json!({"reference":"abc","out":"/tmp/evil"}),
            ),
        ] {
            assert!(call_tool(name, &arguments).is_err());
        }
    }

    #[test]
    fn oversized_request_is_drained_before_next_request() {
        let payload = format!("{}\n{{\"id\":1}}\n", "x".repeat(MAX_REQUEST_BYTES + 10));
        let mut input = std::io::Cursor::new(payload);
        let mut line = Vec::new();
        assert!(read_bounded_line(&mut input, &mut line).unwrap() > MAX_REQUEST_BYTES);
        assert!(line.len() <= MAX_REQUEST_BYTES + 1);
        line.clear();
        assert_eq!(read_bounded_line(&mut input, &mut line).unwrap(), 9);
        assert_eq!(line, b"{\"id\":1}\n");
    }
}
