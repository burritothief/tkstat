use std::path::PathBuf;

use anyhow::Result;

/// Resolve the Claude data directory.
/// Priority: CLI flag > CLAUDE_CONFIG_DIR env > ~/.config/claude/projects > ~/.claude/projects
pub fn resolve_data_dir(cli_override: Option<&str>) -> Result<Option<PathBuf>> {
    if let Some(dir) = cli_override {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return Ok(Some(p));
        }
        anyhow::bail!("specified data dir does not exist: {dir}");
    }

    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        let p = PathBuf::from(dir).join("projects");
        if p.is_dir() {
            return Ok(Some(p));
        }
    }

    if let Some(config) = dirs::config_dir() {
        let p = config.join("claude").join("projects");
        if p.is_dir() {
            return Ok(Some(p));
        }
    }

    if let Some(home) = dirs::home_dir() {
        let p = home.join(".claude").join("projects");
        if p.is_dir() {
            return Ok(Some(p));
        }
    }

    Ok(None)
}

/// Resolve the Codex home directory.
/// Priority: CODEX_HOME env > ~/.codex
pub fn resolve_codex_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return Some(p);
        }
    }

    dirs::home_dir()
        .map(|home| home.join(".codex"))
        .filter(|p| p.is_dir())
}

/// Resolve the database path.
/// Priority: CLI flag > TKSTAT_DB env > ~/.local/share/tkstat/tkstat.db
pub fn resolve_db_path(cli_override: Option<&str>) -> PathBuf {
    if let Some(path) = cli_override {
        return PathBuf::from(path);
    }

    if let Some(path) = std::env::var_os("TKSTAT_DB") {
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
        assert_eq!(
            resolve_db_path(Some("/tmp/my.db")),
            PathBuf::from("/tmp/my.db")
        );
    }

    #[test]
    fn test_resolve_data_dir_missing_override_errors() {
        assert!(resolve_data_dir(Some("/definitely/not/tkstat")).is_err());
    }
}
