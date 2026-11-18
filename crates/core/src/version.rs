//! Release versions, to tell an app too old for a server
//! (docs/protocol.md §11), and the desktop app that a newer release is out
//! (docs/desktop.md "Update it").

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
}

/// What a build is, from its version ([`crate::VERSION`]): a release, a
/// build made after one (`0.5.0-3-gabc1234`, or `0.5.0-dev` with uncommitted
/// changes), or a development build (`dev`, a bare commit hash).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Build {
    Release(Version),
    After(Version),
    Dev,
}

impl Build {
    pub fn of(s: &str) -> Build {
        let s = s.strip_prefix('v').unwrap_or(s);
        match (Version::parse(s), Version::of_build(s)) {
            (Some(v), _) => Build::Release(v),
            (None, Some(v)) => Build::After(v),
            (None, None) => Build::Dev,
        }
    }

    /// An app's build, from what it sends in
    /// [`APP_HEADER`](crate::protocol::APP_HEADER) (`cli/0.5.0`); `None`
    /// when that doesn't say.
    pub fn of_app(app: &str) -> Option<Build> {
        Some(Build::of(app.split_once('/')?.1))
    }

    /// The release it is, or was built after; none for a development build.
    pub fn release(self) -> Option<Version> {
        match self {
            Build::Release(v) | Build::After(v) => Some(v),
            Build::Dev => None,
        }
    }
}

/// A release newer than an app built as `app`, which its server, built as
/// `server`, shows is out: the server runs it, or was built after it. None
/// when either is a development build (there is nothing to compare), or
/// the server's release isn't newer (docs/desktop.md "Update it").
pub fn newer_release(app: Build, server: Build) -> Option<Version> {
    let (app, server) = (app.release()?, server.release()?);
    (server > app).then_some(server)
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
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
        assert_eq!(Version(0, 6, 1).to_string(), "0.6.1");
    }

    #[test]
    fn what_a_build_is() {
        assert_eq!(Build::of("0.5.0"), Build::Release(Version(0, 5, 0)));
        assert_eq!(Build::of("v0.5.0"), Build::Release(Version(0, 5, 0)));
        assert_eq!(
            Build::of("0.5.0-3-gabc1234"),
            Build::After(Version(0, 5, 0))
        );
        assert_eq!(
            Build::of("0.5.0-3-gabc1234-dev"),
            Build::After(Version(0, 5, 0))
        );
        assert_eq!(Build::of("0.5.0-dev"), Build::After(Version(0, 5, 0)));
        assert_eq!(Build::of("dev"), Build::Dev);
        assert_eq!(Build::of("abc1234"), Build::Dev);
        assert_eq!(
            Build::of_app("desktop/0.5.0"),
            Some(Build::Release(Version(0, 5, 0)))
        );
        assert_eq!(Build::of_app("web/dev"), Some(Build::Dev));
        assert_eq!(Build::of_app("0.5.0"), None);
    }

    #[test]
    fn releases_newer_than_an_app() {
        let newer = |app: &str, server: &str| newer_release(Build::of(app), Build::of(server));
        assert_eq!(newer("0.23.0", "0.24.0"), Some(Version(0, 24, 0)));
        assert_eq!(newer("0.23.0", "0.23.1"), Some(Version(0, 23, 1)));
        assert_eq!(newer("0.9.0", "1.0.0"), Some(Version(1, 0, 0)));
        // A server built after a release shows that release is out…
        assert_eq!(
            newer("0.23.0", "0.24.0-3-gabc1234"),
            Some(Version(0, 24, 0))
        );
        // …and an app built after one counts as it.
        assert_eq!(
            newer("0.23.0-3-gabc1234-dev", "0.24.0"),
            Some(Version(0, 24, 0))
        );
        for (app, server) in [
            ("0.24.0", "0.24.0"),
            ("0.24.0", "0.23.0"),
            ("0.24.0-1-gabc1234", "0.24.0"),
            ("0.23.0", "0.23.0-5-gabc1234"),
            // Development builds: nothing to compare.
            ("dev", "0.24.0"),
            ("0.23.0", "dev"),
            ("abc1234", "0.24.0"),
            ("dev", "dev"),
        ] {
            assert_eq!(newer(app, server), None, "{app} {server}");
        }
    }
}
