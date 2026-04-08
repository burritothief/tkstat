use std::path::PathBuf;

use anyhow::Result;

/// Resolve the Claude data directory.
/// Priority: CLI flag > CLAUDE_CONFIG_DIR env > ~/.claude/projects
pub fn resolve_data_dir(cli_override: Option<&str>) -> Result<PathBuf> {
    if let Some(dir) = cli_override {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return Ok(p);
        }
        anyhow::bail!("specified data dir does not exist: {dir}");
    }

    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let p = PathBuf::from(dir).join("projects");
        if p.is_dir() {
            return Ok(p);
        }
    }

    if let Some(config) = dirs::config_dir() {
        let p = config.join("claude").join("projects");
        if p.is_dir() {
            return Ok(p);
        }
    }

    if let Some(home) = dirs::home_dir() {
        let p = home.join(".claude").join("projects");
        if p.is_dir() {
            return Ok(p);
        }
    }

    Err(crate::error::TkstatError::NoDataDir.into())
}

/// Resolve the database path.
/// Priority: CLI flag > TKSTAT_DB env > ~/.local/share/tkstat/tkstat.db
pub fn resolve_db_path(cli_override: Option<&str>) -> PathBuf {
    if let Some(path) = cli_override {
        return PathBuf::from(path);
    }

    if let Ok(path) = std::env::var("TKSTAT_DB") {
        return PathBuf::from(path);
    }

    dirs::data_local_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        })
        .join("tkstat")
        .join("tkstat.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_db_path_default() {
        let path = resolve_db_path(None);
        assert!(path.to_string_lossy().contains("tkstat"));
    }

    #[test]
    fn test_resolve_db_path_override() {
        assert_eq!(resolve_db_path(Some("/tmp/my.db")), PathBuf::from("/tmp/my.db"));
    }
}
