//! Fixture-driven black-box CLI tests.

mod support;
use support::*;

#[test]
fn test_stderr_field_value_matches_provider_exactly() {
    let stderr = "missing pricing coverage for provider=claude-code, model=m, category=input, usage range a to b";
    assert_eq!(stderr_field_value(stderr, "provider"), Some("claude-code"));
    assert_ne!(stderr_field_value(stderr, "provider"), Some("claude"));
}

#[test]
fn test_pricing_failure_without_seed_then_seed_remediates_json_report() {
    let root = temp_root("pricing-failure-remediation-json");
    let projects = make_claude_corpus_fixture(&root);
    let db = root.join("tkstat.db");
    let report_args = [
        "--provider",
        "claude-code",
        "--model",
        "claude-opus-4-5-20251101",
        "--db",
        db.to_str().unwrap(),
        "--data-dir",
        projects.to_str().unwrap(),
        "--json",
        "-d",
        "--limit",
        "200",
    ];

    let missing = run_tkstat(&root, report_args);
    assert_missing_pricing_remediation(&missing, "claude-code", "claude-opus-4-5-20251101", None);
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr
            .contains("usage range 2026-01-31T21:20:19.858+00:00 to 2026-01-31T21:20:19.858+00:00")
    );

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

    let repaired = run_tkstat(&root, report_args);
    assert_success(&repaired);
    assert_no_pricing_coverage_error(&repaired);
    let json = parse_stdout_json(&repaired);
    let cost = json
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["period"] == "2026-01-31" && row["provider"] == "claude-code")
        .and_then(|row| row["cost_usd"].as_f64())
        .unwrap();
    assert!(cost > 0.0);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_incomplete_seeded_pricing_failure_then_reseed_repairs_csv_report() {
    let root = temp_root("pricing-incomplete-remediation-csv");
    let projects = make_claude_corpus_fixture(&root);
    let db = root.join("tkstat.db");
    let report_args = [
        "--provider",
        "claude-code",
        "--model",
        "claude-opus-4-5-20251101",
        "--db",
        db.to_str().unwrap(),
        "--data-dir",
        projects.to_str().unwrap(),
        "--csv",
        "-d",
        "--limit",
        "200",
    ];

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
    let initial = run_tkstat(&root, report_args);
    assert_success(&initial);
    assert_no_pricing_coverage_error(&initial);

    let conn = Connection::open(&db).unwrap();
    let deleted = conn
        .execute(
            "DELETE FROM pricing_intervals
             WHERE provider = 'claude-code'
               AND model_id = 'claude-opus-4-5-20251101'
               AND token_category = 'cache_creation'",
            [],
        )
        .unwrap();
    assert_eq!(deleted, 1);
    drop(conn);

    let missing = run_tkstat(&root, report_args);
    assert_missing_pricing_remediation(
        &missing,
        "claude-code",
        "claude-opus-4-5-20251101",
        Some("cache_creation"),
    );
    assert!(
        String::from_utf8_lossy(&missing.stderr)
            .contains("usage range 2026-01-31T21:20:19.858+00:00 to 2026-01-31T21:20:19.858+00:00")
    );

    let reseed = run_tkstat(
        &root,
        [
            "--pricing-seed",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
        ],
    );
    assert_success(&reseed);
    assert!(String::from_utf8_lossy(&reseed.stdout).contains("seeded 1 pricing intervals"));

    let repaired = run_tkstat(&root, report_args);
    assert_success(&repaired);
    assert_no_pricing_coverage_error(&repaired);
    let stdout = String::from_utf8_lossy(&repaired.stdout);
    assert!(stdout.starts_with("period,provider,input_tokens"));
    assert!(stdout.contains("2026-01-31,claude-code,"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_token_only_reports_do_not_require_pricing_but_cost_reports_do() {
    let root = temp_root("token-only-without-pricing");
    let projects = make_claude_corpus_fixture(&root);
    let db = root.join("tkstat.db");
    let base = [
        "--provider",
        "claude",
        "--db",
        db.to_str().unwrap(),
        "--data-dir",
        projects.to_str().unwrap(),
    ];

    let token_table = run_tkstat(
        &root,
        base.into_iter().chain([
            "--columns",
            "input,total",
            "-d",
            "--limit",
            "200",
            "--no-color",
        ]),
    );
    assert_success(&token_table);
    let stdout = String::from_utf8_lossy(&token_table.stdout);
    assert!(stdout.contains("claude-code / daily"));
    assert!(stdout.contains("2026-01-31"));
    assert!(!stdout.contains("cost"));
    assert_no_pricing_coverage_error(&token_table);

    let token_chart = run_tkstat(
        &root,
        base.into_iter()
            .chain(["--chart", "--chart-metric", "tokens", "--no-color"]),
    );
    assert_success(&token_chart);
    assert!(String::from_utf8_lossy(&token_chart.stdout).contains("claude-code / chart"));
    assert_no_pricing_coverage_error(&token_chart);

    let input_heatmap = run_tkstat(
        &root,
        base.into_iter()
            .chain(["--heatmap", "--chart-metric", "input", "--no-color"]),
    );
    assert_success(&input_heatmap);
    assert!(String::from_utf8_lossy(&input_heatmap.stdout).contains("claude-code / heatmap"));
    assert_no_pricing_coverage_error(&input_heatmap);

    for output in [
        run_tkstat(
            &root,
            base.into_iter().chain([
                "--columns",
                "input,total",
                "--csv",
                "-d",
                "--daily-budget-usd",
                "0.01",
            ]),
        ),
        run_tkstat(
            &root,
            base.into_iter().chain([
                "--columns",
                "input,total",
                "--csv",
                "-d",
                "--monthly-budget-usd",
                "0.01",
            ]),
        ),
    ] {
        assert_failure(&output);
        assert!(
            output.stdout.is_empty(),
            "budget pricing failures should not write CSV stdout; stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("missing pricing coverage"));
        assert_eq!(stderr_field_value(&stderr, "provider"), Some("claude-code"));
        assert!(stderr.contains("tkstat --pricing-seed"));
    }

    for output in [
        run_tkstat(&root, base.into_iter().chain(["-d"])),
        run_tkstat(&root, base.into_iter().chain(["--columns", "cost", "-d"])),
        run_tkstat(
            &root,
            base.into_iter()
                .chain(["--budget", "--begin", "2026-01-31", "--end", "2026-01-31"]),
        ),
        run_tkstat(&root, base.into_iter().chain(["--json", "-d"])),
    ] {
        assert_failure(&output);
        assert!(String::from_utf8_lossy(&output.stderr).contains("missing pricing coverage"));
    }

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_pricing_audit_missing_database_is_read_only() {
    let root = temp_root("pricing-audit-missing-db");
    let db = root.join("missing.db");

    let output = run_tkstat(
        &root,
        ["--db", db.to_str().unwrap(), "--pricing-audit", "--json"],
    );
    assert_failure(&output);
    assert!(!db.exists());
    let json = parse_stdout_json(&output);
    let findings = json.as_array().unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["kind"], "MissingDatabase");
    assert!(
        findings[0]["remediation"]
            .as_str()
            .unwrap()
            .contains("--pricing-seed")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_pricing_audit_clean_catalog_exits_success() {
    let root = temp_root("pricing-audit-clean");
    let projects = make_claude_fixture(&root);
    let db = root.join("tkstat.db");
    seed_pricing(&root, &db, &projects);

    let output = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--pricing-audit",
        ],
    );
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("pricing audit: no findings"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_pricing_audit_json_reports_missing_coverage_and_exits_nonzero() {
    let root = temp_root("pricing-audit-missing-coverage");
    fs::create_dir_all(&root).unwrap();
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("tkstat.db");
    let database = Database::open(&db).unwrap();
    database
        .insert_records(&[audit_record("gpt-audit")])
        .unwrap();
    drop(database);

    let output = run_tkstat(
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
    assert_failure(&output);
    let json = parse_stdout_json(&output);
    let findings = json.as_array().unwrap();
    assert!(findings.iter().any(|finding| {
        finding["kind"] == "MissingCoverage"
            && finding["provider"] == "codex"
            && finding["model_id"] == "gpt-audit"
            && finding["token_category"] == "cached_input"
            && finding["remediation"]
                .as_str()
                .unwrap()
                .contains("--pricing-seed")
    }));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_pricing_audit_json_reports_unsupported_usage_provider_without_aborting() {
    let root = temp_root("pricing-audit-unsupported-provider");
    fs::create_dir_all(&root).unwrap();
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("tkstat.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE pricing_intervals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            provider TEXT NOT NULL,
            model_id TEXT NOT NULL,
            token_category TEXT NOT NULL,
            currency TEXT NOT NULL DEFAULT 'USD',
            rate_per_1m_tokens REAL NOT NULL,
            effective_from TEXT NOT NULL,
            effective_to TEXT,
            source TEXT NOT NULL
         );
         CREATE TABLE token_usage (
            provider TEXT NOT NULL,
            request_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            uuid TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            model_family TEXT NOT NULL,
            model_id TEXT NOT NULL,
            input_tokens INTEGER NOT NULL,
            output_tokens INTEGER NOT NULL,
            cache_creation_tokens INTEGER NOT NULL,
            cache_read_tokens INTEGER NOT NULL,
            cached_input_tokens INTEGER NOT NULL DEFAULT 0,
            reasoning_output_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL,
            cost_usd REAL NOT NULL,
            project TEXT NOT NULL,
            source_file TEXT NOT NULL,
            is_subagent INTEGER NOT NULL DEFAULT 0
         );
         INSERT INTO token_usage
            (provider, request_id, session_id, uuid, timestamp, model_family, model_id,
             input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
             cached_input_tokens, reasoning_output_tokens, total_tokens, cost_usd, project, source_file, is_subagent)
         VALUES
            ('manual-provider', 'r1', 's1', 'u1', '2026-04-07T10:00:00Z', 'unknown', 'manual-model',
             10, 0, 0, 0, 0, 0, 10, 0.0, 'manual', '/manual.jsonl', 0);",
    )
    .unwrap();
    drop(conn);

    let output = run_tkstat(
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
    assert_failure(&output);
    let json = parse_stdout_json(&output);
    let findings = json.as_array().unwrap();
    assert!(findings.iter().any(|finding| {
        finding["kind"] == "UnsupportedProviderId"
            && finding["provider"] == "manual-provider"
            && finding["model_id"] == "manual-model"
            && finding["token_category"] == ""
            && finding["remediation"]
                .as_str()
                .unwrap()
                .contains("canonical provider id")
    }));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_pricing_audit_reports_overlap_and_exits_nonzero() {
    let root = temp_root("pricing-audit-overlap");
    fs::create_dir_all(&root).unwrap();
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("tkstat.db");
    let database = Database::open(&db).unwrap();
    database
        .insert_pricing_interval(&audit_interval(
            "2026-01-01T00:00:00Z",
            Some("2026-03-01T00:00:00Z"),
        ))
        .unwrap();
    database
        .insert_pricing_interval(&audit_interval("2026-02-01T00:00:00Z", None))
        .unwrap();
    drop(database);

    let output = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--pricing-audit",
        ],
    );
    assert_failure(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("pricing audit"));
    assert!(stdout.contains("Overlap"));
    assert!(stdout.contains("codex"));
    assert!(stdout.contains("gpt-audit/input"));

    let _ = fs::remove_dir_all(root);
}
