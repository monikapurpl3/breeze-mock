//! One line per event, to stderr, with the time and who it is about.
//!
//! Levels: 0 quiet (errors only), 1 normal (connections, controls,
//! refusals), 2 verbose (every request and reply).

use std::sync::atomic::{AtomicU8, Ordering};

static LEVEL: AtomicU8 = AtomicU8::new(1);

pub fn set_level(level: u8) {
    LEVEL.store(level, Ordering::Relaxed);
}

pub fn enabled(level: u8) -> bool {
    LEVEL.load(Ordering::Relaxed) >= level
}

pub fn line(level: u8, who: &str, message: std::fmt::Arguments) {
    if enabled(level) {
        let now = chrono::Local::now().format("%H:%M:%S%.3f");
        eprintln!("{now} [{who}] {message}");
    }
}

#[macro_export]
macro_rules! info {
    ($who:expr, $($arg:tt)*) => { $crate::log::line(1, &$who, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! debug {
    ($who:expr, $($arg:tt)*) => { $crate::log::line(2, &$who, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! error {
    ($who:expr, $($arg:tt)*) => { $crate::log::line(0, &$who, format_args!($($arg)*)) };
}
