pub const ALL_PROVIDERS_LABEL: &str = "all providers";
pub const CLAUDE_CODE_PROVIDER: &str = "claude-code";
pub const CLAUDE_CODE_ALIAS: &str = "claude";
pub const CODEX_PROVIDER: &str = "codex";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderId {
    ClaudeCode,
    Codex,
}

impl ProviderId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => CLAUDE_CODE_PROVIDER,
            Self::Codex => CODEX_PROVIDER,
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            CLAUDE_CODE_PROVIDER | CLAUDE_CODE_ALIAS => Some(Self::ClaudeCode),
            CODEX_PROVIDER => Some(Self::Codex),
            _ => None,
        }
    }

    pub fn from_canonical(input: &str) -> Option<Self> {
        match input {
            CLAUDE_CODE_PROVIDER => Some(Self::ClaudeCode),
            CODEX_PROVIDER => Some(Self::Codex),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ProviderId {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input).ok_or_else(|| format!("unknown provider '{input}'"))
    }
}

impl AsRef<str> for ProviderId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<&str> for ProviderId {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

pub fn canonical_provider_id(provider: &str) -> Option<&'static str> {
    ProviderId::parse(provider).map(ProviderId::as_str)
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

    #[test]
    fn test_provider_id_parses_aliases_to_canonical_display() {
        assert_eq!(
            "claude".parse::<ProviderId>().unwrap(),
            ProviderId::ClaudeCode
        );
        assert_eq!(ProviderId::ClaudeCode.as_str(), CLAUDE_CODE_PROVIDER);
        assert_eq!(ProviderId::Codex.to_string(), CODEX_PROVIDER);
    }

    #[test]
    fn test_provider_id_from_canonical_rejects_input_alias() {
        assert_eq!(
            ProviderId::from_canonical(CLAUDE_CODE_PROVIDER),
            Some(ProviderId::ClaudeCode)
        );
        assert_eq!(ProviderId::from_canonical(CLAUDE_CODE_ALIAS), None);
    }
}
