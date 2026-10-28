//! `lemusic healthcheck`: fragt den eigenen `/health`-Endpoint ab.
//!
//! Für den Docker-`HEALTHCHECK` – so braucht das Image weder curl noch wget.
//! Exit-Code 0 = gesund, sonst ungesund.

use std::{net::SocketAddr, time::Duration};

use anyhow::{Context as _, Result, bail};
use lemusic_core::config::web_bind_from_env;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const TIMEOUT: Duration = Duration::from_secs(4);

pub async fn run() -> Result<()> {
    let bind = web_bind_from_env()?;
    // 0.0.0.0 ist keine Zieladresse – lokal verbinden.
    let target = SocketAddr::from(([127, 0, 0, 1], bind.port()));
    tokio::time::timeout(TIMEOUT, check(target))
        .await
        .context("Zeitüberschreitung")?
}

async fn check(target: SocketAddr) -> Result<()> {
    let mut stream = TcpStream::connect(target).await?;
    stream
        .write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let response = String::from_utf8_lossy(&response);
    let status_line = response.lines().next().unwrap_or_default();
    if status_line.split_whitespace().nth(1) == Some("200") {
        Ok(())
    } else {
        let body = response.split("\r\n\r\n").nth(1).unwrap_or_default();
        bail!("ungesund: {status_line} {body}")
    }
}
