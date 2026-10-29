//! How many drawings a vault may hold, and what is said about it.
//!
//! Drawings belong to vaults, like notes. Each vault may hold so many
//! ([`VaultInfo::drawing_limit`]): the instance's `drawings_per_vault`
//! unless an administrator gave the vault its own. They count whoever makes
//! them, those in the trash too, until deleted for good; a drawing moved to
//! a vault needs room there.
//!
//! [`VaultInfo::drawing_limit`]: crate::protocol::VaultInfo::drawing_limit

use crate::protocol::Limit;

/// Whether one more drawing fits when there are `count`.
pub fn has_room(count: u64, limit: Limit) -> bool {
    limit.allows_one_more(count)
}

/// What is said when the vault has no room for another drawing.
pub fn full_message(limit: Limit) -> String {
    match limit.max() {
        Some(0) => "This vault can't hold drawings on this server.".into(),
        Some(1) => {
            "This vault may hold 1 drawing on this server: delete it for good to make another."
                .into()
        }
        Some(n) => format!(
            "This vault may hold {n} drawings on this server: delete one for good to make another."
        ),
        None => "Drawings are full: delete one for good to make another.".into(),
    }
}

/// How many drawings a vault holds of how many it may: "Drawings are full
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
    fn room_follows_the_limit() {
        assert!(has_room(0, Limit::Max(1)));
        assert!(!has_room(1, Limit::Max(1)));
        assert!(
            !has_room(3, Limit::Max(1)),
            "a lowered limit takes none away"
        );
        assert!(has_room(10_000, Limit::Unlimited));
        assert!(full_message(Limit::Max(1)).contains("1 drawing"));
        assert!(full_message(Limit::Max(5)).contains("5 drawings"));
        assert!(full_message(Limit::Max(0)).contains("can't"));
        assert_eq!(count_text(1, Limit::Max(1)), "Drawings are full (1 of 1)");
        assert_eq!(count_text(2, Limit::Max(10)), "2 of 10 drawings");
        assert_eq!(count_text(1, Limit::Unlimited), "1 drawing");
        assert_eq!(count_text(4, Limit::Unlimited), "4 drawings");
    }
}
