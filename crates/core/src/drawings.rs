//! The Drawings section: each person's place for drawings of their own, the
//! same in every app.
//!
//! Each account has at most one vault of the kind [`VaultKind::Drawings`]:
//! never shared (like the quick notes), holding drawings and the files they
//! show, nothing else. Apps show it as "Drawings", never as a vault, and
//! make it the first time it's needed.
//!
//! How many drawings someone may have is counted across every vault they
//! own, the Drawings section among them ([`MeResponse::drawing_limit`]): the
//! owner pays, also for a drawing someone else makes in their vault. A move
//! to another vault of theirs needs no free slot.
//!
//! [`VaultKind::Drawings`]: crate::protocol::VaultKind::Drawings
//! [`MeResponse::drawing_limit`]: crate::protocol::MeResponse::drawing_limit

use crate::protocol::Limit;

/// What the Drawings section is called everywhere: its vault's (encrypted)
/// name and its folder in the Markdown mirror.
pub const NAME: &str = "Drawings";

/// Whether `name` is the Drawings section's own (in any case and spacing):
/// no vault may be called that, so in commands and in the Markdown folder it
/// always means one's drawings. Clients refuse it; the server can't (vault
/// names are encrypted).
pub fn is_reserved_name(name: &str) -> bool {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        == NAME.to_lowercase()
}

/// What is said when a vault would be named like the Drawings section.
pub fn reserved_name_message() -> String {
    format!("\"{NAME}\" is the name of your drawings: pick another name for the vault.")
}

/// Whether one more drawing fits when there are `count`.
pub fn has_room(count: u64, limit: Limit) -> bool {
    limit.allows_one_more(count)
}

/// What is said when there's no room for another drawing: `own`, the
/// person's own limit; otherwise that of the vault's owner (who pays for
/// what is drawn in their vaults).
pub fn full_message(limit: Limit, own: bool) -> String {
    if !own {
        return "The vault's owner has as many drawings as they may: they can make room by \
                deleting one of theirs for good."
            .into();
    }
    match limit.max() {
        Some(0) => "You can't make drawings on this server.".into(),
        Some(1) => {
            "You can have 1 drawing on this server: delete it for good to make another.".into()
        }
        Some(n) => format!(
            "You can have {n} drawings on this server: delete one for good to make another."
        ),
        None => "Drawings are full: delete one for good to make another.".into(),
    }
}

/// How many drawings someone has of how many they may: "Drawings are full
/// (1 of 1)" when there's no room, else "3 of 10 drawings" (or "3 drawings"
/// without a limit).
pub fn count_text(count: u64, limit: Limit) -> String {
    match limit.max() {
        Some(max) if !has_room(count, limit) => format!("Drawings are full ({count} of {max})"),
        Some(max) => format!("{count} of {max} drawings"),
        None if count == 1 => "1 drawing".into(),
        None => format!("{count} drawings"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_vault_is_named_like_the_drawings() {
        for n in ["Drawings", "drawings", " DRAWINGS ", "\tDrawings\n"] {
            assert!(is_reserved_name(n), "{n:?}");
        }
        for n in ["Drawings 2", "My drawings", "Drawing", "Quick notes", ""] {
            assert!(!is_reserved_name(n), "{n:?}");
        }
        assert!(reserved_name_message().contains("\"Drawings\""));
        assert!(!crate::quick::is_reserved_name(NAME));
    }

    #[test]
    fn room_follows_the_limit() {
        assert!(has_room(0, Limit::Max(1)));
        assert!(!has_room(1, Limit::Max(1)));
        assert!(
            !has_room(3, Limit::Max(1)),
            "a lowered limit takes none away"
        );
        assert!(has_room(10_000, Limit::Unlimited));
        assert!(full_message(Limit::Max(1), true).contains("1 drawing"));
        assert!(full_message(Limit::Max(5), true).contains("5 drawings"));
        assert!(full_message(Limit::Max(0), true).contains("can't"));
        assert!(full_message(Limit::Max(5), false).contains("owner"));
        assert_eq!(count_text(1, Limit::Max(1)), "Drawings are full (1 of 1)");
        assert_eq!(count_text(2, Limit::Max(10)), "2 of 10 drawings");
        assert_eq!(count_text(1, Limit::Unlimited), "1 drawing");
        assert_eq!(count_text(4, Limit::Unlimited), "4 drawings");
    }
}
