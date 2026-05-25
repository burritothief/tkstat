//! Fixture-driven black-box CLI tests.

mod support;
use std::process::Command;
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
fn test_seeded_pricing_covers_claude_standard_modifiers() {
    let root = temp_root("pricing-claude-standard-modifiers");
    let projects = root.join("claude").join("projects");
    let project_dir = projects.join("-home-tester-work-standard-modifiers");
    fs::create_dir_all(&project_dir).unwrap();
    fs::write(
        project_dir.join("main.jsonl"),
        r#"{"type":"assistant","message":{"model":"claude-haiku-4-5-20251001","usage":{"input_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":1000000,"output_tokens":0,"service_tier":"standard","speed":"standard"}},"requestId":"standard-modifiers","uuid":"standard-modifiers-uuid","timestamp":"2026-04-07T10:00:00Z","sessionId":"standard-session"}"#,
    )
    .unwrap();
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

    let report = run_tkstat(
        &root,
        [
            "--provider",
            "claude-code",
            "--model",
            "claude-haiku-4-5-20251001",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--force-update",
            "--by-model",
            "--json",
        ],
    );
    assert_success(&report);
    assert_no_pricing_coverage_error(&report);
    let json = parse_stdout_json(&report);
    let row = json
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["model_id"] == "claude-haiku-4-5-20251001")
        .unwrap();
    assert_eq!(row["cache_read_tokens"], serde_json::json!(1_000_000));
    assert_eq!(row["cost_usd"], serde_json::json!(0.1));

    let conn = Connection::open(&db).unwrap();
    let dimensions: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT service_tier, speed
             FROM usage_billing_components
             WHERE request_id = 'standard-modifiers' AND token_category = 'cache_read'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(dimensions, (None, None));
    drop(conn);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_seeded_pricing_covers_claude_cache_creation_ttl_dimensions() {
    let root = temp_root("pricing-claude-cache-creation-ttl");
    let projects = root.join("claude").join("projects");
    let project_dir = projects.join("-home-tester-work-cache-ttl");
    fs::create_dir_all(&project_dir).unwrap();
    fs::write(
        project_dir.join("main.jsonl"),
        r#"{"type":"assistant","message":{"model":"claude-sonnet-4-5-20250929","usage":{"input_tokens":0,"cache_creation_input_tokens":2000000,"cache_creation":{"ephemeral_5m_input_tokens":1000000,"ephemeral_1h_input_tokens":1000000},"cache_read_input_tokens":0,"output_tokens":0}},"requestId":"cache-ttl","uuid":"cache-ttl-uuid","timestamp":"2026-01-31T21:37:42.435Z","sessionId":"cache-ttl-session"}"#,
    )
    .unwrap();
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

    let report = run_tkstat(
        &root,
        [
            "--provider",
            "claude",
            "--model",
            "claude-sonnet-4-5-20250929",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--force-update",
            "--by-model",
            "--json",
        ],
    );
    assert_success(&report);
    assert_no_pricing_coverage_error(&report);
    let json = parse_stdout_json(&report);
    let row = json
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["model_id"] == "claude-sonnet-4-5-20250929")
        .unwrap();
    assert_eq!(row["cache_creation_tokens"], serde_json::json!(2_000_000));
    assert_eq!(row["cost_usd"], serde_json::json!(9.75));

    let conn = Connection::open(&db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT source_detail, tokens
             FROM usage_billing_components
             WHERE request_id = 'cache-ttl' AND token_category = 'cache_creation'
             ORDER BY source_detail",
        )
        .unwrap();
    let rows: Vec<(Option<String>, i64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (Some("ephemeral_1h".into()), 1_000_000),
            (Some("ephemeral_5m".into()), 1_000_000),
        ]
    );
    drop(stmt);
    drop(conn);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_seeded_pricing_covers_claude_placeholder_regions() {
    let root = temp_root("pricing-claude-placeholder-regions");
    let projects = root.join("claude").join("projects");
    let project_dir = projects.join("-home-tester-work-placeholder-regions");
    fs::create_dir_all(&project_dir).unwrap();
    fs::write(
        project_dir.join("main.jsonl"),
        concat!(
            r#"{"type":"assistant","message":{"model":"claude-haiku-4-5-20251001","usage":{"input_tokens":0,"cache_creation_input_tokens":1000000,"cache_creation":{"ephemeral_5m_input_tokens":1000000},"cache_read_input_tokens":0,"output_tokens":0,"speed":"fast","inference_geo":"not_available"}},"requestId":"region-not-available","uuid":"region-not-available-uuid","timestamp":"2026-02-11T08:40:09.341Z","sessionId":"region-session"}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":1000000,"cache_creation_input_tokens":0,"cache_read_input_tokens":1000000,"output_tokens":1000000,"speed":"fast","inference_geo":"global"}},"requestId":"region-global","uuid":"region-global-uuid","timestamp":"2026-02-14T00:55:16.342Z","sessionId":"region-session"}"#,
            "\n",
        ),
    )
    .unwrap();
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

    let report = run_tkstat(
        &root,
        [
            "--provider",
            "claude",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--force-update",
            "--by-model",
            "--json",
        ],
    );
    assert_success(&report);
    assert_no_pricing_coverage_error(&report);
    let json = parse_stdout_json(&report);
    let rows = json.as_array().unwrap();
    assert!(rows.iter().any(|row| {
        row["model_id"] == "claude-haiku-4-5-20251001"
            && row["cache_creation_tokens"] == serde_json::json!(1_000_000)
            && row["cost_usd"] == serde_json::json!(1.25)
    }));
    assert!(rows.iter().any(|row| {
        row["model_id"] == "claude-opus-4-6"
            && row["input_tokens"] == serde_json::json!(1_000_000)
            && row["output_tokens"] == serde_json::json!(1_000_000)
            && row["cache_read_tokens"] == serde_json::json!(1_000_000)
            && row["cost_usd"] == serde_json::json!(30.5)
    }));

    let conn = Connection::open(&db).unwrap();
    let normalized_components: i64 = conn
        .query_row(
            "SELECT COUNT(*)
             FROM usage_billing_components
             WHERE request_id IN ('region-not-available', 'region-global')
               AND region IS NULL
               AND speed IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(normalized_components, 4);
    drop(conn);

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
    assert_eq!(deleted, 3);
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
    assert!(String::from_utf8_lossy(&reseed.stdout).contains("seeded 3 pricing intervals"));

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
fn test_cost_report_missing_component_modifier_writes_no_json_or_table_stdout() {
    let root = temp_root("pricing-missing-component-modifier");
    fs::create_dir_all(&root).unwrap();
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("tkstat.db");
    let database = Database::open(&db).unwrap();
    database
        .insert_pricing_interval(&PricingInterval::usd(
            tkstat::domain::provider::ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z".parse().unwrap(),
            "e2e",
        ))
        .unwrap();
    let mut record = cli_claude_record("missing-modifier", "2026-04-07T10:00:00Z");
    record.speed = Some("turbo".into());
    database.insert_records(&[record]).unwrap();
    drop(database);

    let output = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--provider",
            "claude-code",
            "--json",
            "-d",
        ],
    );
    assert_failure(&output);
    assert!(
        output.stdout.is_empty(),
        "pricing failure should not write JSON stdout; stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing pricing coverage"));
    assert_eq!(stderr_field_value(&stderr, "provider"), Some("claude-code"));
    assert_eq!(
        stderr_field_value(&stderr, "model"),
        Some("claude-opus-4-6")
    );
    assert_eq!(stderr_field_value(&stderr, "category"), Some("input"));
    assert!(stderr.contains("speed=turbo"));
    assert!(
        stderr.contains("usage range 2026-04-07T10:00:00+00:00 to 2026-04-07T10:00:00+00:00"),
        "stderr did not contain usage range:\n{stderr}"
    );

    let table = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--provider",
            "claude-code",
            "-d",
        ],
    );
    assert_failure(&table);
    assert!(
        table.stdout.is_empty(),
        "pricing failure should not write table stdout; stdout:\n{}",
        String::from_utf8_lossy(&table.stdout)
    );
    let stderr = String::from_utf8_lossy(&table.stderr);
    assert!(stderr.contains("missing pricing coverage"));
    assert!(stderr.contains("speed=turbo"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_cost_report_overlapping_component_prices_writes_no_csv_stdout() {
    let root = temp_root("pricing-overlapping-component-prices");
    fs::create_dir_all(&root).unwrap();
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("tkstat.db");
    let database = Database::open(&db).unwrap();
    for (from, rate) in [
        ("2026-01-01T00:00:00Z", 10.0),
        ("2026-02-01T00:00:00Z", 20.0),
    ] {
        database
            .insert_pricing_interval(&PricingInterval::usd(
                tkstat::domain::provider::ProviderId::ClaudeCode,
                "claude-opus-4-6",
                TokenCategory::Input,
                rate,
                from.parse().unwrap(),
                "e2e",
            ))
            .unwrap();
    }
    database
        .insert_records(&[cli_claude_record(
            "overlapping-prices",
            "2026-04-07T10:00:00Z",
        )])
        .unwrap();
    drop(database);

    let output = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--provider",
            "claude-code",
            "--csv",
            "-d",
        ],
    );
    assert_failure(&output);
    assert!(
        output.stdout.is_empty(),
        "pricing overlap failure should not write CSV stdout; stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("overlapping pricing intervals"));
    assert_eq!(stderr_field_value(&stderr, "provider"), Some("claude-code"));
    assert_eq!(
        stderr_field_value(&stderr, "model"),
        Some("claude-opus-4-6")
    );
    assert_eq!(stderr_field_value(&stderr, "category"), Some("input"));
    assert!(
        stderr.contains("usage range 2026-04-07T10:00:00+00:00 to 2026-04-07T10:00:00+00:00"),
        "stderr did not contain usage range:\n{stderr}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_pricing_import_imports_reviewed_catalog_file() {
    let root = temp_root("pricing-import-reviewed-catalog");
    fs::create_dir_all(&root).unwrap();
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("tkstat.db");
    let catalog = root.join("reviewed-catalog.json");
    fs::write(
        &catalog,
        cli_pricing_catalog("claude-opus-4-6", 13.0, "2026-01-01T00:00:00+00:00"),
    )
    .unwrap();

    let output = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--pricing-import",
            catalog.to_str().unwrap(),
        ],
    );
    assert_success(&output);
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("imported pricing catalog with 1 interval changes")
    );

    let conn = Connection::open(&db).unwrap();
    let rate: f64 = conn
        .query_row(
            "SELECT rate_per_1m_tokens FROM pricing_intervals
             WHERE provider = 'claude-code'
               AND model_id = 'claude-opus-4-6'
               AND token_category = 'input'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rate, 13.0);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_update_pricing_catalog_script_parses_sanitized_fixtures_offline() {
    let root = temp_root("pricing-catalog-updater-fixtures");
    fs::create_dir_all(&root).unwrap();
    let output_catalog = root.join("generated-catalog.json");

    let output = Command::new("python3")
        .args([
            "scripts/update_pricing_catalog.py",
            "--from-fixtures",
            "--anthropic-html",
            "tests/fixtures/pricing/anthropic_pricing.html",
            "--openai-json",
            "tests/fixtures/pricing/openai_pricing.json",
            "--retrieved-at",
            "2026-05-24",
            "--effective-from",
            "2026-05-24T00:00:00+00:00",
            "--output",
            output_catalog.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "catalog updater failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let generated: Value = serde_json::from_slice(&fs::read(&output_catalog).unwrap()).unwrap();
    let entries = generated["entries"].as_array().unwrap();
    assert!(entries.iter().any(|entry| {
        entry["provider"] == "claude-code"
            && entry["model_ids"]
                .as_array()
                .unwrap()
                .iter()
                .any(|model| model == "claude-opus-fixture")
    }));
    assert!(entries.iter().any(|entry| {
        entry["provider"] == "claude-code"
            && entry["dimensions"]["source_detail"] == "ephemeral_1h"
            && entry["model_ids"]
                .as_array()
                .unwrap()
                .iter()
                .any(|model| model == "claude-opus-fixture")
    }));
    assert!(entries.iter().any(|entry| {
        entry["provider"] == "codex"
            && entry["dimensions"]["processing_mode"] == "batch"
            && entry["model_ids"]
                .as_array()
                .unwrap()
                .iter()
                .any(|model| model == "gpt-fixture-codex-batch")
    }));

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

fn cli_claude_record(request_id: &str, timestamp: &str) -> TokenRecord {
    TokenRecord {
        provider: tkstat::domain::provider::ProviderId::ClaudeCode,
        request_id: request_id.into(),
        session_id: "cli-pricing-session".into(),
        uuid: format!("{request_id}-uuid"),
        timestamp: timestamp.parse().unwrap(),
        model: ModelFamily::Opus,
        model_id: "claude-opus-4-6".into(),
        input_tokens: 1_000_000,
        output_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        cached_input_tokens: 0,
        reasoning_output_tokens: 0,
        cache_creation_5m_tokens: 0,
        cache_creation_1h_tokens: 0,
        service_tier: None,
        speed: None,
        region: None,
        processing_mode: None,
        cost_usd: 0.0,
        project: "pricing-e2e".into(),
        source_file: "/pricing-e2e.jsonl".into(),
        is_subagent: false,
    }
}

fn cli_pricing_catalog(model_id: &str, rate: f64, effective_from: &str) -> String {
    format!(
        r#"{{
  "schema_version": 1,
  "notes": "offline pricing snapshot test catalog",
  "sources": [
    {{
      "id": "reviewed-source",
      "url": "https://example.com/pricing",
      "retrieved_at": "2026-05-23",
      "notes": "reviewed test source"
    }}
  ],
  "entries": [
    {{
      "provider": "claude-code",
      "model_ids": ["{model_id}"],
      "model_aliases": ["opus"],
      "currency": "USD",
      "effective_from": "{effective_from}",
      "effective_to": null,
      "source": "seed:reviewed-source",
      "source_ref": "reviewed-source",
      "dimensions": {{}},
      "rates_per_1m_tokens": {{
        "input": {rate}
      }},
      "notes": "reviewed test entry"
    }}
  ]
}}"#
    )
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
fn test_pricing_audit_json_reports_machine_readable_freshness_findings() {
    let root = temp_root("pricing-audit-source-freshness-json");
    fs::create_dir_all(&root).unwrap();
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("tkstat.db");
    let database = Database::open(&db).unwrap();
    let mut interval = PricingInterval::usd(
        tkstat::domain::provider::ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z".parse().unwrap(),
        "reviewed:old-source",
    );
    interval.effective_to = None;
    database.insert_pricing_interval(&interval).unwrap();
    tkstat::db::pricing::upsert_source_metadata(
        database.conn(),
        &tkstat::db::pricing::PricingSourceMetadata {
            source: "reviewed:old-source".into(),
            source_url: "https://example.com/pricing".into(),
            source_retrieved_at: "2025-01-01".into(),
            catalog_version: "1".into(),
            source_kind: "reviewed".into(),
            notes: "reviewed stale test source".into(),
        },
    )
    .unwrap();
    database
        .insert_records(&[cli_claude_record("freshness-r1", "2026-04-07T10:00:00Z")])
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
    assert_success(&output);
    let json = parse_stdout_json(&output);
    let findings = json.as_array().unwrap();
    assert!(findings.iter().any(|finding| {
        finding["kind"] == "StaleSource"
            && finding["severity"] == "Warning"
            && finding["provider"] == "claude-code"
            && finding["model_id"] == "claude-opus-4-6"
            && finding["token_category"] == "input"
            && finding["remediation"]
                .as_str()
                .unwrap()
                .contains("reviewed:old-source")
    }));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_cost_reports_do_not_fail_for_source_quality_warnings() {
    let root = temp_root("pricing-source-quality-cost-report");
    fs::create_dir_all(&root).unwrap();
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("tkstat.db");
    let database = Database::open(&db).unwrap();
    let interval = PricingInterval::usd(
        tkstat::domain::provider::ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z".parse().unwrap(),
        "reviewed:old-source",
    );
    database.insert_pricing_interval(&interval).unwrap();
    tkstat::db::pricing::upsert_source_metadata(
        database.conn(),
        &tkstat::db::pricing::PricingSourceMetadata {
            source: "reviewed:old-source".into(),
            source_url: "https://example.com/pricing".into(),
            source_retrieved_at: "2025-01-01".into(),
            catalog_version: "1".into(),
            source_kind: "reviewed".into(),
            notes: "reviewed stale test source".into(),
        },
    )
    .unwrap();
    database
        .insert_records(&[cli_claude_record(
            "source-quality-r1",
            "2026-04-07T10:00:00Z",
        )])
        .unwrap();
    drop(database);

    let output = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--provider",
            "claude",
            "--json",
            "-d",
        ],
    );
    assert_success(&output);
    assert_no_pricing_coverage_error(&output);
    let json = parse_stdout_json(&output);
    let cost = json
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["period"] == "2026-04-07")
        .and_then(|row| row["cost_usd"].as_f64())
        .unwrap();
    assert_eq!(cost, 10.0);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_cost_explain_json_reports_confidence_and_assumptions() {
    let root = temp_root("cost-explain-json");
    fs::create_dir_all(&root).unwrap();
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("tkstat.db");
    let database = Database::open(&db).unwrap();
    database.seed_pricing().unwrap();
    database
        .insert_records(&[cli_claude_record("cost-explain-r1", "2026-04-07T10:00:00Z")])
        .unwrap();
    drop(database);

    let output = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--provider",
            "claude-code",
            "--cost-explain",
            "--json",
        ],
    );
    assert_success(&output);
    let json = parse_stdout_json(&output);
    assert_eq!(json["confidence"], "Estimated");
    assert_eq!(json["component_count"], 1);
    assert!(json["cost_usd"].as_f64().unwrap() > 0.0);
    assert!(
        json["assumptions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|assumption| assumption["kind"] == "BundledPricingSource")
    );

    let text = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--provider",
            "claude-code",
            "--cost-explain",
        ],
    );
    assert_success(&text);
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("cost explain"));
    assert!(stdout.contains("Estimated"));
    assert!(stdout.contains("BundledPricingSource"));

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
            ('manual-provider', 'r1', 's1', 'u1', '2026-04-07T10:00:00+00:00', 'unknown', 'manual-model',
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
