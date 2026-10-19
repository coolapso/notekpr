//! REST access to the encrypted log (used by agents, the CLI and catch-up).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use coolpsonotes_core::protocol::{
    ChangesResponse, CreateSnapshotRequest, ObjectState, PushRequest, PushResponse, Snapshot, SnapshotInfo,
};
use serde::Deserialize;

use crate::access;
use crate::auth::Principal;
use crate::error::AppResult;
use crate::state::AppState;
use crate::sync;

#[derive(Deserialize)]
pub struct ChangesQuery {
    #[serde(default)]
    since: i64,
    limit: Option<i64>,
    /// Snapshots are only included on the first page of a catch-up.
    #[serde(default = "yes")]
    snapshots: bool,
}

fn yes() -> bool {
    true
}

pub async fn changes(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
    Query(q): Query<ChangesQuery>,
) -> AppResult<Json<ChangesResponse>> {
    access::member_role(&state.db, &v, &who.user_id).await?;
    let limit = q.limit.unwrap_or(sync::DEFAULT_PAGE);
    Ok(Json(sync::changes_since(&state.db, &v, None, q.since.max(0), limit, q.snapshots, false).await?))
}

/// Changes of one object (for people a single note is shared with).
pub async fn object_changes(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
    Query(q): Query<ChangesQuery>,
) -> AppResult<Json<ChangesResponse>> {
    let (_, member) = access::object_access(&state.db, &v, &o, &who.user_id).await?;
    let limit = q.limit.unwrap_or(sync::DEFAULT_PAGE);
    Ok(Json(sync::changes_since(&state.db, &v, Some(&o), q.since.max(0), limit, q.snapshots, !member).await?))
}

pub async fn object(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
) -> AppResult<Json<ObjectState>> {
    let (_, member) = access::object_access(&state.db, &v, &o, &who.user_id).await?;
    Ok(Json(sync::object_state(&state.db, &v, &o, !member).await?))
}

pub async fn push(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
    Json(req): Json<PushRequest>,
) -> AppResult<Json<PushResponse>> {
    let out = sync::push(&state, &who, &v, req.updates, 0).await?;
    Ok(Json(PushResponse { results: out.results, head_seq: out.head_seq }))
}

pub async fn create_snapshot(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
    Json(req): Json<CreateSnapshotRequest>,
) -> AppResult<(StatusCode, Json<SnapshotInfo>)> {
    let info = sync::create_snapshot(&state, &who, &v, &o, &req.snapshot_id, req.covers_seq, req.payload).await?;
    Ok((StatusCode::CREATED, Json(info)))
}

pub async fn list_snapshots(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
) -> AppResult<Json<Vec<SnapshotInfo>>> {
    let (_, member) = access::object_access(&state.db, &v, &o, &who.user_id).await?;
    Ok(Json(sync::list_snapshots(&state.db, &v, &o, !member).await?))
}

pub async fn get_snapshot(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o, id)): Path<(String, String, String)>,
) -> AppResult<Json<Snapshot>> {
    let (_, member) = access::object_access(&state.db, &v, &o, &who.user_id).await?;
    Ok(Json(sync::get_snapshot(&state.db, &v, &o, &id, !member).await?))
}

pub async fn purge(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    sync::purge(&state, &who, &v, &o).await?;
    Ok(StatusCode::NO_CONTENT)
}
