use crate::db::query::{CostAssumption, CostExplanation};

pub fn render_cost_explain(
    provider_label: &str,
    explanation: &CostExplanation,
    filter_desc: Option<&str>,
) -> String {
    let mut out = crate::render::header(provider_label, "cost explain", filter_desc);
    out.push_str(&format!(" confidence: {:?}\n", explanation.confidence));
    out.push_str(&format!(" cost: ${:.4}\n", explanation.cost_usd));
    out.push_str(&format!(
        " billable components: {}\n",
        explanation.component_count
    ));
    if explanation.assumptions.is_empty() {
        out.push_str(" assumptions: none\n");
        return out;
    }

    out.push_str(" assumptions:\n");
    for assumption in &explanation.assumptions {
        out.push_str(&format!("  - {}\n", assumption_label(assumption)));
    }
    out
}

fn assumption_label(assumption: &CostAssumption) -> String {
    let dimension = assumption
        .dimension
        .as_ref()
        .map(|dimension| {
            assumption
                .value
                .as_ref()
                .map(|value| format!(" {dimension}={value}"))
                .unwrap_or_else(|| format!(" {dimension}=default"))
        })
        .unwrap_or_default();
    let source = assumption
        .source
        .as_ref()
        .map(|source| format!(" source={source}"))
        .unwrap_or_default();
    format!(
        "{:?} {}/{}/{}{}{}: {}",
        assumption.kind,
        assumption.provider,
        assumption.model_id,
        assumption.token_category,
        dimension,
        source,
        assumption.detail
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::{CostAssumptionKind, CostConfidence};

    #[test]
    fn test_render_cost_explain_includes_confidence_and_assumptions() {
        let output = render_cost_explain(
            "codex",
            &CostExplanation {
                confidence: CostConfidence::Estimated,
                cost_usd: 1.25,
                component_count: 2,
                assumptions: vec![CostAssumption {
                    kind: CostAssumptionKind::AssumedDefaultModifier,
                    provider: "codex".into(),
                    model_id: "gpt-5.4".into(),
                    token_category: "input".into(),
                    dimension: Some("processing_mode".into()),
                    value: Some("standard".into()),
                    source: None,
                    detail: "standard processing mode was used".into(),
                }],
            },
            Some("provider: codex"),
        );

        assert!(output.contains("cost explain"));
        assert!(output.contains("Estimated"));
        assert!(output.contains("processing_mode=standard"));
    }
}
