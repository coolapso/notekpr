//! Release versions, to tell an app too old for a server
//! (docs/protocol.md §11).

use std::fmt;

/// A release's version, `x.y.z`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// Exactly `x.y.z`.
    pub fn parse(s: &str) -> Option<Version> {
        let mut parts = s.split('.');
        let mut next = || -> Option<u64> {
            let p = parts.next()?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        let v = Version(next()?, next()?, next()?);
        parts.next().is_none().then_some(v)
    }

    /// The release a build was made from: `0.5.0`, also `0.5.0-3-gabc1234`
    /// (three commits after it) and `…-dev` (uncommitted changes), as
    /// [`crate::VERSION`] says. `None` for builds not made from a release
    /// (`dev`, or a bare commit hash before the first one).
    pub fn of_build(s: &str) -> Option<Version> {
        let s = s.strip_prefix('v').unwrap_or(s);
        Version::parse(s.split('-').next().unwrap_or(s))
    }

    /// The release of an app, from what it sends in
    /// [`APP_HEADER`](crate::protocol::APP_HEADER) (`cli/0.5.0`); `None` for
    /// development builds and anything else.
    pub fn of_app(app: &str) -> Option<Version> {
        Version::of_build(app.split_once('/')?.1)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_of_releases_and_builds() {
        assert_eq!(Version::parse("0.5.0"), Some(Version(0, 5, 0)));
        assert_eq!(Version::parse("1.12.3"), Some(Version(1, 12, 3)));
        for bad in [
            "", "1", "1.2", "1.2.3.4", "1.2.x", "1..3", "-1.2.3", "1.2.3-1", " 1.2.3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version::of_build("0.5.0"), Some(Version(0, 5, 0)));
        assert_eq!(Version::of_build("v0.5.0"), Some(Version(0, 5, 0)));
        assert_eq!(
            Version::of_build("0.5.0-3-gabc1234"),
            Some(Version(0, 5, 0))
        );
        assert_eq!(
            Version::of_build("0.5.0-3-gabc1234-dev"),
            Some(Version(0, 5, 0))
        );
        assert_eq!(Version::of_build("dev"), None);
        assert_eq!(Version::of_build("abc1234"), None);
        assert_eq!(Version::of_app("cli/0.5.0"), Some(Version(0, 5, 0)));
        assert_eq!(Version::of_app("web/dev"), None);
        assert_eq!(Version::of_app("0.5.0"), None);
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
        assert_eq!(Version(0, 6, 1).to_string(), "0.6.1");
    }
}
