//! `solpay` CLI entrypoint.
//!
//! Exit codes (the contract ZeroClaw SOPs branch on):
//!   0 success · 2 invalid input · 3 config error · 4 RPC/transient · 5 internal
//!
//! clap maps argument/usage errors to exit code 2, which aligns with our
//! "invalid input" category.

use clap::Parser;

use solpay::cli::{dispatch, Cli, Command};
use solpay::output::OutputFormat;

fn main() {
    let cli = Cli::parse();
    let format: OutputFormat = cli.format.into();

    // A packaged binary pins the issuer and checks activation before *every*
    // operational entrypoint, including direct binary and MCP execution.
    #[cfg(feature = "commercial-package")]
    if !matches!(
        &cli.command,
        Command::LicenseCheck(_) | Command::LicenseIssuer
    ) {
        if let Err(message) = solpay::license::check_installed() {
            eprintln!(
                "{}",
                serde_json::json!({"level":"error","exit_code":3,"message":message})
            );
            std::process::exit(3);
        }
    }

    if let Command::McpServe(args) = &cli.command {
        if let Err(e) = solpay::bridge::serve(args.env_file.clone()) {
            eprintln!(
                "{}",
                serde_json::json!({"level":"error","exit_code":e.code.as_i32(),"message":e.message})
            );
            std::process::exit(e.code.as_i32());
        }
        return;
    }

    match dispatch(cli.command, format) {
        Ok(rendered) => {
            println!("{rendered}");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!(
                "{}",
                serde_json::json!({
                    "level": "error",
                    "exit_code": e.code.as_i32(),
                    "message": e.message,
                })
            );
            std::process::exit(e.code.as_i32());
        }
    }
}
