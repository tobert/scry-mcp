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
use tracing_subscriber::EnvFilter;

#[derive(clap::Parser)]
#[command(name = "scry-mcp", about = "Computational scrying glass — MCP visual scratchpad")]
struct Cli {
    /// Gallery web server bind address
    #[arg(long, default_value = "127.0.0.1")]
    address: String,
    /// Gallery web server port
    #[arg(long, default_value_t = 3333)]
    port: u16,
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

    tracing::info!("Scry MCP starting — gallery on {}:{}", cli.address, cli.port);

    let state = AppState::new(cli.address.clone(), cli.port);

    // Spawn web gallery
    let gallery_router = gallery::router(state.clone());
    let bind_addr = format!("{}:{}", cli.address, cli.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("Gallery listening on {bind_addr}");

    let gallery_handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, gallery_router).await {
            tracing::error!("Gallery server error: {e}");
        }
    });

    // Serve MCP on stdio
    let server = ScryServer::new(state);
    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("MCP serve error: {e:?}");
    })?;

    // Wait for MCP session to end
    service.waiting().await?;
    tracing::info!("MCP session ended, shutting down");

    // Shutdown gallery
    gallery_handle.abort();

    Ok(())
}
