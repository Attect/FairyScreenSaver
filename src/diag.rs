//! Dead-simple append-only file logger.
//!
//! A screen saver has no console, so anything worth diagnosing goes to
//! `%APPDATA%\FairyScreenSaver\log.txt`.
//!
//! Off unless the config says otherwise -- see [`set_enabled`].  Callers log
//! unconditionally; the switch is checked in exactly one place, so adding a log
//! line never means remembering to guard it.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Whether anything is written at all.
///
/// Set once at startup from the config.  While this is false the log file is
/// not created, not opened, and not appended to -- the early return happens
/// before any filesystem call.  Everything else in the program logs freely and
/// does not have to care about the setting.
static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

fn path() -> PathBuf {
    crate::config::Config::dir().join("log.txt")
}

pub fn log(msg: &str) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let dir = crate::config::Config::dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let file = path();
    // Keep the log from growing without bound across months of use.
    if let Ok(meta) = std::fs::metadata(&file) {
        if meta.len() > 256 * 1024 {
            let _ = std::fs::remove_file(&file);
        }
    }
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&file) else {
        return;
    };
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(f, "[{secs}] {msg}");
}
