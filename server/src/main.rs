mod server;

use std::path::PathBuf;

use anyhow::{Context, Result};
use rmcp::{transport::stdio, ServiceExt};
use tracing::info;

const DEFAULT_KB_DIR: &str = "data";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false)
        .init();

    let kb_dir = std::env::var("KB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_KB_DIR));

    let kb = kb_core::KnowledgeBase::new(kb_dir)
        .context("Failed to initialize Knowledge Base")?;

    let server = server::KbServer::new(kb);

    info!("knowledge-base MCP server starting on stdio");

    let service = server
        .serve(stdio())
        .await
        .context("Failed to start MCP service")?;
    service.waiting().await?;

    Ok(())
}
