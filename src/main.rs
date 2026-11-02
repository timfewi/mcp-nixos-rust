use std::process::ExitCode;
use std::sync::Arc;

use mcp_nixos::App;
use mcp_nixos::http::Http;
use mcp_nixos::server::Server;
use rmcp::service::ServerInitializeError;
use rmcp::{ServiceExt, transport::stdio};

const HELP: &str = "mcp-nixos - MCP server for NixOS, Home Manager, and nix-darwin

Usage: mcp-nixos [--help | --version]

Speaks MCP revisions 2026-07-28 and 2025-11-25 over stdin/stdout. Exits when stdin closes.";

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let http = Http::from_env()?;
    match Server::new(Arc::new(App::new(http))).serve(stdio()).await {
        Ok(running) => {
            running.waiting().await?;
            Ok(())
        }
        // Stdin closing is the normal shutdown signal, whenever it happens.
        Err(ServerInitializeError::ConnectionClosed(_)) => Ok(()),
        Err(failure) => Err(failure.into()),
    }
}

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--help" | "-h") => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Some("--version" | "-V") => {
            println!("mcp-nixos {}", mcp_nixos::config::VERSION);
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(Into::into)
        .and_then(|runtime| runtime.block_on(serve()));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mcp-nixos: error: {err}");
            ExitCode::FAILURE
        }
    }
}
