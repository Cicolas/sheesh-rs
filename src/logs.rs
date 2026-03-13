use std::path::{Path, PathBuf};

use anyhow::Result;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Default, Debug)]
pub struct LogsConfig {
    /// Directory where session log files are stored.
    /// Defaults to `$TMPDIR/sheesh` (usually `/tmp/sheesh`).
    #[serde(default)]
    pub dir: Option<PathBuf>,
}

impl LogsConfig {
    /// Resolves the log directory, applying the default when `dir` is unset.
    pub fn resolved_dir(&self) -> PathBuf {
        self.dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("sheesh"))
    }
}

// ---------------------------------------------------------------------------
// Disabled marker (always in ~/.config/sheesh/ — survives /tmp clears)
// ---------------------------------------------------------------------------

fn disabled_marker() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sheesh")
        .join(".logs-disabled")
}

/// Returns `true` when the user has disabled logging.
pub fn is_disabled() -> bool {
    disabled_marker().exists()
}

// ---------------------------------------------------------------------------
// Session log path
// ---------------------------------------------------------------------------

/// Returns a per-session log path inside `dir`, e.g.
/// `/tmp/sheesh/session-1741875000.log`.
pub fn session_log_path(dir: &Path) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    dir.join(format!("session-{secs}.log"))
}

// ---------------------------------------------------------------------------
// Migration helper
// ---------------------------------------------------------------------------

/// If `dir` currently exists as a plain file (legacy single-file log), move it
/// into the directory as `session-legacy.log` and create the directory.
fn migrate_if_file(dir: &Path) {
    if dir.is_dir() {
        return;
    }
    if dir.is_file() {
        // Temporarily move the file aside.
        let tmp = dir.with_extension("log.bak");
        if std::fs::rename(dir, &tmp).is_ok() && std::fs::create_dir_all(dir).is_ok() {
            let _ = std::fs::rename(tmp, dir.join("session-legacy.log"));
        }
    }
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

/// `sheesh-rs log clean` — removes all `*.log` files from `dir`.
pub fn cmd_clean(dir: &Path) -> Result<()> {
    migrate_if_file(dir);
    if !dir.exists() {
        println!("No logs directory found ({}).", dir.display());
        return Ok(());
    }
    let mut count = 0u32;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "log") {
            std::fs::remove_file(&path)?;
            count += 1;
        }
    }
    println!("Removed {count} log file(s) from {}.", dir.display());
    Ok(())
}

/// `sheesh-rs log view` — prints the most recent session log to stdout.
pub fn cmd_view(dir: &Path) -> Result<()> {
    migrate_if_file(dir);
    if !dir.exists() {
        println!("No logs directory found ({}).", dir.display());
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .collect();
    // File names are `session-<unix-secs>.log` — lexicographic order is chronological.
    entries.sort_by_key(|e| e.file_name());
    match entries.last() {
        None => println!("No log files found in {}.", dir.display()),
        Some(entry) => {
            eprintln!("==> {} <==", entry.path().display());
            let content = std::fs::read_to_string(entry.path())?;
            print!("{content}");
        }
    }
    Ok(())
}

/// `sheesh-rs log disable` — creates the disabled marker.
pub fn cmd_disable() -> Result<()> {
    let marker = disabled_marker();
    if marker.exists() {
        println!("Logging is already disabled.");
        return Ok(());
    }
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&marker, "")?;
    println!("Logging disabled. Run `sheesh-rs log enable` to re-enable.");
    Ok(())
}

/// `sheesh-rs log enable` — removes the disabled marker.
pub fn cmd_enable() -> Result<()> {
    let marker = disabled_marker();
    if marker.exists() {
        std::fs::remove_file(&marker)?;
        println!("Logging enabled.");
    } else {
        println!("Logging is already enabled.");
    }
    Ok(())
}
