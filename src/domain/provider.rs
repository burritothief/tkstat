pub const ALL_PROVIDERS_LABEL: &str = "all providers";
pub const CLAUDE_CODE_PROVIDER: &str = "claude-code";
pub const CLAUDE_CODE_ALIAS: &str = "claude";
pub const CODEX_PROVIDER: &str = "codex";

pub fn canonical_provider_id(provider: &str) -> Option<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        CLAUDE_CODE_PROVIDER | CLAUDE_CODE_ALIAS => Some(CLAUDE_CODE_PROVIDER),
        CODEX_PROVIDER => Some(CODEX_PROVIDER),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonical_provider_id_accepts_aliases() {
        assert_eq!(canonical_provider_id("claude"), Some(CLAUDE_CODE_PROVIDER));
        assert_eq!(
            canonical_provider_id("claude-code"),
            Some(CLAUDE_CODE_PROVIDER)
        );
        assert_eq!(canonical_provider_id("codex"), Some(CODEX_PROVIDER));
        assert_eq!(canonical_provider_id("unknown"), None);
    }
}
