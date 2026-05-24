pub mod budget;
pub mod chart;
pub mod columns;
pub mod cost_explain;
pub mod csv;
pub mod doctor;
pub mod heatmap;
pub mod json;
pub mod oneline;
pub mod pricing_audit;
pub mod summary;
pub mod table;

/// Common header shown above all table output.
pub fn header(provider_label: &str, period: &str, filter_desc: Option<&str>) -> String {
    let mut h = format!(" {provider_label} / {period}");
    if let Some(desc) = filter_desc {
        h.push_str(&format!("  ({desc})"));
    }
    h.push('\n');
    h
}

/// Build a human-readable filter description from active filters.
pub fn filter_description(
    provider: Option<&str>,
    model: Option<&str>,
    model_family: Option<&str>,
    project: Option<&str>,
    begin: Option<&str>,
    end: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(p) = provider {
        parts.push(format!("provider: {p}"));
    }
    if let Some(m) = model {
        parts.push(format!("model: {m}"));
    }
    if let Some(m) = model_family {
        parts.push(format!("model family: {m}"));
    }
    if let Some(p) = project {
        parts.push(format!("project: {p}"));
    }
    match (begin, end) {
        (Some(b), Some(e)) => parts.push(format!("{b} to {e}")),
        (Some(b), None) => parts.push(format!("from {b}")),
        (None, Some(e)) => parts.push(format!("until {e}")),
        (None, None) => {}
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_description_empty() {
        assert!(filter_description(None, None, None, None, None, None).is_none());
    }

    #[test]
    fn test_filter_description_model() {
        assert_eq!(
            filter_description(None, Some("opus"), None, None, None, None).unwrap(),
            "model: opus"
        );
    }

    #[test]
    fn test_filter_description_model_family() {
        assert_eq!(
            filter_description(None, None, Some("sonnet"), None, None, None).unwrap(),
            "model family: sonnet"
        );
    }

    #[test]
    fn test_filter_description_provider() {
        assert_eq!(
            filter_description(Some("codex"), None, None, None, None, None).unwrap(),
            "provider: codex"
        );
    }

    #[test]
    fn test_filter_description_combined() {
        let d = filter_description(
            Some("codex"),
            Some("sonnet"),
            Some("sonnet"),
            Some("myproj"),
            Some("2026-04-01"),
            Some("2026-04-07"),
        )
        .unwrap();
        assert!(d.contains("provider: codex"));
        assert!(d.contains("model: sonnet"));
        assert!(d.contains("model family: sonnet"));
        assert!(d.contains("project: myproj"));
        assert!(d.contains("2026-04-01 to 2026-04-07"));
    }
}
