use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConsoleConfig {
    /// Master switch. Missing or false means the console is not started at all.
    #[serde(default)]
    pub enabled: bool,
    /// Listen address, e.g. "127.0.0.1:11211".
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Login password. Empty password is rejected when the console is enabled.
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_session_ttl_hours")]
    pub session_ttl_hours: u64,
    /// SQLite database file.
    #[serde(default = "default_db_path")]
    pub db_path: String,
    /// Explicit peer console addresses for config sync ("ip:port").
    /// Empty means: use overlay IPs of all known peers and the local port.
    #[serde(default)]
    pub sync_peers: Vec<String>,
    /// Local TOML file edited through the node console (if started from a file).
    #[serde(default)]
    pub config_path: Option<std::path::PathBuf>,
}

fn default_listen() -> String {
    "127.0.0.1:11211".to_string()
}

fn default_session_ttl_hours() -> u64 {
    12
}

fn default_db_path() -> String {
    "/var/lib/dokitier/web.db".to_string()
}

impl Default for WebConsoleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: default_listen(),
            password: String::new(),
            session_ttl_hours: default_session_ttl_hours(),
            db_path: default_db_path(),
            sync_peers: Vec::new(),
            config_path: None,
        }
    }
}

impl WebConsoleConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if self.password.trim().is_empty() {
            anyhow::bail!("web_console.password must not be empty when web console is enabled");
        }
        self.listen
            .parse::<std::net::SocketAddr>()
            .map_err(|err| anyhow::anyhow!("invalid web_console.listen {:?}: {err}", self.listen))?;
        if !self.listen.starts_with("127.")
            && !self.listen.starts_with("[::1]")
            && !self.listen.starts_with("localhost")
        {
            tracing::warn!(
                listen = %self.listen,
                "web console is listening on a non-loopback address"
            );
        }
        Ok(())
    }
}
