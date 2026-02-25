/// Output of a command executed on the remote host.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl CommandOutput {
    /// Combined stdout + stderr, useful for sending to the LLM.
    pub fn combined(&self) -> String {
        let mut out = self.stdout.clone();
        if !self.stderr.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&self.stderr);
        }
        out
    }

    pub fn succeeded(&self) -> bool {
        self.exit_code == 0
    }
}

/// The interface that tools use to interact with the remote SSH session.
///
/// The main app will implement this trait on top of `TerminalTab` (PTY) or
/// a dedicated SSH exec channel — tools don't care which.
pub trait SshContext: Send + Sync {
    /// Execute a shell command and return its output.
    fn execute(&self, command: &str) -> anyhow::Result<CommandOutput>;

    /// Read a remote file's full contents as UTF-8.
    fn read_file(&self, path: &str) -> anyhow::Result<String>;

    /// Write `content` to `path` on the remote host (create or overwrite).
    fn write_file(&self, path: &str, content: &str) -> anyhow::Result<()>;

    /// Append `content` to `path` on the remote host.
    fn append_file(&self, path: &str, content: &str) -> anyhow::Result<()>;

    /// List entries in a remote directory.
    fn list_dir(&self, path: &str) -> anyhow::Result<Vec<DirEntry>>;

    /// Return `true` if the remote path exists.
    fn path_exists(&self, path: &str) -> anyhow::Result<bool>;

    /// Return the current working directory of the remote session.
    fn working_dir(&self) -> anyhow::Result<String>;
}

/// A single entry returned by `SshContext::list_dir`.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    /// Size in bytes (None if unknown).
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}
