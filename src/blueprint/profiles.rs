//! The Breeze Core builds a mock server can pretend to be.
//!
//! Each profile is what that release really advertised, so a client under test
//! sees the same `/api/version` a real one of that version sends, and the mock
//! enables exactly the behaviour those features promise.

/// Every feature the newest Breeze Core advertises, in its order.
pub const ALL_FEATURES: &[&str] = &[
    "batch_state",
    "beep_control",
    "compression",
    "config_api",
    "control_feedback",
    "delete_unit",
    "device_pairing",
    "ed25519_auth",
    "live_stream",
    "metrics",
    "programs",
    "sleep_timer",
    "system_info",
    "timer_at",
    "unit_capabilities",
    "unit_history",
    "unit_scan",
    "whoami",
];

pub struct Profile {
    pub name: &'static str,
    pub version: &'static str,
    pub commit: &'static str,
    /// Features this release did NOT have, relative to [`ALL_FEATURES`].
    pub missing: &'static [&'static str],
    pub auth_versions: &'static [u8],
}

/// Newest first. The Python line (3.x) is the reference the native line was
/// measured against; it lacks exactly the two features native added.
pub const PROFILES: &[Profile] = &[
    Profile {
        name: "4.2.0",
        version: "4.2.0",
        commit: "38e747e",
        missing: &[],
        auth_versions: &[1, 2],
    },
    Profile {
        name: "4.1.1",
        version: "4.1.1",
        commit: "e55452d",
        missing: &["timer_at"],
        auth_versions: &[1, 2],
    },
    Profile {
        name: "4.1.0",
        version: "4.1.0",
        commit: "a40a7f8",
        missing: &["control_feedback", "timer_at"],
        auth_versions: &[1, 2],
    },
    Profile {
        name: "4.0.2",
        version: "4.0.2",
        commit: "122b007",
        missing: &["control_feedback", "timer_at"],
        auth_versions: &[1, 2],
    },
    Profile {
        name: "3.2.0",
        version: "3.2.0",
        commit: "481714f",
        missing: &["control_feedback", "timer_at"],
        auth_versions: &[1, 2],
    },
];

pub fn names() -> Vec<&'static str> {
    PROFILES.iter().map(|p| p.name).collect()
}

pub fn find(name: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.name == name)
}

impl Profile {
    pub fn features(&self) -> Vec<String> {
        ALL_FEATURES
            .iter()
            .filter(|f| !self.missing.contains(f))
            .map(|f| f.to_string())
            .collect()
    }
}
