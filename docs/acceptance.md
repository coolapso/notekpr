# Release acceptance criteria (spec §21) and how they are verified

Legend: **S** = `cargo test -p coolpsonotes-server` (`crates/server/tests/api.rs`),
**C** = `cargo test -p coolpsonotes-core`, **E** = browser suite `tests/e2e/run.sh`,
**D** = desktop client tests `cargo test -p coolpsonotes-client` (`crates/client/tests`),
**X** = browser ↔ CLI test `tests/e2e/cross_client.py`.

| # | Criterion | Verified by |
|---|---|---|
| 1 | Two independently authenticated users can create private vaults | S `private_vaults_are_isolated_and_sharing_works`; E `test_collab` (two browsers) |
| 2 | Neither user can access the other's private content | S same test: list/get/keys/changes/push on another user's vault all return 404 |
| 3 | One user can share a vault with the other | S `private_vaults_are_isolated_and_sharing_works`; E `test_collab` (Share dialog) |
| 4 | Both can collaboratively edit shared notes | S concurrent edits merge; E `test_collab` both editors converge, presence and cursors shown |
| 5 | Changes synchronize across connected clients | S `websocket_live_sync_acks_and_presence`; E `test_collab`; D `shared_vault_reaches_running_sync_live`; X browser ↔ CLI mirror both ways |
| 6 | Clients can edit offline and synchronize later | E `test_features` (server stopped, edits queued, restart, viewer receives them); D `offline_edits_reconcile_and_pending_queue_survives_restart` |
| 7 | No edits silently lost during concurrent editing or reconnection | C `note_concurrent_edits_merge`; S `pushes_are_idempotent_and_snapshots_compact` (idempotent retries); E offline test; D `mirror_syncs_both_ways_and_merges_external_edits` (stale external-editor save preserves both edits), `concurrent_offline_creation_gives_two_files_not_a_lost_note`; E `test_recovered_copy`, C `unsent_edits_of_a_note_deleted_elsewhere_are_kept`, `unsent_edits_refused_because_their_note_is_gone_are_kept` (unsent edits of a note someone deletes for good meanwhile are kept as a "(recovered)" copy — whether the deletion arrives live, with the catch-up after reconnecting, or as a refused upload) |
| 8 | The server cannot decrypt note contents | Architecture: the server holds no vault key (see security.md). S checks stored payloads and vault metadata contain no plaintext. Nor can it add members, plant vault keys or slip in content: D `a_member_the_owner_never_added_gets_no_keys`, `a_vault_key_the_owner_never_made_is_refused`, `a_rolled_back_membership_log_is_noticed`, `content_a_server_slips_in_is_refused`; S `membership_changes_carry_owner_signed_log_entries`, `signed_content_is_required_once_the_log_says_so`; E `test_collab` (signed log and changes) |
| 9 | Document bodies, attachments and sensitive filenames encrypted before upload | Paths/names live in the encrypted index object; S asserts ciphertext only; attachments test decrypts client-side |
| 10 | A revoked collaborator cannot access future content after key rotation | S `viewers_are_read_only_and_revocation_rotates_keys`: removed member gets 404 and their old keys cannot decrypt epoch-2 content; D `revocation_rotates_keys_and_keeps_local_copy_read_only`, `removing_a_member_renews_every_notes_key` (every note's own key moves on in the same rotation) |
| 11 | Device enrollment and recovery work without server-held plaintext keys | S `device_enrollment_and_recovery`; E `test_devices_and_recovery` (code comparison, approval, recovery-key unlock) |
| 12 | The web frontend can edit encrypted vaults | E all tests |
| 13 | An authorized desktop client can read and edit Markdown locally | D `mirror_syncs_both_ways_and_merges_external_edits`; X files edited on disk reach the browser and vice versa |
| 14 | An authorized agent client can read and update permitted documents without exposing plaintext to the server | S `agents_are_scoped_to_shared_vaults`; D `agent_gateway_is_scoped_audited_and_syncs`, `gateway_streams_change_events`; agents can also use the CLI and a Markdown folder ([agents.md §3](agents.md#3-how-an-agent-works-with-notes)) |
| 15 | Vaults can be exported in portable Markdown | Web: vault settings → Export (zip of `.md` + attachments); D `export_import_round_trip_preserves_paths_and_content`, `without_the_server_a_device_exports_and_recovers_from_its_local_copies` (offline export and recover) |
| 16 | Encrypted backups can be restored successfully | S `backup_and_restore_roundtrip` (online backup, restore into a new data dir, original keys decrypt notes and attachments) |
| 17 | Runs without proprietary cloud services or paid licenses | Single self-hosted binary + SQLite; any OIDC provider (verified with Dex); AGPL-3.0 |
| 18 | A new family member can start without writing code or scripts | E `test_onboarding`: sign in, one click to create keys, save recovery key, welcome note with instructions; toolbar for formatting |
| §6.2 | Selected notes can be shared without granting vault access; recipients get no unrelated content, metadata, attachments or keys | S `individual_note_sharing` (every other vault endpoint → 404, only document-key payloads, no pre-share history, live single-note subscription, stale-key refusal, viewer/editor, revoke + key rotation, purge), `notes_have_own_keys_and_recipients_never_get_the_history_before_sharing` (a note's own private key, then a fresh unlinked one when sharing starts: no earlier payloads or chain links reach the recipient); D `single_note_sharing_end_to_end`, `a_new_note_is_never_under_the_vault_key_and_sharing_starts_a_fresh_key`, `an_older_note_moves_to_its_own_key_at_its_next_snapshot`, `a_note_key_the_owner_never_gave_is_refused`; S `note_key_grants_carry_the_owners_signature`; E `test_note_sharing` (share dialog, recipient view, live edits both ways, presence, rename, reload, revoke) |

## Manual checks performed

* OIDC login against a real provider (Dex in Docker): discovery, PKCE,
  code exchange, ID token verification, automatic provisioning, redirect back
  into the app. No personal data in the server logs.
* Release WebAssembly bundle: ≈3.3 MB as built by `cargo` (≈740 KB with
  brotli), ≈2.7 MB after `wasm-opt -Oz`, which `scripts/build-web.sh` runs when
  binaryen is installed — always in the Docker image.
* Phone-width layout (390 px): drawer sidebar, compact header, side panel only
  on demand.
