//! Agent identity + local HTTP gateway: scoped to shared vaults, read-only
//! mode, audit log, and changes visible to the owner.

mod common;

use std::sync::Arc;

use common::*;
use notekpr_client::account::{self, AgentSetupOptions};
use notekpr_client::audit::AuditLog;
use notekpr_client::gateway::{self, Gateway};
use notekpr_client::hub::{Hub, HubOptions};
use notekpr_client::session::Session;
use notekpr_client::{manage, ops};
use notekpr_core::engine::PinStatus;
use notekpr_core::protocol::Role;
use serde_json::Value;

async fn start_gateway(hub: Arc<Hub>, token: &str) -> String {
    let audit = Arc::new(AuditLog::open(&hub.session.data_dir()));
    let gw = Arc::new(Gateway {
        hub,
        token: token.to_string(),
        audit,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(gateway::serve(listener, gw, std::future::pending()));
    format!("http://{addr}/v1")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_gateway_is_scoped_audited_and_syncs() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Agent memory").await.unwrap();
    manage::create_vault(&alice, "Diary").await.unwrap();
    let alice_hub = hub(&alice).await;
    let mem = alice_hub.resolve("Agent memory").await.unwrap();
    let mem_id = mem.lock().await.id().to_string();
    let diary_id = alice_hub
        .resolve("Diary")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
    ops::put_document(
        &alice_hub,
        &mem_id,
        "Facts.md",
        "# Facts\n- the sky is blue\n",
        None,
    )
    .await
    .unwrap();
    ops::put_document(&alice_hub, &diary_id, "Secret.md", "dear diary", None)
        .await
        .unwrap();

    // The owner creates the agent; its secret goes into a 0600 file.
    let created = manage::create_agent(&alice, "Assistant").await.unwrap();
    let secret_file = srv.path("assistant.secret");
    notekpr_client::fsutil::create_private_file(
        &secret_file,
        created.credential.client_secret.as_bytes(),
    )
    .unwrap();

    // `agent setup` on the agent's machine: own key pair, fingerprint printed.
    let agent_dirs = srv.dirs("agent-host");
    let setup = account::agent_setup(
        &agent_dirs,
        "assistant",
        AgentSetupOptions {
            server: srv.url.clone(),
            insecure_http: false,
            client_id: created.credential.client_id.clone(),
            client_secret_file: secret_file.clone(),
        },
    )
    .await
    .unwrap();
    assert!(setup.created_keys);
    // Running setup again is idempotent (same key).
    let again = account::agent_setup(
        &agent_dirs,
        "assistant",
        AgentSetupOptions {
            server: srv.url.clone(),
            insecure_http: false,
            client_id: created.credential.client_id.clone(),
            client_secret_file: secret_file.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(again.fingerprint, setup.fingerprint);

    // The owner compares the fingerprint, then shares exactly one vault.
    let fp = manage::agent_fingerprint(&alice, &setup.agent_id)
        .await
        .unwrap();
    assert_eq!(fp.as_deref(), Some(setup.fingerprint.as_str()));
    let shared = manage::share(&alice, &mem, &setup.agent_id, Role::Editor, false)
        .await
        .unwrap();
    assert_eq!(
        shared.fingerprint.as_deref(),
        Some(setup.fingerprint.as_str())
    );
    assert_eq!(shared.pin, Some(PinStatus::New));

    let agent = Arc::new(Session::open(&agent_dirs, "assistant").unwrap());
    let agent_live = live(hub(&agent).await).await;
    let base = start_gateway(agent_live.hub.clone(), "gw-test-token").await;
    let http = reqwest::Client::new();
    let get = |path: &str| {
        http.get(format!("{base}{path}"))
            .bearer_auth("gw-test-token")
    };

    // Authentication is required.
    let st = http
        .get(format!("{base}/vaults"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 401);
    let st = http
        .get(format!("{base}/vaults"))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 401);

    // Only the shared vault is visible; the private one does not exist for the agent.
    let vaults: Vec<Value> = get("/vaults").send().await.unwrap().json().await.unwrap();
    assert_eq!(vaults.len(), 1);
    assert_eq!(vaults[0]["name"], "Agent memory");
    assert_eq!(vaults[0]["role"], "editor");
    for p in [
        format!("/vaults/{diary_id}/documents"),
        "/vaults/Diary/documents".to_string(),
    ] {
        assert_eq!(get(&p).send().await.unwrap().status().as_u16(), 404);
    }
    // Saved shapes are a person's: an agent has none.
    assert_eq!(get("/shapes").send().await.unwrap().status().as_u16(), 404);
    let st = http
        .post(format!("{base}/shapes"))
        .bearer_auth("gw-test-token")
        .body(r#"{"type":"excalidrawlib","version":2,"libraryItems":[]}"#)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 404);
    let hits: Vec<Value> = get("/search?q=diary")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(hits.is_empty());

    // List and read.
    let docs: Vec<Value> = get("/vaults/Agent%20memory/documents")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["path"], "Facts.md");
    let r = get("/vaults/Agent%20memory/documents/Facts.md")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let etag = r.headers()["etag"].to_str().unwrap().to_string();
    assert_eq!(r.text().await.unwrap(), "# Facts\n- the sky is blue\n");
    let j: Value = get("/vaults/Agent%20memory/documents/Facts?format=json")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(j["content"], "# Facts\n- the sky is blue\n");

    // Lost-update protection, then an update.
    let put = |path: &str| {
        http.put(format!("{base}{path}"))
            .bearer_auth("gw-test-token")
    };
    let st = put("/vaults/Agent%20memory/documents/Facts.md")
        .header("if-match", "\"stale\"")
        .body("x")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 412);
    let new = "# Facts\n- the sky is blue\n- water is wet (agent)\n";
    let r = put("/vaults/Agent%20memory/documents/Facts.md")
        .header("if-match", etag)
        .body(new)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let out: Value = r.json().await.unwrap();
    let doc_id = out["id"].as_str().unwrap().to_string();
    let r = put("/vaults/Agent%20memory/documents/Inbox/Todo.md")
        .body("- [ ] call mom\n")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);

    // The owner sees the agent's edits (written as the agent).
    eventually(10, "alice sees the agent's update", || async {
        alice_hub.sync_vault(&mem_id).await.unwrap();
        note(&alice_hub, &mem_id, "Facts.md").await.as_deref() == Some(new)
            && note(&alice_hub, &mem_id, "Inbox/Todo.md").await.is_some()
    })
    .await;

    // Owner edits reach the gateway live.
    ops::put_document(
        &alice_hub,
        &mem_id,
        "Facts.md",
        &format!("{new}- grass is green (alice)\n"),
        None,
    )
    .await
    .unwrap();
    eventually(10, "gateway serves alice's edit", || async {
        get("/vaults/Agent%20memory/documents/Facts.md")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
            .contains("grass")
    })
    .await;

    // Moves, backlinks, deletes (trash), attachments.
    let r = http
        .post(format!("{base}/vaults/Agent%20memory/move"))
        .bearer_auth("gw-test-token")
        .json(&serde_json::json!({ "from": "Inbox/Todo.md", "to": "Todo.md" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    put("/vaults/Agent%20memory/documents/Index.md")
        .body("see [[Facts]]")
        .send()
        .await
        .unwrap();
    let bl: Vec<Value> = get("/vaults/Agent%20memory/backlinks/Facts.md")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bl.len(), 1);
    assert_eq!(bl[0]["path"], "Index.md");
    let st = http
        .delete(format!("{base}/vaults/Agent%20memory/documents/Todo.md"))
        .bearer_auth("gw-test-token")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 200);
    assert_eq!(
        get("/vaults/Agent%20memory/documents/Todo.md")
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        404
    );
    let r = put("/vaults/Agent%20memory/attachments/img/dot.png")
        .header("content-type", "image/png")
        .body(vec![137u8, 80, 78, 71, 1, 2, 3])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let r = get("/vaults/Agent%20memory/attachments/img/dot.png")
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(
        r.bytes().await.unwrap().as_ref(),
        &[137u8, 80, 78, 71, 1, 2, 3]
    );
    let hist = get("/vaults/Agent%20memory/history/Facts.md")
        .send()
        .await
        .unwrap();
    assert_eq!(hist.status().as_u16(), 200);

    // Comments: on some of a note's text, then a reply.
    let comment = |body: Value| {
        http.post(format!("{base}/vaults/Agent%20memory/comments/Index.md"))
            .bearer_auth("gw-test-token")
            .json(&body)
            .send()
    };
    let r = comment(serde_json::json!({ "on": "Facts", "text": "Is this up to date?" }))
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let sent: Value = r.json().await.unwrap();
    let id = sent["id"].as_str().unwrap().to_string();
    let r = comment(serde_json::json!({ "id": &id[..8], "action": "reply", "text": "Yes" }))
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let bad = comment(serde_json::json!({ "on": "not in the note", "text": "?" }))
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 400);
    let list: Value = get("/vaults/Agent%20memory/comments/Index.md")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let t = &list["threads"][0];
    assert_eq!(t["quote"], "Facts");
    assert_eq!(t["comments"][1]["body"], "Yes");

    // Mentions: the agent mentions Alice; her answer mentions the agent,
    // which finds it among its mentions and marks it read.
    let r = comment(
        serde_json::json!({ "id": &id[..8], "action": "reply", "text": "@Alice, please check" }),
    )
    .await
    .unwrap();
    let sent: Value = r.json().await.unwrap();
    assert_eq!(sent["mentions"][0], alice.user_id.as_str());
    let ah = hub(&alice).await;
    ops::comment_action(
        &ah,
        "Agent memory",
        "Index.md",
        &id[..8],
        notekpr_client::vault::CommentAct::Reply("Done, @Assistant".into()),
    )
    .await
    .unwrap();
    eventually(10, "the mention reaches the agent", || async {
        let l: Vec<Value> = get("/mentions?unread=true")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        l.len() == 1 && l[0]["body"] == "Done, @Assistant" && l[0]["path"] == "Index.md"
    })
    .await;
    let r: Value = http
        .post(format!("{base}/mentions/read"))
        .bearer_auth("gw-test-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["marked"], 1);
    let l: Vec<Value> = get("/mentions?unread=true")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(l.is_empty());

    // Every write is audited without content.
    let audit = std::fs::read_to_string(agent.data_dir().join("audit.jsonl")).unwrap();
    let lines: Vec<Value> = audit
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(lines.iter().any(|l| l["op"] == "put_document"
        && l["document"] == doc_id.as_str()
        && l["vault"] == mem_id.as_str()));
    assert!(lines.iter().any(|l| l["op"] == "move_document"));
    assert!(lines.iter().any(|l| l["op"] == "trash_document"));
    assert!(lines.iter().any(|l| l["op"] == "put_attachment"));
    assert!(lines.iter().any(|l| l["op"] == "comment"));
    assert!(!audit.contains("water is wet") && !audit.contains("Facts"));
    assert!(!audit.contains("up to date") && !audit.contains("Index"));

    // A read-only gateway refuses every write but still reads.
    let ro_hub = Hub::start(
        agent.clone(),
        HubOptions {
            read_only: true,
            offline_ok: true,
            catch_up: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let ro = start_gateway(ro_hub, "ro-token").await;
    let st = http
        .put(format!("{ro}/vaults/Agent%20memory/documents/Facts.md"))
        .bearer_auth("ro-token")
        .body("nope")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 403);
    let st = http
        .delete(format!("{ro}/vaults/Agent%20memory/documents/Facts.md"))
        .bearer_auth("ro-token")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 403);
    let st = http
        .get(format!("{ro}/vaults/Agent%20memory/documents/Facts.md"))
        .bearer_auth("ro-token")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 200);

    // A vault allow-list hides everything else even for the owner's own gateway.
    let restricted = Hub::start(
        alice.clone(),
        HubOptions {
            vaults: vec!["Diary".into()],
            offline_ok: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let rbase = start_gateway(restricted, "r-token").await;
    let vaults: Vec<Value> = http
        .get(format!("{rbase}/vaults"))
        .bearer_auth("r-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(vaults.len(), 1);
    assert_eq!(vaults[0]["name"], "Diary");
    let st = http
        .get(format!("{rbase}/vaults/{mem_id}/documents"))
        .bearer_auth("r-token")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 404);
    // Her saved shapes (Personal storage) aren't among them either.
    let st = http
        .get(format!("{rbase}/shapes"))
        .bearer_auth("r-token")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 404);

    agent_live.stop().await;
}

/// A person's gateway reads their saved shapes as Excalidraw's library
/// file, and adds those of one (once each).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gateway_reads_and_adds_saved_shapes() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let h = hub(&alice).await;
    let base = start_gateway(h.clone(), "t").await;
    let http = reqwest::Client::new();
    let get = || async {
        let r = http
            .get(format!("{base}/shapes"))
            .bearer_auth("t")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 200);
        assert_eq!(
            r.headers()[reqwest::header::CONTENT_TYPE],
            "application/vnd.excalidrawlib+json"
        );
        r.json::<Value>().await.unwrap()
    };
    let post = |body: String| {
        http.post(format!("{base}/shapes"))
            .bearer_auth("t")
            .body(body)
            .send()
    };
    let empty = get().await;
    assert_eq!(empty["type"], "excalidrawlib");
    assert_eq!(empty["libraryItems"].as_array().unwrap().len(), 0);

    let library = serde_json::json!({
        "type": "excalidrawlib", "version": 2,
        "libraryItems": [
            {"id": "bed", "status": "unpublished", "created": 1, "name": "Raised bed",
             "elements": [{"id": "r", "type": "rectangle", "width": 80, "height": 40, "versionNonce": 3}]},
            {"id": "stone", "status": "unpublished", "created": 2,
             "elements": [{"id": "e", "type": "ellipse", "width": 20, "height": 20, "versionNonce": 4}]},
        ],
    })
    .to_string();
    let r = post(library.clone()).await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let done: Value = r.json().await.unwrap();
    assert_eq!(
        (done["added"].as_u64(), done["already"].as_u64()),
        (Some(2), Some(0))
    );
    // Again: saved already.
    let done: Value = post(library).await.unwrap().json().await.unwrap();
    assert_eq!(
        (done["added"].as_u64(), done["already"].as_u64()),
        (Some(0), Some(2))
    );
    let file = get().await;
    let items = file["libraryItems"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(
        (items[0]["id"].as_str(), items[1]["name"].as_str()),
        (Some("stone"), Some("Raised bed"))
    );
    assert!(ops::library(&h).await.iter().any(|i| i.id == "bed"));

    // What isn't a library is refused.
    let r = post("# a note".into()).await.unwrap();
    assert_eq!(r.status().as_u16(), 400);
    let err: Value = r.json().await.unwrap();
    assert_eq!(err["error"], "bad_request");

    // Recorded in the audit log, without what they are.
    let audit = std::fs::read_to_string(alice.data_dir().join("audit.jsonl")).unwrap();
    assert_eq!(
        audit
            .lines()
            .filter(|l| l.contains("\"add_shapes\""))
            .count(),
        1,
        "{audit}"
    );
    assert!(!audit.contains("Raised bed") && !audit.contains("rectangle"));

    // A read-only gateway reads them, and adds none.
    let ro = Hub::start(
        alice.clone(),
        HubOptions {
            read_only: true,
            offline_ok: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let rbase = start_gateway(ro, "ro").await;
    let r = http
        .post(format!("{rbase}/shapes"))
        .bearer_auth("ro")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
    let r = http
        .get(format!("{rbase}/shapes"))
        .bearer_auth("ro")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);

    // Limited to her quick notes, it serves them too.
    let quick = Hub::start(
        alice.clone(),
        HubOptions {
            vaults: vec!["Quick notes".into()],
            offline_ok: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let qbase = start_gateway(quick, "q").await;
    let file: Value = http
        .get(format!("{qbase}/shapes"))
        .bearer_auth("q")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(file["libraryItems"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gateway_streams_change_events() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Inbox").await.unwrap();
    let writer = hub(&alice).await;
    let watcher = live(hub(&alice).await).await;
    let base = start_gateway(watcher.hub.clone(), "t").await;
    let mut resp = reqwest::Client::new()
        .get(format!("{base}/events"))
        .bearer_auth("t")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    ops::put_document(&writer, "Inbox", "Hello.md", "hi", None)
        .await
        .unwrap();
    let mut seen = String::new();
    let found = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(chunk) = resp.chunk().await.unwrap() {
            seen.push_str(&String::from_utf8_lossy(&chunk));
            if seen.contains("document_changed") && seen.contains("Hello.md") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(found, "no change event in: {seen}");
    assert!(!seen.contains("\"hi\""), "events must not carry content");
    watcher.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rotated_secret_that_cannot_be_saved_revokes_nothing() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let created = manage::create_agent(&alice, "helper").await.unwrap();
    let id = created.agent.id.clone();
    let live = |agents: Vec<notekpr_core::protocol::AgentInfo>| {
        agents
            .into_iter()
            .find(|a| a.id == id)
            .unwrap()
            .credentials
            .into_iter()
            .filter(|c| c.revoked_at.is_none())
            .map(|c| c.client_id)
            .collect::<Vec<_>>()
    };
    let before = live(manage::agents(&alice).await.unwrap());
    assert_eq!(before.len(), 1);

    let err = manage::rotate_agent_secret(&alice, &id, true, |_| anyhow::bail!("disk full"))
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("old credentials still work"),
        "{err:#}"
    );
    assert_eq!(live(manage::agents(&alice).await.unwrap()), before);

    let new = manage::rotate_agent_secret(&alice, &id, true, |_| Ok(()))
        .await
        .unwrap();
    assert_eq!(
        live(manage::agents(&alice).await.unwrap()),
        vec![new.client_id]
    );
}

/// Agents read a drawing's words and file, and draw: Excalidraw's JSON
/// sent to a `.drawing` path makes a drawing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_read_and_make_drawings() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Board").await.unwrap();
    let h = hub(&alice).await;
    let base = start_gateway(h.clone(), "t").await;
    let http = reqwest::Client::new();
    let drawing = serde_json::json!({
        "type": "excalidraw", "version": 2,
        "elements": [
            {"id": "r", "type": "rectangle", "x": 0, "y": 0, "width": 100, "height": 60,
             "boundElements": [{"id": "l", "type": "text"}]},
            {"id": "l", "type": "text", "x": 10, "y": 20, "width": 80, "height": 20,
             "text": "Kitchen", "originalText": "Kitchen", "containerId": "r"},
            {"id": "t", "type": "text", "x": 0, "y": 100, "width": 80, "height": 20,
             "text": "Plan for the house", "originalText": "Plan for the house"},
        ],
    });
    let r = http
        .put(format!("{base}/vaults/Board/documents/House.drawing"))
        .bearer_auth("t")
        .body(drawing.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 201, "{}", r.text().await.unwrap());
    // Listed with the notes, as a drawing.
    let docs: Vec<Value> = http
        .get(format!("{base}/vaults/Board/documents"))
        .bearer_auth("t")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (docs[0]["kind"].as_str(), docs[0]["title"].as_str()),
        (Some("drawing"), Some("House"))
    );
    // Its words, and its file.
    let words = http
        .get(format!("{base}/vaults/Board/documents/House?format=text"))
        .bearer_auth("t")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        words.contains("Kitchen") && words.contains("Plan for the house"),
        "{words}"
    );
    let file: Value = http
        .get(format!("{base}/vaults/Board/documents/House.drawing"))
        .bearer_auth("t")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(file["type"], "notekpr-drawing");
    assert_eq!(file["elements"].as_array().unwrap().len(), 3);
}
