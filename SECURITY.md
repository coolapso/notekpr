# Security

## Reporting a vulnerability

Please don't post it in an issue or a discussion: both are public.

Report it privately on GitHub instead: this repository's **Security** tab →
**Report a vulnerability**
([direct link](https://github.com/coolapso/notekpr/security/advisories/new)).
Only the maintainer sees it. Say what you found, how to make it happen, the
version (in the app: Settings → Account, at the end; the CLI:
`notekpr --version`), and what someone could do with it.

The maintainer answers in the report, and works out the fix with you there.
Once a release has the fix, the advisory is published, with credit to you if
you want it.

## What counts

notekpr's promises are in [docs/security.md](docs/security.md): what is
encrypted, who can read and change what, and what a server can still see. A
way around one of them is a vulnerability: for example, a server or another
person reading something they shouldn't, a removed member still getting new
content, or an answer that gives away whether a vault or a note exists.

What docs/security.md already lists as a known limit ("Residual risks and
planned hardening") isn't new; a better way to close one is welcome in a
discussion.

The public demo runs in development mode on purpose: anyone can sign in as
anyone, and it's reset regularly, so keep nothing real there. That isn't a
vulnerability. Development mode turning on by accident on a real server
would be.

## Supported versions

notekpr is before 1.0: fixes go into the next release, so update to it.
