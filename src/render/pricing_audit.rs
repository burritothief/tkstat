use crate::db::pricing::PricingAuditFinding;

pub fn render_pricing_audit(findings: &[PricingAuditFinding]) -> String {
    if findings.is_empty() {
        return "pricing audit: no findings\n".into();
    }

    let mut out = String::new();
    out.push_str("pricing audit\n");
    out.push_str(" severity kind                 provider    model/category                  range                                      remediation\n");
    out.push_str(" -------- -------------------- ----------- ------------------------------- ------------------------------------------ ----------------------------------------\n");
    for finding in findings {
        out.push_str(&format!(
            " {:<8} {:<20} {:<11} {:<31} {:<42} {}\n",
            format!("{:?}", finding.severity),
            format!("{:?}", finding.kind),
            finding.provider,
            format!("{}/{}", finding.model_id, finding.token_category),
            range_label(finding),
            finding.remediation
        ));
    }
    out
}

fn range_label(finding: &PricingAuditFinding) -> String {
    format!(
        "{}..{}",
        finding.start.as_deref().unwrap_or("-"),
        finding.end.as_deref().unwrap_or("-")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::pricing::{PricingAuditKind, PricingAuditSeverity};

    #[test]
    fn test_render_pricing_audit_empty() {
        let output = render_pricing_audit(&[]);
        assert!(output.contains("no findings"));
    }

    #[test]
    fn test_render_pricing_audit_includes_finding_details() {
        let finding = PricingAuditFinding {
            severity: PricingAuditSeverity::Error,
            kind: PricingAuditKind::MissingCoverage,
            provider: "codex".into(),
            model_id: "gpt-audit".into(),
            token_category: "cached_input".into(),
            start: Some("2026-04-07T10:00:00+00:00".into()),
            end: Some("2026-04-07T10:00:00+00:00".into()),
            remediation: "run `tkstat --pricing-seed`".into(),
        };
        let output = render_pricing_audit(&[finding]);
        assert!(output.contains("Error"));
        assert!(output.contains("MissingCoverage"));
        assert!(output.contains("codex"));
        assert!(output.contains("gpt-audit/cached_input"));
        assert!(output.contains("tkstat --pricing-seed"));
    }
}
