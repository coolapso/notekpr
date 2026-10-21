//! Whatever happens between edits, pushes, acknowledgements, lost
//! connections and merges, everything typed ends up on the server exactly
//! once per update id, and the server's copy equals the local text.

use std::collections::{BTreeMap, BTreeSet};

use notekpr_core::crypto::Purpose;
use notekpr_core::doc::NoteDoc;
use notekpr_core::engine::{VaultMeta, VaultReplica};
use notekpr_core::keys::{VaultKey, VaultKeyring};
use notekpr_core::protocol::{PushResult, Role};
use notekpr_sync::{Limits, Outbox};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    Edit(usize, String),
    Plan,
    /// Acknowledge the first n updates of the last plan.
    Ack(usize),
    /// The connection dropped.
    Reset,
    /// The last plan could not be sent at all (socket closed).
    NotSent,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..3usize, "[a-z ]{1,8}").prop_map(|(n, t)| Op::Edit(n, t)),
        2 => Just(Op::Plan),
        2 => (0..6usize).prop_map(Op::Ack),
        1 => Just(Op::Reset),
        1 => Just(Op::NotSent),
    ]
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn nothing_typed_is_lost(ops in prop::collection::vec(op(), 1..60)) {
        let mut ring = VaultKeyring::new("v");
        ring.insert(1, VaultKey::generate());
        let mut r = VaultReplica::new("v", Role::Editor, ring, VaultMeta::named("t"));
        let mut o = Outbox::default();
        let limits = Limits::default();
        // The server: stored updates by id, in log order.
        let mut stored: BTreeMap<String, (String, Vec<u8>)> = BTreeMap::new();
        let mut log: Vec<String> = Vec::new();
        let mut last: Vec<(String, String, Vec<u8>)> = Vec::new();
        let mut seq = 0;
        let store = |id: &str, obj: &str, ct: &[u8], seq: &mut i64,
                         stored: &mut BTreeMap<String, (String, Vec<u8>)>, log: &mut Vec<String>| {
            if !stored.contains_key(id) {
                stored.insert(id.to_string(), (obj.to_string(), ct.to_vec()));
                log.push(id.to_string());
            }
            *seq += 1;
            PushResult { client_update_id: id.to_string(), seq: *seq }
        };
        for op in ops {
            match op {
                Op::Edit(n, t) => {
                    let id = format!("n{n}");
                    let d = r.notes.entry(id.clone()).or_default();
                    let cur = d.text();
                    if let Some(u) = d.set_text(&format!("{cur}{t}")) {
                        r.local_change(&id, u).unwrap();
                    }
                }
                Op::Plan => {
                    last = o
                        .plan(&mut r, &limits)
                        .pushes
                        .into_iter()
                        .flatten()
                        .map(|u| (u.client_update_id, u.object_id, u.payload))
                        .collect();
                }
                Op::Ack(k) => {
                    let acked: Vec<PushResult> = last
                        .iter()
                        .take(k)
                        .map(|(id, obj, ct)| store(id, obj, ct, &mut seq, &mut stored, &mut log))
                        .collect();
                    o.on_ack(&mut r, &acked, true);
                    last.drain(..k.min(last.len()));
                }
                Op::Reset => o.connection_reset(),
                Op::NotSent => {
                    let ids: Vec<String> = last.drain(..).map(|(id, _, _)| id).collect();
                    o.not_sent(&ids);
                }
            }
        }
        // Finally everything goes through.
        o.connection_reset();
        let rest: Vec<PushResult> = o
            .plan(&mut r, &limits)
            .pushes
            .into_iter()
            .flatten()
            .map(|u| store(&u.client_update_id, &u.object_id, &u.payload, &mut seq, &mut stored, &mut log))
            .collect();
        o.on_ack(&mut r, &rest, true);
        prop_assert!(r.pending.is_empty());
        // Another client applying the server's log sees the same texts.
        let mut docs: BTreeMap<String, NoteDoc> = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for id in &log {
            prop_assert!(seen.insert(id.clone()));
            let (obj, ct) = &stored[id];
            let plain = r.decrypt_object(Purpose::Update, obj, ct).unwrap();
            docs.entry(obj.clone()).or_default().apply_update(&plain).unwrap();
        }
        for (id, d) in &r.notes {
            let theirs = docs.get(id).map(|d| d.text()).unwrap_or_default();
            prop_assert_eq!(theirs, d.text());
        }
    }
}
