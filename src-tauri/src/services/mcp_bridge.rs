//! MCP (Model Context Protocol) bridge — JSON-RPC 2.0 adapter over the
//! [`ToolRouter`].
//!
//! This deliberately ships only the protocol structs + the adapter logic (no
//! TCP/HTTP server yet): external MCP clients can `tools/list` to discover
//! capabilities and `tools/call` to invoke them, routed straight into the same
//! `ToolRouter` the local LLM uses. Maximum future extensibility, zero coupling.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::services::tools::{ToolCall, ToolRouter};

/// A JSON-RPC 2.0 request.
#[derive(Debug, Deserialize)]
pub struct McpRequest {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

fn jsonrpc_version() -> String {
    "2.0".to_string()
}

#[derive(Debug, Serialize)]
pub struct McpError {
    pub code: i64,
    pub message: String,
}

/// A JSON-RPC 2.0 response (exactly one of `result` / `error` is set).
#[derive(Debug, Serialize)]
pub struct McpResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<McpError>,
}

impl McpResponse {
    fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(McpError {
                code,
                message: message.into(),
            }),
        }
    }
}

/// Adapt one MCP request to the `ToolRouter`. Supports `tools/list` and
/// `tools/call`; unknown methods return a JSON-RPC "method not found" error.
pub async fn handle(router: &ToolRouter, req: McpRequest) -> McpResponse {
    match req.method.as_str() {
        "tools/list" => {
            let tools: Vec<Value> = router
                .descriptors()
                .into_iter()
                .map(|(name, description)| {
                    json!({
                        "name": name,
                        "description": description,
                        "inputSchema": {
                            "type": "object",
                            "properties": { "args": { "type": "string" } }
                        }
                    })
                })
                .collect();
            McpResponse::ok(req.id, json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = req
                .params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                return McpResponse::error(req.id, -32602, "missing tool 'name'");
            }
            let args = req
                .params
                .get("arguments")
                .and_then(|a| a.get("args"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            let call = ToolCall {
                name: name.to_uppercase(),
                raw_args: args,
            };
            match router.dispatch(call).await {
                Ok(outcome) => McpResponse::ok(
                    req.id,
                    json!({
                        "content": [ { "type": "text", "text": outcome.summary } ],
                        "follow_up": outcome.follow_up,
                    }),
                ),
                Err(e) => McpResponse::error(req.id, -32000, e.to_string()),
            }
        }
        other => McpResponse::error(req.id, -32601, format!("method not found: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::{Database, Notifier};
    use crate::services::approval::AlwaysApprove;
    use crate::services::ToolRouter;
    use std::sync::Arc;

    fn router() -> ToolRouter {
        ToolRouter::with_defaults(
            Database::open_in_memory().expect("db"),
            Notifier::default(),
            Arc::new(AlwaysApprove),
        )
    }

    fn req(method: &str, params: Value) -> McpRequest {
        McpRequest {
            jsonrpc: "2.0".to_string(),
            id: json!(1),
            method: method.to_string(),
            params,
        }
    }

    #[tokio::test]
    async fn tools_list_exposes_router() {
        let resp = handle(&router(), req("tools/list", Value::Null)).await;
        let result = resp.result.expect("result");
        let names: Vec<String> = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"ADD_TASK".to_string()));
        assert!(names.contains(&"RECALL".to_string()));
        assert!(names.contains(&"CMD".to_string()));
    }

    #[tokio::test]
    async fn tools_call_routes_to_router() {
        let r = router();
        let params = json!({
            "name": "add_task",
            "arguments": { "args": "Comprar pão|2026-06-20" }
        });
        let resp = handle(&r, req("tools/call", params)).await;
        let result = resp.result.expect("result");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Comprar pão"));
    }

    #[tokio::test]
    async fn unknown_method_errors() {
        let resp = handle(&router(), req("foo/bar", Value::Null)).await;
        assert!(resp.result.is_none());
        assert_eq!(resp.error.expect("err").code, -32601);
    }
}
