//! A D-Bus bus of its own for tests (dbus-daemon), stopped with it: where a
//! test plays the system's services (the Background portal, systemd's user
//! manager).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

pub struct Bus(Child, String);

impl Bus {
    pub fn start() -> Bus {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon (the dbus package)");
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Bus(daemon, address.trim().to_string())
    }

    pub fn address(&self) -> &str {
        &self.1
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
