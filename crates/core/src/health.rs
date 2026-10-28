//! Laufzeitstatus für `/health` und den Docker-Healthcheck.

use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Default)]
pub struct Health {
    discord_connected: AtomicBool,
}

impl Health {
    pub fn set_discord_connected(&self, connected: bool) {
        self.discord_connected.store(connected, Ordering::Relaxed);
    }

    pub fn discord_connected(&self) -> bool {
        self.discord_connected.load(Ordering::Relaxed)
    }
}
