mod board;
mod error;
mod gallery;
mod python;
mod render;
mod server;

use crate::board::AppState;
use crate::server::ScryServer;
use clap::Parser;
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(clap::Parser)]
#[command(name = "scry-mcp", about = "Computational scrying glass — MCP visual scratchpad")]
struct Cli {
    /// Gallery web server bind address (only used with --port)
    #[arg(long, default_value = "127.0.0.1")]
    address: String,
    /// Gallery web server port. Omit to run headless (no HTTP listener).
    #[arg(long)]
    port: Option<u16>,
    /// Directory to write PNG/SVG output files. Created if it doesn't exist.
    #[arg(long)]
    output_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Tracing MUST go to stderr — stdout is MCP JSON-RPC transport
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("scry_mcp=info".parse()?)
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    // Validate and create output directory if requested
    if let Some(ref dir) = cli.output_dir {
        std::fs::create_dir_all(dir).map_err(|e| {
            anyhow::anyhow!("Failed to create output directory {}: {}", dir.display(), e)
        })?;
        tracing::info!("File output enabled: {}", dir.display());
    }

    let gallery_addr = cli.port.map(|p| (cli.address.clone(), p));

    match &gallery_addr {
        Some((addr, port)) => tracing::info!("Scry MCP starting — gallery on {addr}:{port}"),
        None => tracing::info!("Scry MCP starting — headless (no gallery)"),
    }

    let state = AppState::new(gallery_addr.clone(), cli.output_dir);

    // Spawn web gallery only if --port was provided
    let gallery_handle = if let Some((ref addr, port)) = gallery_addr {
        let gallery_router = gallery::router(state.clone());
        let bind_addr = format!("{addr}:{port}");
        let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
        tracing::info!("Gallery listening on {bind_addr}");

        Some(tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, gallery_router).await {
                tracing::error!("Gallery server error: {e}");
            }
        }))
    } else {
        None
    };

    // Serve MCP on stdio
    let server = ScryServer::new(state);
    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("MCP serve error: {e:?}");
    })?;

    // Wait for MCP session to end
    service.waiting().await?;
    tracing::info!("MCP session ended, shutting down");

    // Shutdown gallery
    if let Some(handle) = gallery_handle {
        handle.abort();
    }

    Ok(())
}
