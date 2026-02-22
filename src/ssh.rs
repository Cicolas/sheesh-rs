use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SSHConnection {
    /// Matches the `Host` alias in ~/.ssh/config
    pub name: String,
    pub description: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
    /// Extra SSH options as key=value pairs (e.g. "ForwardAgent yes")
    pub extra_options: Vec<String>,
}

impl SSHConnection {
    pub fn ssh_args(&self) -> Vec<String> {
        let mut args = vec![];

        if self.port != 0 && self.port != 22 {
            args.push("-p".into());
            args.push(self.port.to_string());
        }

        if let Some(ref key) = self.identity_file {
            args.push("-i".into());
            args.push(key.clone());
        }

        for opt in &self.extra_options {
            args.push("-o".into());
            args.push(opt.clone());
        }

        args.push(format!("{}@{}", self.user, self.hostname));
        args
    }
}
