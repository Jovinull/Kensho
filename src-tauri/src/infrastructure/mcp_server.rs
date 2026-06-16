//! Minimal async MCP server over TCP (JSON-Lines).
//!
//! Each connection speaks newline-delimited JSON-RPC 2.0: one request per line,
//! one response per line. Requests are handed to [`crate::services::mcp_bridge`]
//! and routed into the shared `ToolRouter`. Binds to loopback only — Kensho
//! becomes a local daemon any other AI client on the machine can query.

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::services::mcp_bridge::{self, McpRequest};
use crate::services::ToolRouter;

/// Bind the loopback MCP listener on `port` (0 = OS-assigned, for tests).
pub async fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port)).await
}

/// Accept loop: one task per connection. Runs until the listener is dropped.
pub async fn serve(listener: TcpListener, router: ToolRouter) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                tracing::debug!(%peer, "mcp client connected");
                let router = router.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, router).await {
                        tracing::debug!(error = %e, "mcp connection closed");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "mcp accept failed");
            }
        }
    }
}

/// Read JSON-Lines requests, dispatch each, write JSON-Lines responses.
async fn handle_connection(stream: TcpStream, router: ToolRouter) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<McpRequest>(&line) {
            Ok(req) => mcp_bridge::handle(&router, req).await,
            Err(e) => {
                // JSON-RPC parse error (-32700), id unknown → null.
                let body = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                });
                write_half.write_all(body.to_string().as_bytes()).await?;
                write_half.write_all(b"\n").await?;
                continue;
            }
        };
        let encoded = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
        write_half.write_all(encoded.as_bytes()).await?;
        write_half.write_all(b"\n").await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::{Database, Notifier};
    use crate::services::approval::AlwaysApprove;
    use std::sync::Arc;
    use tokio::io::AsyncReadExt;

    fn test_router() -> ToolRouter {
        ToolRouter::with_defaults(
            Database::open_in_memory().expect("db"),
            Notifier::default(),
            Arc::new(AlwaysApprove),
        )
    }

    #[tokio::test]
    async fn tcp_client_round_trip() {
        let listener = bind(0).await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(serve(listener, test_router()));

        let mut client = TcpStream::connect(addr).await.expect("connect");

        // tools/list request as a single JSON line.
        let req = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n";
        client.write_all(req).await.expect("write");

        // Read one response line back.
        let mut buf = vec![0u8; 4096];
        let n = client.read(&mut buf).await.expect("read");
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("\"result\""));
        assert!(resp.contains("ADD_TASK"));
        assert!(resp.ends_with('\n'));
    }

    #[tokio::test]
    async fn tcp_malformed_line_returns_parse_error() {
        let listener = bind(0).await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(serve(listener, test_router()));

        let mut client = TcpStream::connect(addr).await.expect("connect");
        client.write_all(b"not json at all\n").await.expect("write");

        let mut buf = vec![0u8; 1024];
        let n = client.read(&mut buf).await.expect("read");
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("-32700"));
    }
}
