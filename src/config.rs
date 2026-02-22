use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::ssh::SSHConnection;

/// Returns the path to ~/.ssh/config, creating the file if it doesn't exist.
pub fn ssh_config_path() -> PathBuf {
    let home = dirs::home_dir().expect("cannot determine home directory");
    home.join(".ssh").join("config")
}

/// Parse all `Host` blocks from a ~/.ssh/config file into `SSHConnection`s.
/// Wildcards (`Host *`) are ignored.
pub fn load_connections(path: &Path) -> Result<Vec<SSHConnection>> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).context("reading ~/.ssh/config"),
    };

    let mut connections: Vec<SSHConnection> = vec![];
    let mut current: Option<SSHConnection> = None;
    let mut pending_comment = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('#') {
            let comment = trimmed.trim_start_matches('#').trim();
            if !pending_comment.is_empty() {
                pending_comment.push(' ');
            }
            pending_comment.push_str(comment);
            continue;
        }

        if trimmed.is_empty() {
            // Blank line resets pending comment if no Host block has started
            if current.is_none() {
                pending_comment.clear();
            }
            continue;
        }

        let (key, value) = match trimmed.split_once(char::is_whitespace) {
            Some(pair) => (pair.0.to_lowercase(), pair.1.trim().to_string()),
            None => continue,
        };

        match key.as_str() {
            "Host" | "host" => {
                if let Some(conn) = current.take() {
                    connections.push(conn);
                }
                // Skip wildcard blocks
                if value == "*" {
                    pending_comment.clear();
                    continue;
                }
                let mut conn = SSHConnection::default();
                conn.name = value;
                conn.description = std::mem::take(&mut pending_comment);
                current = Some(conn);
            }
            "HostName" | "hostname" => {
                if let Some(ref mut c) = current {
                    c.hostname = value;
                }
            }
            "User" | "user" => {
                if let Some(ref mut c) = current {
                    c.user = value;
                }
            }
            "Port" | "port" => {
                if let Some(ref mut c) = current {
                    c.port = value.parse().unwrap_or(22);
                }
            }
            "IdentityFile" | "identityfile" => {
                if let Some(ref mut c) = current {
                    c.identity_file = Some(value);
                }
            }
            _ => {
                if let Some(ref mut c) = current {
                    c.extra_options.push(format!("{} {}", key, value));
                }
            }
        }
    }

    if let Some(conn) = current {
        connections.push(conn);
    }

    Ok(connections)
}

/// Write connections back to ~/.ssh/config.
/// Preserves the rest of the file (lines not belonging to any managed Host block).
pub fn save_connections(path: &Path, connections: &[SSHConnection]) -> Result<()> {
    let mut out = String::new();

    for conn in connections {
        if !conn.description.is_empty() {
            out.push_str(&format!("# {}\n", conn.description));
        }
        out.push_str(&format!("Host {}\n", conn.name));
        out.push_str(&format!("    HostName {}\n", conn.hostname));
        out.push_str(&format!("    User {}\n", conn.user));
        if conn.port != 0 && conn.port != 22 {
            out.push_str(&format!("    Port {}\n", conn.port));
        }
        if let Some(ref key) = conn.identity_file {
            out.push_str(&format!("    IdentityFile {}\n", key));
        }
        for opt in &conn.extra_options {
            out.push_str(&format!("    {}\n", opt));
        }
        out.push('\n');
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("creating ~/.ssh directory")?;
    }
    fs::write(path, out).context("writing ~/.ssh/config")?;
    Ok(())
}
