use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use ftail::Ftail;
use log::LevelFilter;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Manage sheesh log files.
    Log { command: LogCommand },
}

#[derive(Debug, PartialEq, Eq)]
pub enum LogCommand {
    /// Print the newest session log file.
    View,
    /// Delete all session log files.
    Clean,
    /// Disable session logging by writing a marker file.
    Disable,
    /// Re-enable session logging by removing the marker file.
    Enable,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LogConfig {
    #[serde(default = "default_logs_enabled")]
    enabled: bool,
    /// Optional override for the session log directory.
    dir: Option<PathBuf>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: None,
        }
    }
}

fn default_logs_enabled() -> bool {
    true
}

pub fn parse_command() -> anyhow::Result<Option<Command>> {
    parse_command_from(std::env::args().skip(1))
}

fn parse_command_from(args: impl IntoIterator<Item = String>) -> anyhow::Result<Option<Command>> {
    let args: Vec<String> = args.into_iter().collect();
    match args.as_slice() {
        [] => Ok(None),
        [arg] if arg == "--help" || arg == "-h" => {
            print_help();
            std::process::exit(0);
        }
        [arg] if arg == "--version" || arg == "-V" => {
            println!("sheesh-rs {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        [cmd, subcmd] if cmd == "log" => {
            let command = match subcmd.as_str() {
                "view" => LogCommand::View,
                "clean" => LogCommand::Clean,
                "disable" => LogCommand::Disable,
                "enable" => LogCommand::Enable,
                _ => anyhow::bail!("unknown log command: {subcmd}"),
            };
            Ok(Some(Command::Log { command }))
        }
        _ => anyhow::bail!("unknown arguments: {}", args.join(" ")),
    }
}

fn print_help() {
    println!("sheesh-rs {}", env!("CARGO_PKG_VERSION"));
    println!("SSH connection manager with an embedded LLM assistant\n");
    println!("Usage:");
    println!("  sheesh-rs");
    println!("  sheesh-rs log <view|clean|disable|enable>");
}

pub fn sheesh_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sheesh")
        .join("config.toml")
}

fn default_log_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sheesh")
        .join("logs")
}

fn disabled_log_marker(log_dir: &Path) -> PathBuf {
    log_dir.join(".disabled")
}

pub fn load_log_config() -> LogConfig {
    let path = sheesh_config_path();
    match fs::read_to_string(&path) {
        Ok(content) => {
            #[derive(serde::Deserialize, Default)]
            struct ConfigFile {
                #[serde(default)]
                logs: LogConfig,
            }
            toml::from_str::<ConfigFile>(&content)
                .map(|cfg| cfg.logs)
                .unwrap_or_default()
        }
        Err(_) => LogConfig::default(),
    }
}

fn configured_log_dir(cfg: &LogConfig) -> PathBuf {
    cfg.dir.clone().unwrap_or_else(default_log_dir)
}

fn logging_enabled(cfg: &LogConfig) -> bool {
    let log_dir = configured_log_dir(cfg);
    cfg.enabled && !disabled_log_marker(&log_dir).exists()
}

pub fn init_logging(cfg: &LogConfig) -> anyhow::Result<()> {
    if !logging_enabled(cfg) {
        return Ok(());
    }

    let log_dir = configured_log_dir(cfg);
    fs::create_dir_all(&log_dir)?;
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let log_file = log_dir.join(format!("session-{started}.log"));

    Ftail::new()
        .single_file(&log_file, true, LevelFilter::Debug)
        .init()?;
    Ok(())
}

pub fn handle_log_command(command: LogCommand, cfg: &LogConfig) -> anyhow::Result<()> {
    let log_dir = configured_log_dir(cfg);
    match command {
        LogCommand::View => {
            let Some(path) = newest_log_file(&log_dir)? else {
                println!("No log files found in {}", log_dir.display());
                return Ok(());
            };
            print!("{}", fs::read_to_string(path)?);
        }
        LogCommand::Clean => {
            let removed = remove_log_files(&log_dir)?;
            println!("Removed {removed} log file(s) from {}", log_dir.display());
        }
        LogCommand::Disable => {
            fs::create_dir_all(&log_dir)?;
            fs::write(disabled_log_marker(&log_dir), "logging disabled\n")?;
            println!("Session logging disabled");
        }
        LogCommand::Enable => {
            let marker = disabled_log_marker(&log_dir);
            if marker.exists() {
                fs::remove_file(marker)?;
            }
            println!("Session logging enabled");
        }
    }
    Ok(())
}

fn newest_log_file(log_dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    if !log_dir.exists() {
        return Ok(None);
    }
    for entry in fs::read_dir(log_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !is_session_log(&path) {
            continue;
        }
        let modified = entry.metadata()?.modified().unwrap_or(UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(time, _)| modified > *time) {
            newest = Some((modified, path));
        }
    }
    Ok(newest.map(|(_, path)| path))
}

fn remove_log_files(log_dir: &Path) -> anyhow::Result<usize> {
    if !log_dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(log_dir)? {
        let path = entry?.path();
        if is_session_log(&path) {
            fs::remove_file(path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn is_session_log(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("session-") && name.ends_with(".log"))
}

#[cfg(test)]
mod tests {
    use super::{parse_command_from, Command, LogCommand};

    fn parse(args: &[&str]) -> anyhow::Result<Option<Command>> {
        parse_command_from(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn parses_no_command() {
        assert_eq!(parse(&[]).unwrap(), None);
    }

    #[test]
    fn parses_log_subcommands() {
        assert_eq!(
            parse(&["log", "view"]).unwrap(),
            Some(Command::Log {
                command: LogCommand::View,
            })
        );
        assert_eq!(
            parse(&["log", "clean"]).unwrap(),
            Some(Command::Log {
                command: LogCommand::Clean,
            })
        );
        assert_eq!(
            parse(&["log", "disable"]).unwrap(),
            Some(Command::Log {
                command: LogCommand::Disable,
            })
        );
        assert_eq!(
            parse(&["log", "enable"]).unwrap(),
            Some(Command::Log {
                command: LogCommand::Enable,
            })
        );
    }

    #[test]
    fn rejects_unknown_log_subcommand() {
        assert!(parse(&["log", "tail"]).is_err());
    }
}
