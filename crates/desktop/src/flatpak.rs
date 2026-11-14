//! Running as a Flatpak (md.notekpr.Notekpr, from Flathub;
//! packaging/flatpak): what its sandbox changes. The app runs the same,
//! but for three things:
//! * the tray's icon is written where the desktop outside can read it
//!   ([`tray_icon_dir`]);
//! * starting at login is asked of the Background portal, which writes the
//!   autostart entry outside, where the sandbox can't ([`request_autostart`]);
//! * the CLI and the app look for each other's Markdown folders in both
//!   installs' folders (`notekpr_cli::profile::desktop_roots_all`).

use std::path::PathBuf;

pub use notekpr_cli::profile::inside_flatpak as inside;

/// Where the tray's icon is written in the Flatpak: the one folder of the
/// app's runtime directory that the desktop outside sees, at the same path.
/// None outside one (the tray's own default then).
pub fn tray_icon_dir() -> Option<PathBuf> {
    if !inside() {
        return None;
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let id = std::env::var("FLATPAK_ID")
        .unwrap_or_else(|_| notekpr_cli::profile::DESKTOP_APP_ID.to_string());
    Some(PathBuf::from(runtime).join("app").join(id))
}

/// Ask the Background portal to start the app when the person logs in (or
/// no longer): `commandline` starts it, inside the sandbox. The portal may
/// ask the person first; the answer is whether it will start at login.
#[cfg(target_os = "linux")]
pub fn request_autostart(on: bool, commandline: Vec<String>) -> anyhow::Result<bool> {
    use std::collections::HashMap;

    use anyhow::Context as _;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    let bus = Connection::session().context("no session bus")?;
    let background = Proxy::new(
        &bus,
        PORTAL,
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Background",
    )?;
    // Not every desktop has it (Hyprland's portals, say): said plainly, not
    // as the bus's error.
    if background.get_property::<u32>("version").is_err() {
        anyhow::bail!("This desktop can't start notekpr's Flatpak when you log in.");
    }
    // The portal answers on a request object named after us and a token of
    // ours: listened to before asking, so the answer can't come first.
    let token = format!("notekpr_{}", std::process::id());
    let sender = bus
        .unique_name()
        .context("no name on the session bus")?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = Proxy::new(
        &bus,
        PORTAL,
        path.as_str(),
        "org.freedesktop.portal.Request",
    )?;
    let mut answers = request.receive_signal("Response")?;

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert(
        "reason",
        Value::from("Start notekpr in the tray when you log in"),
    );
    options.insert("autostart", Value::from(on));
    options.insert("commandline", Value::from(commandline));
    options.insert("dbus-activatable", Value::from(false));
    let _: OwnedObjectPath = background
        .call("RequestBackground", &("", options))
        .context("asking the Background portal")?;

    let answer = answers
        .next()
        .context("the Background portal didn't answer")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = answer.body().deserialize()?;
    // 0: done; 1: the person said no; 2: it didn't happen otherwise.
    Ok(code == 0
        && results
            .get("autostart")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false))
}

#[cfg(not(target_os = "linux"))]
pub fn request_autostart(_on: bool, _commandline: Vec<String>) -> anyhow::Result<bool> {
    anyhow::bail!("there is no Flatpak here")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_a_flatpak_the_tray_keeps_its_own_folder() {
        // Tests don't run in a Flatpak.
        assert!(!inside());
        assert_eq!(tray_icon_dir(), None);
    }
}
