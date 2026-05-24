//! Black-box regressions for corrupted billing-component state.

mod support;
use support::*;

#[test]
fn test_token_only_reports_survive_component_corruption_but_cost_reports_fail() {
    let root = temp_root("component-integrity-corruption");
    let projects = make_claude_fixture(&root);
    let db = root.join("tkstat.db");

    let seed = run_tkstat(
        &root,
        [
            "--pricing-seed",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
        ],
    );
    assert_success(&seed);

    let ingest = run_tkstat(
        &root,
        [
            "--force-update",
            "--provider",
            "claude-code",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-model",
        ],
    );
    assert_success(&ingest);

    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "DELETE FROM usage_billing_components
         WHERE provider = 'claude-code'
           AND request_id = 'req-sonnet'
           AND token_category = 'output'",
        [],
    )
    .unwrap();
    drop(conn);

    let token_only = run_tkstat(
        &root,
        [
            "--provider",
            "claude-code",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--columns",
            "total",
            "-d",
        ],
    );
    assert_success(&token_only);
    assert!(String::from_utf8_lossy(&token_only.stdout).contains("total"));

    let cost_report = run_tkstat(
        &root,
        [
            "--provider",
            "claude-code",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-model",
        ],
    );
    assert_failure(&cost_report);
    assert!(cost_report.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&cost_report.stderr);
    assert!(stderr.contains("billing component integrity error"));
    assert!(stderr.contains("request_id=req-sonnet"));
    assert!(stderr.contains("category=output"));

    let audit = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--pricing-audit",
            "--json",
        ],
    );
    assert_failure(&audit);
    let findings = parse_stdout_json(&audit);
    assert!(findings.as_array().unwrap().iter().any(|finding| {
        finding["kind"] == "BillingComponentIntegrity"
            && finding["provider"] == "claude-code"
            && finding["model_id"] == "claude-sonnet-4-5-20250929"
            && finding["token_category"] == "output"
            && finding["remediation"]
                .as_str()
                .unwrap()
                .contains("req-sonnet")
    }));

    let _ = fs::remove_dir_all(root);
}
