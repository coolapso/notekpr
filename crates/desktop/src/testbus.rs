//! A D-Bus bus of its own for tests (dbus-daemon), stopped with it: where a
//! test plays the system's services (the Background portal, systemd's user
//! manager, the notification server). Nothing else is on it: it starts no
//! service of the computer's when a name is asked for (a session bus's
//! usual settings would, a notification server say).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

/// A session bus that anyone on it may use, and nothing more.
const CONFIG: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

pub struct Bus {
    daemon: Child,
    address: String,
    /// Its settings, for as long as it runs.
    _config: tempfile::NamedTempFile,
}

impl Bus {
    pub fn start() -> Bus {
        let config = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(config.path(), CONFIG).unwrap();
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.path().display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon (the dbus package)");
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Bus {
            daemon,
            address: address.trim().to_string(),
            _config: config,
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}
