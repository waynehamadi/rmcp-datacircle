//! A stdio MCP server of Datacircle's four tools: find the best GTM APIs. `DATACIRCLE_BASE_URL` sets where it calls,
//! https://api.datacircle.dev/v1 when unset.

use rmcp::{ServiceExt, transport::stdio};
use rmcp_datacircle::Datacircle;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().skip(1).any(|arg| arg == "--version" || arg == "-V") {
        println!("datacircle-mcp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut builder = Datacircle::builder();
    if let Ok(base) = std::env::var("DATACIRCLE_BASE_URL")
        && !base.trim().is_empty()
    {
        builder = builder.base_url(base.trim());
    }
    let server = builder.build().serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}
