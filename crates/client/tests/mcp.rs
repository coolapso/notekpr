//! MCP server: JSON-RPC handling in-process.

mod common;

use std::sync::Arc;

use common::*;
use coolpsonotes_client::audit::AuditLog;
use coolpsonotes_client::mcp::{McpServer, PROTOCOL_VERSION};
use coolpsonotes_client::{manage, ops};
use serde_json::{Value, json};

async fn call(s: &McpServer, id: u64, method: &str, params: Value) -> Value {
    s.handle(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
        .await
        .expect("a response")
}

async fn tool(s: &McpServer, id: u64, name: &str, args: Value) -> Value {
    let r = call(
        s,
        id,
        "tools/call",
        json!({ "name": name, "arguments": args }),
    )
    .await;
    r["result"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_tools_read_search_and_update() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Research").await.unwrap();
    let h = hub(&alice).await;
    ops::put_document(
        &h,
        "Research",
        "Rust.md",
        "# Rust\nOwnership and #borrowing\n",
        None,
    )
    .await
    .unwrap();
    ops::put_document(&h, "Research", "Index.md", "Start at [[Rust]]\n", None)
        .await
        .unwrap();
    let audit = Arc::new(AuditLog::open(&alice.data_dir()));
    let s = McpServer::new(h.clone(), false, audit.clone());

    let init = call(&s, 1, "initialize", json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } })).await;
    assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(
        s.handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await
            .is_none()
    );
    assert_eq!(call(&s, 2, "ping", json!({})).await["result"], json!({}));

    let tools = call(&s, 3, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for n in [
        "list_vaults",
        "list_documents",
        "read_document",
        "search",
        "create_document",
        "update_document",
        "append_to_document",
        "move_document",
        "delete_document",
        "get_backlinks",
        "document_history",
    ] {
        assert!(names.contains(&n), "missing tool {n}");
    }
    for t in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(t["inputSchema"]["type"], "object");
    }

    let r = tool(
        &s,
        4,
        "read_document",
        json!({ "vault": "Research", "path": "Rust" }),
    )
    .await;
    assert_eq!(r["isError"], false);
    assert_eq!(r["content"][0]["type"], "text");
    assert_eq!(
        r["content"][0]["text"],
        "# Rust\nOwnership and #borrowing\n"
    );
    let meta: Value = serde_json::from_str(r["content"][1]["text"].as_str().unwrap()).unwrap();
    let hash = meta["hash"].as_str().unwrap().to_string();
    assert_eq!(meta["tags"], json!(["borrowing"]));

    let r = tool(&s, 5, "search", json!({ "query": "tag:borrowing" })).await;
    let hits: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(hits[0]["path"], "Rust.md");

    // Lost-update protection with expected_hash, then a real update.
    let r = tool(
        &s,
        6,
        "update_document",
        json!({ "vault": "Research", "path": "Rust.md", "content": "x", "expected_hash": "bogus" }),
    )
    .await;
    assert_eq!(r["isError"], true);
    let r = tool(&s, 7, "update_document", json!({ "vault": "Research", "path": "Rust.md", "content": "# Rust\nOwnership, #borrowing and lifetimes\n", "expected_hash": hash })).await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        note(&h, "Research", "Rust.md").await.as_deref(),
        Some("# Rust\nOwnership, #borrowing and lifetimes\n")
    );
    let r = tool(
        &s,
        8,
        "append_to_document",
        json!({ "vault": "Research", "path": "Rust.md", "content": "Traits too.\n" }),
    )
    .await;
    assert_eq!(r["isError"], false);
    let r = tool(
        &s,
        9,
        "get_backlinks",
        json!({ "vault": "Research", "path": "Rust.md" }),
    )
    .await;
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Index.md")
    );
    let r = tool(
        &s,
        10,
        "create_document",
        json!({ "vault": "Research", "path": "Rust.md", "content": "dup" }),
    )
    .await;
    assert_eq!(r["isError"], true, "create must not overwrite");
    let r = tool(
        &s,
        11,
        "move_document",
        json!({ "vault": "Research", "from": "Index.md", "to": "Archive/" }),
    )
    .await;
    assert_eq!(r["isError"], false);
    let r = tool(
        &s,
        12,
        "delete_document",
        json!({ "vault": "Research", "path": "Archive/Index.md" }),
    )
    .await;
    assert_eq!(r["isError"], false);
    let r = tool(
        &s,
        13,
        "read_document",
        json!({ "vault": "Nope", "path": "x" }),
    )
    .await;
    assert_eq!(r["isError"], true);

    // Errors: unknown tool and method are protocol errors.
    let r = call(
        &s,
        14,
        "tools/call",
        json!({ "name": "rm_rf", "arguments": {} }),
    )
    .await;
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(
        call(&s, 15, "resources/list", json!({})).await["error"]["code"],
        -32601
    );
    assert!(s.handle_line("{not json").await.unwrap().contains("-32700"));

    // The change reaches the server (pushed by the MCP write).
    let other = hub(&reopen(&srv, "alice", "alice").await).await;
    assert!(
        note(&other, "Research", "Rust.md")
            .await
            .unwrap()
            .contains("Traits too.")
    );

    // Writes were audited.
    let log = std::fs::read_to_string(audit.path()).unwrap();
    assert!(log.contains("\"via\":\"mcp\"") && log.contains("update_document"));

    // Read-only: write tools are not offered and are refused.
    let ro = McpServer::new(h.clone(), true, audit);
    let tools = call(&ro, 1, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"read_document")
            && !names.contains(&"update_document")
            && !names.contains(&"delete_document")
    );
    let r = tool(
        &ro,
        2,
        "update_document",
        json!({ "vault": "Research", "path": "Rust.md", "content": "x" }),
    )
    .await;
    assert_eq!(r["isError"], true);
}
