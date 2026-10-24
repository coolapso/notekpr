//! Admin clients: scripts and services that administer the instance — a
//! billing system setting allowances, an orchestrator switching read-only
//! mode — each with its own name, scopes and bearer tokens.
//!
//! An admin client is no account: no user row, keys, sessions or
//! memberships. Its tokens (`notekpr_a_…`) are accepted only by the
//! administration routes (`auth::Admin`); every other route takes a session
//! (`auth::Principal`), so they never reach a vault, note or key.
//!
//! Clients are made by administrators (people, or a client with the
//! `clients` scope, never with more scopes than they have), and from the
//! server's command line: `notekpr-server admin-client create` (prints a new
//! token) and `ensure` (a client with exactly the tokens in a file, for
//! declarative setups such as an init container). Both write to the database
//! directly, so they work while the server runs and on an instance without
//! administrators.

use std::collections::BTreeSet;

use notekpr_core::protocol::{ADMIN_TOKEN_PREFIX, AdminClientInfo, AdminScope, CreatedAdminToken};

use crate::db::{self, Db};
use crate::error::{AppError, AppResult, bad, conflict};
use crate::repo;

/// Characters an imported token needs after its prefix.
const MIN_TOKEN_CHARS: usize = 32;

/// `a-z`, `0-9`, `.`, `_`, `-`; 1 to 64 characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

/// Sorted, without repeats, with `read` (every client can read).
pub fn normalize(scopes: &[AdminScope]) -> Vec<AdminScope> {
    let mut set: BTreeSet<AdminScope> = scopes.iter().copied().collect();
    set.insert(AdminScope::Read);
    set.into_iter().collect()
}

/// Scope names as given on the command line (`all` for every one).
pub fn parse_scopes(names: &[String]) -> anyhow::Result<Vec<AdminScope>> {
    let mut out = Vec::new();
    for n in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        if n == "all" {
            out.extend(AdminScope::ALL);
            continue;
        }
        out.push(AdminScope::parse(n).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown scope {n:?}: use read, settings, accounts, admins, clients or all"
            )
        })?);
    }
    Ok(normalize(&out))
}

/// A token made elsewhere (an operator's secret store) that may be stored:
/// the prefix, then at least 32 printable characters.
pub fn valid_token(token: &str) -> bool {
    token.len() <= 256
        && token.strip_prefix(ADMIN_TOKEN_PREFIX).is_some_and(|rest| {
            rest.len() >= MIN_TOKEN_CHARS && rest.bytes().all(|b| b.is_ascii_graphic())
        })
}

fn hint(token: &str) -> String {
    let tail: String = token
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

/// A client's info by id.
pub async fn info(db: &Db, id: &str) -> AppResult<AdminClientInfo> {
    repo::admin_clients::list(db)
        .await?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or(AppError::NotFound)
}

/// Make a client with a first token (returned only here).
pub async fn create(
    db: &Db,
    name: &str,
    scopes: &[AdminScope],
    created_by: Option<&str>,
) -> AppResult<CreatedAdminToken> {
    if !valid_name(name) {
        return Err(bad(
            "an admin client's name is 1-64 characters of a-z, 0-9, '.', '_' or '-'",
        ));
    }
    let id = db::new_id();
    let (token, hash) = db::new_secret(ADMIN_TOKEN_PREFIX);
    let now = db::now();
    let mut tx = db.begin_with("BEGIN IMMEDIATE").await?;
    if !repo::admin_clients::insert(
        &mut *tx,
        &id,
        name,
        &normalize(scopes),
        false,
        created_by,
        now,
    )
    .await?
    {
        return Err(conflict(format!(
            "there is an admin client called {name} already"
        )));
    }
    repo::admin_clients::insert_token(&mut *tx, &db::new_id(), &id, &hash, &hint(&token), now)
        .await?;
    tx.commit().await?;
    Ok(CreatedAdminToken {
        client: info(db, &id).await?,
        token,
    })
}

/// Another token for a client (returned only here).
pub async fn new_token(db: &Db, client_id: &str) -> AppResult<CreatedAdminToken> {
    let (token, hash) = db::new_secret(ADMIN_TOKEN_PREFIX);
    repo::admin_clients::insert_token(
        db,
        &db::new_id(),
        client_id,
        &hash,
        &hint(&token),
        db::now(),
    )
    .await?;
    Ok(CreatedAdminToken {
        client: info(db, client_id).await?,
        token,
    })
}

/// What [`ensure`] changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Ensured {
    pub created: bool,
    pub added: usize,
    pub removed: usize,
}

/// Make the client called `name` exist with these scopes and exactly these
/// tokens, and mark it as kept by the server's configuration. Running it
/// again with the same arguments changes nothing.
pub async fn ensure(
    db: &Db,
    name: &str,
    scopes: &[AdminScope],
    tokens: &[String],
) -> AppResult<Ensured> {
    if !valid_name(name) {
        return Err(bad(
            "an admin client's name is 1-64 characters of a-z, 0-9, '.', '_' or '-'",
        ));
    }
    if tokens.is_empty() {
        return Err(bad("no tokens given"));
    }
    if let Some(t) = tokens.iter().find(|t| !valid_token(t)) {
        return Err(bad(format!(
            "a token ({}) isn't {ADMIN_TOKEN_PREFIX} followed by at least {MIN_TOKEN_CHARS} printable characters",
            hint(t)
        )));
    }
    let scopes = normalize(scopes);
    let wanted: BTreeSet<Vec<u8>> = tokens.iter().map(|t| db::sha256(t.as_bytes())).collect();
    let now = db::now();
    let mut out = Ensured::default();
    let mut tx = db.begin_with("BEGIN IMMEDIATE").await?;
    let id = match repo::admin_clients::by_name(&mut *tx, name).await? {
        Some((id, ..)) => {
            repo::admin_clients::set_scopes(&mut *tx, &id, &scopes).await?;
            repo::admin_clients::set_managed(&mut *tx, &id, true).await?;
            id
        }
        None => {
            let id = db::new_id();
            repo::admin_clients::insert(&mut *tx, &id, name, &scopes, true, None, now).await?;
            out.created = true;
            id
        }
    };
    let have = repo::admin_clients::token_hashes(&mut *tx, &id).await?;
    for (token_id, hash) in &have {
        if !wanted.contains(hash) {
            repo::admin_clients::delete_token(&mut *tx, &id, token_id).await?;
            out.removed += 1;
        }
    }
    for t in tokens {
        let hash = db::sha256(t.as_bytes());
        if have.iter().any(|(_, h)| *h == hash) {
            continue;
        }
        if !repo::admin_clients::insert_token(&mut *tx, &db::new_id(), &id, &hash, &hint(t), now)
            .await?
        {
            return Err(conflict(format!(
                "a token ({}) belongs to another admin client",
                hint(t)
            )));
        }
        out.added += 1;
    }
    tx.commit().await?;
    Ok(out)
}

/// The client signing in with this token.
pub struct Authenticated {
    pub id: String,
    pub name: String,
    pub scopes: Vec<AdminScope>,
}

pub async fn authenticate(db: &Db, token: &str) -> AppResult<Option<Authenticated>> {
    if token.len() > 256 || !token.starts_with(ADMIN_TOKEN_PREFIX) {
        return Ok(None);
    }
    let Some(owner) = repo::admin_clients::by_token_hash(db, &db::sha256(token.as_bytes())).await?
    else {
        return Ok(None);
    };
    let now = db::now();
    if owner.last_used_at.is_none_or(|t| now - t > 60_000) {
        let _ = repo::admin_clients::touch_token(db, &owner.token_id, now).await;
    }
    Ok(Some(Authenticated {
        id: owner.client_id,
        name: owner.name,
        scopes: owner.scopes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_tokens_and_scopes() {
        assert!(valid_name("billing"));
        assert!(valid_name("ops-dashboard.v2"));
        assert!(!valid_name(""));
        assert!(!valid_name("Billing"));
        assert!(!valid_name("bill ing"));
        assert!(!valid_name(&"a".repeat(65)));

        let ok = format!("{ADMIN_TOKEN_PREFIX}{}", "f".repeat(32));
        assert!(valid_token(&ok));
        assert!(!valid_token(&format!(
            "{ADMIN_TOKEN_PREFIX}{}",
            "f".repeat(31)
        )));
        assert!(!valid_token(&"f".repeat(64)));
        assert!(!valid_token(&format!(
            "{ADMIN_TOKEN_PREFIX}{} x",
            "f".repeat(32)
        )));
        assert_eq!(hint("notekpr_a_abcdef"), "…cdef");

        assert_eq!(
            parse_scopes(&["settings".into(), "accounts".into()]).unwrap(),
            vec![AdminScope::Read, AdminScope::Settings, AdminScope::Accounts]
        );
        assert_eq!(
            parse_scopes(&["all".into()]).unwrap(),
            AdminScope::ALL.to_vec()
        );
        assert_eq!(parse_scopes(&[]).unwrap(), vec![AdminScope::Read]);
        assert!(parse_scopes(&["root".into()]).is_err());
    }
}
