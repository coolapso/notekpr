use coolpsonotes_core::doc::{EntryKind, NoteDoc};
use coolpsonotes_core::engine::{VaultMeta, VaultReplica};
use coolpsonotes_core::keys::{VaultKey, VaultKeyring};
use coolpsonotes_core::protocol::*;

use crate::*;

fn replica() -> VaultReplica {
    let mut ring = VaultKeyring::new("v1");
    ring.insert(1, VaultKey::generate());
    VaultReplica::new("v1", Role::Editor, ring, VaultMeta::named("Test"))
}

/// A local edit of `note`: applied to the replica's document and queued.
fn edit(r: &mut VaultReplica, note: &str, text: &str) {
    let n = r.notes.entry(note.to_string()).or_default();
    let cur = n.text();
    let u = n.set_text(&format!("{cur}{text}")).unwrap();
    r.local_change(note, u).unwrap();
}

fn ack_all(o: &mut Outbox, r: &mut VaultReplica, plan: &Plan, first_seq: i64) {
    let results: Vec<PushResult> = plan
        .ids()
        .into_iter()
        .enumerate()
        .map(|(i, id)| PushResult {
            client_update_id: id,
            seq: first_seq + i as i64,
        })
        .collect();
    o.on_ack(r, &results, true);
}

#[test]
fn unsent_updates_of_one_note_are_merged_into_one() {
    let (mut r, mut o) = (replica(), Outbox::default());
    for i in 0..50 {
        edit(&mut r, "n1", &format!("line {i}\n"));
    }
    edit(&mut r, "n2", "other");
    let plan = o.plan(&mut r, &Limits::default());
    assert_eq!(r.pending.len(), 2, "one update per note");
    assert_eq!(plan.pushes.len(), 1);
    // The merged update carries all the text.
    let n = NoteDoc::new();
    let p = r.pending.iter().find(|p| p.object_id == "n1").unwrap();
    n.apply_update(&p.update).unwrap();
    assert!(n.text().starts_with("line 0\n") && n.text().ends_with("line 49\n"));
}

#[test]
fn merging_never_undoes_a_split_paste() {
    let (mut r, mut o) = (replica(), Outbox::default());
    let limits = Limits::new(64 * 1024);
    // Parts of one big paste, each just under the edit budget.
    for _ in 0..6 {
        edit(&mut r, "n1", &"z".repeat(limits.edit_budget() - 200));
    }
    let plan = o.plan(&mut r, &limits);
    assert!(plan.held.is_empty(), "nothing became too large");
    assert_eq!(plan.ids().len(), 6);
}

#[test]
fn sent_updates_are_not_merged_or_sent_twice() {
    let (mut r, mut o) = (replica(), Outbox::default());
    edit(&mut r, "n1", "a");
    let first = o.plan(&mut r, &Limits::default());
    assert_eq!(first.ids().len(), 1);
    edit(&mut r, "n1", "b");
    let second = o.plan(&mut r, &Limits::default());
    // The first is in flight: only the new one goes out, both stay queued.
    assert_eq!(second.ids().len(), 1);
    assert_ne!(second.ids(), first.ids());
    assert_eq!(r.pending.len(), 2);
    // After a reconnect everything unacknowledged goes again, same ids.
    o.connection_reset();
    let again = o.plan(&mut r, &Limits::default());
    let mut ids = again.ids();
    ids.sort();
    let mut want = [first.ids(), second.ids()].concat();
    want.sort();
    assert_eq!(ids, want);
    ack_all(&mut o, &mut r, &again, 1);
    assert!(r.pending.is_empty());
}

#[test]
fn not_sent_updates_merge_again() {
    let (mut r, mut o) = (replica(), Outbox::default());
    edit(&mut r, "n1", "a");
    let p = o.plan(&mut r, &Limits::default());
    o.not_sent(&p.ids()); // offline: the socket was closed
    edit(&mut r, "n1", "b");
    o.plan(&mut r, &Limits::default());
    assert_eq!(r.pending.len(), 1);
}

#[test]
fn pushes_respect_count_and_byte_limits() {
    let (mut r, mut o) = (replica(), Outbox::default());
    for i in 0..30 {
        edit(&mut r, &format!("n{i}"), &"x".repeat(3000));
    }
    let limits = Limits {
        max_update_bytes: 10_000,
        max_push_count: 4,
    };
    let plan = o.plan(&mut r, &limits);
    assert_eq!(plan.ids().len(), 30);
    for push in &plan.pushes {
        assert!(push.len() <= 4);
        let bytes: usize = push.iter().map(|u| u.payload.len()).sum();
        assert!(bytes <= 10_000 || push.len() == 1);
    }
}

#[test]
fn an_oversized_update_is_held_and_never_blocks_the_rest() {
    let (mut r, mut o) = (replica(), Outbox::default());
    edit(&mut r, "big", &"y".repeat(50_000));
    edit(&mut r, "small", "hi");
    let plan = o.plan(&mut r, &Limits::new(10_000));
    assert_eq!(plan.held.len(), 1);
    assert_eq!(plan.held[0].object_id, "big");
    assert_eq!(plan.ids().len(), 1);
    assert_eq!(o.held_count(), 1);
    assert_eq!(o.unsent(&r), 2);
    // Reported once only.
    o.connection_reset();
    assert!(o.plan(&mut r, &Limits::new(10_000)).held.is_empty());
    // The administrator raised the limit: it goes out.
    o.connection_reset();
    let later = o.plan(&mut r, &Limits::new(100_000));
    assert_eq!(later.released, 1);
    assert!(!later.ids().is_empty());
    assert_eq!(o.held_count(), 0);
}

#[test]
fn refusals_hold_drop_or_reload_one_update_each() {
    let (mut r, mut o) = (replica(), Outbox::default());
    edit(&mut r, "a", "1");
    edit(&mut r, "b", "2");
    edit(&mut r, "c", "3");
    edit(&mut r, "d", "4");
    let plan = o.plan(&mut r, &Limits::default());
    let id = |obj: &str| {
        r.pending
            .iter()
            .find(|p| p.object_id == obj)
            .unwrap()
            .client_update_id
            .clone()
    };
    let rejected = vec![
        RejectedUpdate {
            client_update_id: id("a"),
            object_id: "a".into(),
            code: REJECT_TOO_LARGE.into(),
            message: String::new(),
        },
        RejectedUpdate {
            client_update_id: id("b"),
            object_id: "b".into(),
            code: REJECT_PURGED.into(),
            message: String::new(),
        },
        RejectedUpdate {
            client_update_id: id("c"),
            object_id: "c".into(),
            code: REJECT_STALE_KEY.into(),
            message: String::new(),
        },
    ];
    let ok = PushResult {
        client_update_id: id("d"),
        seq: 1,
    };
    let reaction = o.on_rejected(&mut r, &rejected, &Limits::default());
    o.on_ack(&mut r, &[ok], true);
    assert!(reaction.reload_keys);
    assert_eq!(reaction.purged, vec!["b".to_string()]);
    assert_eq!(reaction.held.len(), 1);
    assert_eq!(o.held.len(), 1, "a is kept locally");
    let left: Vec<&str> = r.pending.iter().map(|p| p.object_id.as_str()).collect();
    assert_eq!(left, vec!["c"], "c waits for keys, b is gone, d is stored");
    // c is no longer in flight: it is planned again (with new keys); a,
    // refused as too large under these limits, stays held until they grow.
    assert_eq!(o.plan(&mut r, &Limits::default()).ids().len(), 1);
    assert_eq!(o.held_count(), 1);
    o.connection_reset();
    let bigger = Limits::new(Limits::DEFAULT_MAX_UPDATE * 2);
    assert_eq!(o.plan(&mut r, &bigger).released, 1);
    drop(plan);
}

#[test]
fn a_batch_an_older_server_refused_is_retried_one_by_one() {
    let (mut r, mut o) = (replica(), Outbox::default());
    edit(&mut r, "a", "1");
    edit(&mut r, "b", "2");
    let plan = o.plan(&mut r, &Limits::default());
    assert_eq!(plan.pushes.len(), 1);
    let reaction = o.on_batch_refused(&mut r, &plan.ids(), true, &Limits::default());
    assert!(reaction.resend);
    let next = o.plan(&mut r, &Limits::default());
    assert_eq!(next.pushes.len(), 2, "one push per update");
    // The single refused one is held, the other goes through.
    let bad = next.pushes[0][0].client_update_id.clone();
    o.on_batch_refused(&mut r, &[bad], true, &Limits::default());
    assert_eq!(o.held_count(), 1);
}

#[test]
fn read_only_replicas_send_nothing() {
    let mut r = replica();
    edit(&mut r, "n1", "x");
    r.role = Role::Viewer;
    assert!(
        Outbox::default()
            .plan(&mut r, &Limits::default())
            .is_empty()
    );
}

#[test]
fn a_rollback_requeues_the_whole_state() {
    let mut r = replica();
    r.notes.insert("n1".into(), NoteDoc::new());
    r.seq = 50;
    r.max_head_seen = 50;
    assert_eq!(recover_from_rollback(&mut r, 20), 2, "index and the note");
    assert_eq!((r.seq, r.max_head_seen), (20, 20));
    assert_eq!(r.pending.len(), 2);
}

#[test]
fn stored_copies_keep_what_cannot_be_encrypted_now() {
    let (mut r, o) = (replica(), Outbox::default());
    edit(&mut r, "n1", "x");
    let first = stored_copies(&r, &o, &[]).unwrap();
    assert_eq!(first.len(), 1);
    // The key is gone (being reloaded): the stored copy survives.
    r.keyring = VaultKeyring::new("v1");
    let kept = stored_copies(&r, &o, &first).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].payload, first[0].payload);
    assert!(
        stored_copies(&r, &o, &[]).is_none(),
        "never replace with nothing"
    );
}

#[test]
fn snapshots_wait_for_confirmation_and_catch_up() {
    let (mut r, mut o) = (replica(), Outbox::default());
    edit(&mut r, "n1", "x");
    r.seq = 3;
    assert!(
        snapshot_now(&r, &o, "n1", None).is_none(),
        "unconfirmed change"
    );
    let plan = o.plan(&mut r, &Limits::default());
    ack_all(&mut o, &mut r, &plan, 4);
    let s = snapshot_now(&r, &o, "n1", None).unwrap();
    assert_eq!(s.covers_seq, 4);
    assert_eq!(snapshot_now(&r, &o, "n1", Some(9)).unwrap().covers_seq, 9);
    assert!(snapshot_now(&r, &o, "missing", None).is_none());
    r.seq = 0;
    assert!(snapshot_now(&r, &o, "n1", None).is_none(), "not caught up");
}

#[test]
fn a_new_note_waits_for_its_key_then_goes_out_under_it() {
    let (mut r, mut o) = (replica(), keyed());
    assert!(o.new_document_key(&mut r, "n1", "Trip"));
    assert!(!o.new_document_key(&mut r, "n1", "Trip"), "only once");
    edit(&mut r, "n1", "pack");
    edit(&mut r, "n2", "other");
    let plan = o.plan(&mut r, &Limits::default());
    assert_eq!(plan.register.len(), 1);
    assert_eq!(plan.register[0].object_id, "n1");
    assert!(plan.register[0].update.private);
    let objects: Vec<String> = plan
        .pushes
        .iter()
        .flatten()
        .map(|u| u.object_id.clone())
        .collect();
    assert_eq!(objects, vec!["n2".to_string()], "n1 waits for its key");
    assert!(snapshot_now(&r, &o, "n1", Some(5)).is_none());
    o.key_registered("n1");
    let plan = o.plan(&mut r, &Limits::default());
    assert!(plan.register.is_empty());
    let u = &plan.pushes[0][0];
    assert_eq!(u.object_id, "n1");
    let epoch = coolpsonotes_core::crypto::sym_epoch(&u.payload).unwrap();
    assert!(
        coolpsonotes_core::keys::is_doc_epoch(epoch),
        "never under the vault key"
    );
}

#[test]
fn a_refused_key_falls_back_to_the_vault_key() {
    let (mut r, mut o) = (replica(), keyed());
    o.new_document_key(&mut r, "n1", "Trip");
    edit(&mut r, "n1", "pack");
    o.key_refused(&mut r, "n1");
    let plan = o.plan(&mut r, &Limits::default());
    let u = &plan.pushes[0][0];
    let epoch = coolpsonotes_core::crypto::sym_epoch(&u.payload).unwrap();
    assert!(!coolpsonotes_core::keys::is_doc_epoch(epoch));
}

#[test]
fn unregistered_keys_survive_a_reload() {
    let (mut r, mut o) = (replica(), keyed());
    o.new_document_key(&mut r, "n1", "Trip");
    let stored = serde_json_roundtrip(&o);
    let mut fresh = replica();
    fresh.keyring = r.keyring.clone();
    let mut o2 = stored;
    o2.restore_new_keys(&mut fresh);
    assert_eq!(
        fresh.doc_keys["n1"].current().unwrap().1.0,
        r.doc_keys["n1"].current().unwrap().1.0
    );
    assert_eq!(o2.new_keys.len(), 1);
}

#[test]
fn a_reload_recognises_keys_the_server_already_has() {
    let (mut r, mut o) = (replica(), keyed());
    o.new_document_key(&mut r, "n1", "Trip");
    o.new_document_key(&mut r, "n2", "Other");
    let ring = r.doc_keys["n1"].clone();
    // The server answered n1's registration but the answer was lost; the
    // client reloads its keys from the server.
    r.doc_keys.clear();
    r.doc_keys.insert("n1".into(), ring);
    o.keys_reloaded(&mut r, true);
    let waiting: Vec<&str> = o.new_keys.iter().map(|k| k.object_id.as_str()).collect();
    assert_eq!(waiting, vec!["n2"]);
    assert!(r.doc_keys.contains_key("n2"), "n2's key put back");
}

#[test]
fn a_key_wrapped_under_an_old_vault_key_is_replaced() {
    let (mut r, mut o) = (replica(), keyed());
    r.index.create("n1", "Trip.md", EntryKind::Note, 1);
    o.new_document_key(&mut r, "n1", "Trip");
    let old = r.doc_keys["n1"].current().unwrap().1.0;
    let next = VaultKey::generate();
    r.keyring.insert(2, next);
    o.key_stale(&mut r, "n1");
    assert_eq!(o.new_keys.len(), 1);
    let env = &o.new_keys[0].update.vault_envelope;
    assert_eq!(coolpsonotes_core::crypto::sym_epoch(env).unwrap(), 2);
    assert_ne!(r.doc_keys["n1"].current().unwrap().1.0, old);
}

#[test]
fn servers_without_note_keys_get_none() {
    let (mut r, mut o) = (replica(), Outbox::default());
    assert!(!o.new_document_key(&mut r, "n1", "Trip"));
    edit(&mut r, "old", "text");
    let plan = o.plan(&mut r, &Limits::default());
    ack_all(&mut o, &mut r, &plan, 1);
    assert!(matches!(
        snapshot_action(&r, &o, "old", None),
        Some(SnapshotAction::Snapshot(_))
    ));
}

fn keyed() -> Outbox {
    let mut o = Outbox::default();
    o.note_keys = true;
    o
}

fn serde_json_roundtrip(o: &Outbox) -> Outbox {
    let v = serde_json::to_vec(o).unwrap();
    serde_json::from_slice(&v).unwrap()
}

#[test]
fn an_older_note_gets_its_own_key_at_its_next_snapshot() {
    let (mut r, mut o) = (replica(), keyed());
    edit(&mut r, "old", "under the vault key");
    let plan = o.plan(&mut r, &Limits::default());
    ack_all(&mut o, &mut r, &plan, 1);
    match snapshot_action(&r, &o, "old", None).unwrap() {
        SnapshotAction::AdoptKey { update, .. } => {
            assert!(update.private && update.snapshot.is_some());
        }
        SnapshotAction::Snapshot(_) => panic!("should move to its own key"),
    }
    match snapshot_action(&r, &o, INDEX_OBJECT_ID, None) {
        Some(SnapshotAction::Snapshot(_)) => {}
        _ => panic!("the index stays under the vault key"),
    }
}
