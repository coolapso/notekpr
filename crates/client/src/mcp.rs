//! Model Context Protocol server over stdio (JSON-RPC 2.0, one message per
//! line), protocol revision 2025-06-18, exposing the same document operations
//! as the gateway as MCP tools. It runs with the profile's identity — ideally
//! an agent that only has the vaults its owner shared — so MCP never bypasses
//! authentication, roles or encryption. stdout carries protocol messages only;
//! all logging goes to stderr.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::audit::AuditLog;
use crate::hub::Hub;
use crate::ops::{self, OpError};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub struct McpServer {
    hub: Arc<Hub>,
    read_only: bool,
    audit: Arc<AuditLog>,
}

const WRITE_TOOLS: &[&str] = &[
    "create_document",
    "update_document",
    "append_to_document",
    "move_document",
    "delete_document",
];

fn tool(
    name: &str,
    title: &str,
    description: &str,
    props: Value,
    required: &[&str],
    read_only: bool,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "required": required, "additionalProperties": false },
        "annotations": { "readOnlyHint": read_only, "destructiveHint": name == "delete_document", "openWorldHint": false },
    })
}

fn all_tools() -> Vec<Value> {
    let vault = json!({ "type": "string", "description": "Vault id or name (see list_vaults)" });
    let path = json!({ "type": "string", "description": "Document path inside the vault, e.g. 'Projects/Plan.md' ('.md' optional)" });
    vec![
        tool(
            "list_vaults",
            "List vaults",
            "List the vaults this identity can access, with role and document count.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "list_documents",
            "List documents",
            "List the Markdown documents of a vault (path, id, title, tags, modified, size).",
            json!({ "vault": vault, "folder": { "type": "string", "description": "Only documents inside this folder" } }),
            &["vault"],
            true,
        ),
        tool(
            "read_document",
            "Read a document",
            "Return the full Markdown source of a document (first content item) and its metadata as JSON (second item, including the content hash for update_document's expected_hash).",
            json!({ "vault": vault, "path": path }),
            &["vault", "path"],
            true,
        ),
        tool(
            "search",
            "Search",
            "Full-text search across accessible vaults. Supports \"phrases\", tag:x, folder:x, title:x and vault:x filters.",
            json!({ "query": { "type": "string" }, "vault": vault, "limit": { "type": "integer", "minimum": 1, "maximum": 200 } }),
            &["query"],
            true,
        ),
        tool(
            "create_document",
            "Create a document",
            "Create a new Markdown document. Fails if the path already exists.",
            json!({ "vault": vault, "path": path, "content": { "type": "string", "description": "Markdown source" } }),
            &["vault", "path", "content"],
            false,
        ),
        tool(
            "update_document",
            "Update a document",
            "Replace the full content of a document (created if missing). The change is merged as a diff, so concurrent \
             edits by people are preserved. Pass expected_hash from read_document to fail instead if it changed meanwhile.",
            json!({ "vault": vault, "path": path, "content": { "type": "string" }, "expected_hash": { "type": "string" } }),
            &["vault", "path", "content"],
            false,
        ),
        tool(
            "append_to_document",
            "Append to a document",
            "Append text to the end of a document (created if missing). Include a leading newline if needed.",
            json!({ "vault": vault, "path": path, "content": { "type": "string" } }),
            &["vault", "path", "content"],
            false,
        ),
        tool(
            "move_document",
            "Move or rename",
            "Move or rename a document or folder. A target ending in '/' keeps the file name.",
            json!({ "vault": vault, "from": { "type": "string" }, "to": { "type": "string" }, "update_links": { "type": "boolean", "description": "Rewrite [[wiki links]] in other notes" } }),
            &["vault", "from", "to"],
            false,
        ),
        tool(
            "delete_document",
            "Delete a document",
            "Move a document (or folder) to the vault's trash. It can be restored by a person.",
            json!({ "vault": vault, "path": path }),
            &["vault", "path"],
            false,
        ),
        tool(
            "get_backlinks",
            "Backlinks",
            "List documents that link to the given document.",
            json!({ "vault": vault, "path": path }),
            &["vault", "path"],
            true,
        ),
        tool(
            "document_history",
            "Document history",
            "List stored revisions of a document (newest first), or return one revision's content when 'revision' is given.",
            json!({ "vault": vault, "path": path, "revision": { "type": "string" } }),
            &["vault", "path"],
            true,
        ),
    ]
}

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn text_result(texts: Vec<String>) -> Value {
    let content: Vec<Value> = texts
        .into_iter()
        .map(|t| json!({ "type": "text", "text": t }))
        .collect();
    json!({ "content": content, "isError": false })
}

fn tool_error(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

fn pretty(v: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing string argument '{name}'"))
}

fn opt<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

fn op_msg(e: OpError) -> String {
    match e {
        OpError::Precondition(m) => format!("precondition failed: {m}"),
        e => e.to_string(),
    }
}

impl McpServer {
    pub fn new(hub: Arc<Hub>, read_only: bool, audit: Arc<AuditLog>) -> Self {
        let read_only = read_only || hub.read_only;
        McpServer {
            hub,
            read_only,
            audit,
        }
    }

    pub fn tools(&self) -> Vec<Value> {
        all_tools()
            .into_iter()
            .filter(|t| !self.read_only || !WRITE_TOOLS.contains(&t["name"].as_str().unwrap_or("")))
            .collect()
    }

    /// Handle one JSON-RPC message; `None` for notifications.
    pub async fn handle(&self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str());
        let Some(method) = method else {
            // A response from the client, or garbage.
            return id.map(|id| rpc_error(&id, -32600, "invalid request"));
        };
        let Some(id) = id else {
            // Notifications (initialized, cancelled, ...) need no answer.
            return None;
        };
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        Some(match method {
            "initialize" => rpc_result(
                &id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "coolpsonotes", "title": "coolpsonotes notes", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Markdown notes from end-to-end encrypted coolpsonotes vaults. Use list_vaults first; \
                                     paths are relative to a vault. Deleting moves documents to the trash.",
                }),
            ),
            "ping" => rpc_result(&id, json!({})),
            "tools/list" => rpc_result(&id, json!({ "tools": self.tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                if !all_tools().iter().any(|t| t["name"] == name) {
                    return Some(rpc_error(&id, -32602, &format!("unknown tool '{name}'")));
                }
                let result = if self.read_only && WRITE_TOOLS.contains(&name) {
                    tool_error("this MCP server is read-only".into())
                } else {
                    match self.call(name, &args).await {
                        Ok(texts) => text_result(texts),
                        Err(msg) => tool_error(msg),
                    }
                };
                rpc_result(&id, result)
            }
            _ => rpc_error(&id, -32601, &format!("method '{method}' not found")),
        })
    }

    /// Run a tool; the result is one or more text content items.
    async fn call(&self, name: &str, args: &Value) -> Result<Vec<String>, String> {
        if name == "read_document" {
            let d = ops::read_document(&self.hub, arg(args, "vault")?, arg(args, "path")?)
                .await
                .map_err(op_msg)?;
            let meta = json!({ "vault": d.info.vault, "path": d.info.path, "id": d.info.id, "hash": d.hash, "tags": d.info.tags, "links": d.links, "modified": d.info.modified });
            // The Markdown source first, untouched; metadata as a separate item.
            return Ok(vec![d.content, pretty(&meta)]);
        }
        self.call_text(name, args).await.map(|t| vec![t])
    }

    async fn call_text(&self, name: &str, args: &Value) -> Result<String, String> {
        let hub = &self.hub;
        match name {
            "list_vaults" => Ok(pretty(&ops::list_vaults(hub).await)),
            "list_documents" => {
                let docs =
                    ops::list_documents(hub, arg(args, "vault")?, opt(args, "folder"), false)
                        .await
                        .map_err(op_msg)?;
                Ok(pretty(&docs))
            }
            "search" => {
                let limit = args
                    .get("limit")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(20)
                    .clamp(1, 200) as usize;
                let r = ops::search(hub, arg(args, "query")?, opt(args, "vault"), limit)
                    .await
                    .map_err(op_msg)?;
                Ok(pretty(&r))
            }
            "create_document" => {
                let out = ops::create_document(
                    hub,
                    arg(args, "vault")?,
                    arg(args, "path")?,
                    arg(args, "content")?,
                )
                .await
                .map_err(op_msg)?;
                self.audit.record(
                    "mcp",
                    "create_document",
                    &out.vault,
                    Some(&out.id),
                    out.bytes,
                );
                Ok(pretty(&out))
            }
            "update_document" => {
                let out = ops::put_document(
                    hub,
                    arg(args, "vault")?,
                    arg(args, "path")?,
                    arg(args, "content")?,
                    opt(args, "expected_hash"),
                )
                .await
                .map_err(op_msg)?;
                if out.changed {
                    self.audit.record(
                        "mcp",
                        "update_document",
                        &out.vault,
                        Some(&out.id),
                        out.bytes,
                    );
                }
                Ok(pretty(&out))
            }
            "append_to_document" => {
                let out = ops::append_document(
                    hub,
                    arg(args, "vault")?,
                    arg(args, "path")?,
                    arg(args, "content")?,
                )
                .await
                .map_err(op_msg)?;
                self.audit.record(
                    "mcp",
                    "append_to_document",
                    &out.vault,
                    Some(&out.id),
                    out.bytes,
                );
                Ok(pretty(&out))
            }
            "move_document" => {
                let update_links = args
                    .get("update_links")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let out = ops::move_document(
                    hub,
                    arg(args, "vault")?,
                    arg(args, "from")?,
                    arg(args, "to")?,
                    update_links,
                )
                .await
                .map_err(op_msg)?;
                self.audit
                    .record("mcp", "move_document", &out.vault, out.id.as_deref(), 0);
                Ok(pretty(&out))
            }
            "delete_document" => {
                let (vault, ids) =
                    ops::delete_document(hub, arg(args, "vault")?, arg(args, "path")?)
                        .await
                        .map_err(op_msg)?;
                for id in &ids {
                    self.audit
                        .record("mcp", "delete_document", &vault, Some(id), 0);
                }
                Ok(pretty(&json!({ "vault": vault, "trashed": ids })))
            }
            "get_backlinks" => {
                let r = ops::backlinks(hub, arg(args, "vault")?, arg(args, "path")?)
                    .await
                    .map_err(op_msg)?;
                Ok(pretty(&r))
            }
            "document_history" => {
                let (v, p) = (arg(args, "vault")?, arg(args, "path")?);
                match opt(args, "revision") {
                    Some(rev) => ops::revision(hub, v, p, rev).await.map_err(op_msg),
                    None => Ok(pretty(&ops::history(hub, v, p).await.map_err(op_msg)?)),
                }
            }
            _ => Err(format!("unknown tool '{name}'")),
        }
    }

    /// Parse and handle one line of input.
    pub async fn handle_line(&self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return Some(rpc_error(&Value::Null, -32700, "parse error").to_string()),
        };
        if msg.is_array() {
            return Some(rpc_error(&Value::Null, -32600, "batches are not supported").to_string());
        }
        self.handle(&msg).await.map(|v| v.to_string())
    }

    /// Serve on stdin/stdout until stdin closes.
    pub async fn serve_stdio(&self) -> anyhow::Result<()> {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        let mut out = tokio::io::stdout();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(resp) = self.handle_line(&line).await {
                out.write_all(resp.as_bytes()).await?;
                out.write_all(b"\n").await?;
                out.flush().await?;
            }
        }
        Ok(())
    }
}
