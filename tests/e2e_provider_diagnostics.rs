//! Fixture-driven black-box CLI tests.

mod support;
use support::*;

#[test]
fn test_provider_selection_uses_isolated_sources_and_does_not_duplicate() {
    let root = temp_root("provider-selection-isolation");
    let projects = make_claude_corpus_fixture(&root);
    make_codex_fixture(&root);

    let claude_db = root.join("claude-only.db");
    let claude_seed = run_tkstat(
        &root,
        [
            "--pricing-seed",
            "--db",
            claude_db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
        ],
    );
    assert_success(&claude_seed);
    let claude = run_tkstat(
        &root,
        [
            "--provider",
            "claude",
            "--force-update",
            "--db",
            claude_db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-model",
            "--json",
        ],
    );
    assert_success(&claude);
    assert_no_pricing_coverage_error(&claude);
    let claude_rows = parse_stdout_json(&claude);
    assert!(claude_rows.as_array().unwrap().iter().any(|row| {
        row["provider"] == "claude-code" && row["model_id"] == "claude-opus-4-5-20251101"
    }));
    assert!(
        !claude_rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["model_id"] == "gpt-5.5")
    );
    assert_eq!(request_count_sum(&claude_rows), 4);

    let claude_again = run_tkstat(
        &root,
        [
            "--provider",
            "claude",
            "--db",
            claude_db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-model",
            "--json",
        ],
    );
    assert_success(&claude_again);
    assert_eq!(request_count_sum(&parse_stdout_json(&claude_again)), 4);

    let claude_canonical = run_tkstat(
        &root,
        [
            "--provider",
            "claude-code",
            "--db",
            claude_db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-provider",
            "--json",
        ],
    );
    assert_success(&claude_canonical);
    let canonical_rows = parse_stdout_json(&claude_canonical);
    assert_eq!(
        provider_request_count(&canonical_rows, "claude-code"),
        Some(4)
    );
    assert_eq!(request_count_sum(&canonical_rows), 4);

    let claude_conn = Connection::open(&claude_db).unwrap();
    for table in ["token_usage", "file_state", "pricing_intervals"] {
        let legacy_count: i64 = claude_conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE provider = 'claude'"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        let canonical_count: i64 = claude_conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE provider = 'claude-code'"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_count, 0, "{table} should not store the claude alias");
        assert!(
            canonical_count > 0,
            "{table} should store canonical claude-code rows"
        );
    }

    let codex_db = root.join("codex-only.db");
    let codex_seed = run_tkstat(
        &root,
        [
            "--pricing-seed",
            "--db",
            codex_db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
        ],
    );
    assert_success(&codex_seed);
    let codex = run_tkstat(
        &root,
        [
            "--provider",
            "codex",
            "--force-update",
            "--db",
            codex_db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-model",
            "--json",
        ],
    );
    assert_success(&codex);
    assert_no_pricing_coverage_error(&codex);
    let codex_rows = parse_stdout_json(&codex);
    assert!(
        codex_rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["provider"] == "codex" && row["model_id"] == "gpt-5.5" })
    );
    assert!(
        !codex_rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["model_id"] == "claude-opus-4-5-20251101")
    );
    assert_eq!(request_count_sum(&codex_rows), 1);

    let codex_again = run_tkstat(
        &root,
        [
            "--provider",
            "codex",
            "--db",
            codex_db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-model",
            "--json",
        ],
    );
    assert_success(&codex_again);
    assert_eq!(request_count_sum(&parse_stdout_json(&codex_again)), 1);

    let all_db = root.join("all-providers.db");
    let all_seed = run_tkstat(
        &root,
        [
            "--pricing-seed",
            "--db",
            all_db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
        ],
    );
    assert_success(&all_seed);
    let all = run_tkstat(
        &root,
        [
            "--provider",
            "all",
            "--force-update",
            "--db",
            all_db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-provider",
            "--json",
        ],
    );
    assert_success(&all);
    assert_no_pricing_coverage_error(&all);
    let all_rows = parse_stdout_json(&all);
    assert_eq!(provider_request_count(&all_rows, "claude-code"), Some(4));
    assert_eq!(provider_request_count(&all_rows, "codex"), Some(1));
    assert_eq!(request_count_sum(&all_rows), 5);

    let all_models = run_tkstat(
        &root,
        [
            "--provider",
            "all",
            "--db",
            all_db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-model",
            "--json",
        ],
    );
    assert_success(&all_models);
    let all_model_rows = parse_stdout_json(&all_models);
    assert!(all_model_rows.as_array().unwrap().iter().any(|row| {
        row["provider"] == "claude-code" && row["model_id"] == "claude-opus-4-5-20251101"
    }));
    assert!(
        all_model_rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["provider"] == "codex" && row["model_id"] == "gpt-5.5" })
    );

    let all_again = run_tkstat(
        &root,
        [
            "--provider",
            "all",
            "--db",
            all_db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-provider",
            "--json",
        ],
    );
    assert_success(&all_again);
    let all_again_rows = parse_stdout_json(&all_again);
    assert_eq!(
        provider_request_count(&all_again_rows, "claude-code"),
        Some(4)
    );
    assert_eq!(provider_request_count(&all_again_rows, "codex"), Some(1));
    assert_eq!(request_count_sum(&all_again_rows), 5);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_filters_and_subagent_behavior_e2e() {
    let root = temp_root("filters-subagents");
    let projects = make_filter_corpus_fixture(&root);
    let db = root.join("tkstat.db");
    seed_pricing(&root, &db, &projects);
    let base = [
        "--provider",
        "claude",
        "--db",
        db.to_str().unwrap(),
        "--data-dir",
        projects.to_str().unwrap(),
    ];

    let exact_model = run_tkstat(
        &root,
        base.into_iter().chain([
            "--force-update",
            "--model",
            "claude-opus-4-5-20251101",
            "--by-model",
            "--json",
        ]),
    );
    assert_success(&exact_model);
    assert_no_pricing_coverage_error(&exact_model);
    let exact_rows = parse_stdout_json(&exact_model);
    assert_eq!(request_count_sum(&exact_rows), 1);
    assert_eq!(exact_rows.as_array().unwrap().len(), 1);
    assert_eq!(
        exact_rows[0]["model_id"],
        serde_json::json!("claude-opus-4-5-20251101")
    );

    let sonnet_family = run_tkstat(
        &root,
        base.into_iter()
            .chain(["--model-family", "sonnet", "--by-model", "--json"]),
    );
    assert_success(&sonnet_family);
    let sonnet_rows = parse_stdout_json(&sonnet_family);
    assert_eq!(request_count_sum(&sonnet_rows), 4);
    assert!(sonnet_rows.as_array().unwrap().iter().any(|row| {
        row["model_id"] == "claude-sonnet-4-5-20250929" && row["request_count"] == 3
    }));
    assert!(
        sonnet_rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["model_id"] == "claude-sonnet-4-6" && row["request_count"] == 1 })
    );
    assert!(
        !sonnet_rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["model_id"] == "claude-opus-4-5-20251101")
    );

    let project = run_tkstat(
        &root,
        base.into_iter()
            .chain(["--project", "api", "--by-project", "--json"]),
    );
    assert_success(&project);
    let project_rows = parse_stdout_json(&project);
    assert_eq!(project_rows.as_array().unwrap().len(), 1);
    assert_eq!(project_rows[0]["project"], serde_json::json!("api"));
    assert_eq!(request_count_sum(&project_rows), 1);

    let session = run_tkstat(
        &root,
        base.into_iter()
            .chain(["--session", "corpus-session-main", "--by-model", "--json"]),
    );
    assert_success(&session);
    let session_rows = parse_stdout_json(&session);
    assert_eq!(request_count_sum(&session_rows), 2);
    assert!(
        session_rows.as_array().unwrap().iter().any(|row| {
            row["model_id"] == "claude-opus-4-5-20251101" && row["request_count"] == 1
        })
    );
    assert!(session_rows.as_array().unwrap().iter().any(|row| {
        row["model_id"] == "claude-sonnet-4-5-20250929" && row["request_count"] == 1
    }));
    assert!(
        !session_rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["model_id"] == "claude-sonnet-4-6")
    );

    let date_range = run_tkstat(
        &root,
        base.into_iter().chain([
            "--json",
            "-d",
            "--utc",
            "--begin",
            "2026-02-01",
            "--end",
            "2026-02-02",
        ]),
    );
    assert_success(&date_range);
    let date_rows = parse_stdout_json(&date_range);
    assert_eq!(request_count_sum(&date_rows), 3);
    assert!(
        date_rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["period"] == "2026-02-01" && row["request_count"] == 2 })
    );
    assert!(
        date_rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["period"] == "2026-02-02" && row["request_count"] == 1 })
    );
    assert!(
        !date_rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["period"] == "2026-01-31" || row["period"] == "2026-02-03")
    );

    let with_subagents = run_tkstat(
        &root,
        base.into_iter().chain([
            "--json",
            "-d",
            "--utc",
            "--begin",
            "2026-02-01",
            "--end",
            "2026-02-01",
        ]),
    );
    assert_success(&with_subagents);
    assert_eq!(request_count_sum(&parse_stdout_json(&with_subagents)), 2);

    let no_subagents = run_tkstat(
        &root,
        base.into_iter().chain([
            "--json",
            "-d",
            "--utc",
            "--begin",
            "2026-02-01",
            "--end",
            "2026-02-01",
            "--no-subagents",
        ]),
    );
    assert_success(&no_subagents);
    assert_eq!(request_count_sum(&parse_stdout_json(&no_subagents)), 1);

    for output in [
        &exact_model,
        &sonnet_family,
        &project,
        &session,
        &date_range,
        &with_subagents,
        &no_subagents,
    ] {
        assert_no_pricing_coverage_error(output);
    }

    let _ = fs::remove_dir_all(root);
}

fn request_count_sum(rows: &Value) -> i64 {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| row["request_count"].as_i64().unwrap())
        .sum()
}

fn provider_request_count(rows: &Value, provider: &str) -> Option<i64> {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == provider)
        .and_then(|row| row["request_count"].as_i64())
}

#[test]
fn test_empty_data_dir_json_succeeds() {
    let root = temp_root("empty");
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("tkstat.db");
    seed_pricing(&root, &db, &projects);

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
    let json = parse_stdout_json(&output);
    assert_eq!(json.as_array().unwrap().len(), 0);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_ingestion_warning_for_missing_selected_provider_keeps_json_stdout_clean() {
    let root = temp_root("missing-selected-provider");
    fs::create_dir_all(&root).unwrap();
    let db = root.join("tkstat.db");

    let output = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--provider",
            "claude",
            "--json",
            "-d",
        ],
    );
    assert_success(&output);
    let json = parse_stdout_json(&output);
    assert_eq!(json.as_array().unwrap().len(), 0);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("claude-code source path is not configured"));
    assert!(stderr.contains("no usage records found"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("tkstat warning"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_ingestion_warning_for_malformed_jsonl_keeps_csv_stdout_clean() {
    let root = temp_root("malformed-jsonl");
    let projects = root.join("claude").join("projects");
    let project_dir = projects.join("-home-tester-work-demo");
    fs::create_dir_all(&project_dir).unwrap();
    fs::write(project_dir.join("bad.jsonl"), "not-json\n").unwrap();
    let db = root.join("tkstat.db");

    let output = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--provider",
            "claude",
            "--csv",
            "-d",
        ],
    );
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("period,"));
    assert!(!stdout.contains("tkstat warning"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("skipped 1 malformed JSONL line"));
    assert!(stderr.contains("no usage records found"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_doctor_reports_healthy_claude_only_state() {
    let root = temp_root("doctor-healthy");
    let projects = make_claude_fixture(&root);
    let db = root.join("tkstat.db");
    seed_pricing(&root, &db, &projects);

    let ingest = run_tkstat(
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
    assert_success(&ingest);

    let doctor = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--doctor",
        ],
    );
    assert_success(&doctor);
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(stdout.contains("tkstat doctor"));
    assert!(stdout.contains("schema: current"));
    assert!(stdout.contains("claude-code: available"));
    assert!(stdout.contains("usage rows: 3"));
    assert!(stdout.contains("status: available"));

    let json = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--doctor",
            "--json",
        ],
    );
    assert_success(&json);
    let json = parse_stdout_json(&json);
    assert_eq!(json["schema"]["status"], "Current");
    assert_eq!(json["usage"]["total_rows"], 3);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_doctor_empty_setup_does_not_create_database() {
    let root = temp_root("doctor-empty");
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("missing.db");

    let doctor = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--doctor",
        ],
    );
    assert_success(&doctor);
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(stdout.contains("schema: missing"));
    assert!(stdout.contains("pricing table is missing"));
    assert!(!db.exists());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_doctor_reports_missing_provider_directory_as_warning() {
    let root = temp_root("doctor-missing-provider");
    let db = root.join("missing.db");
    let missing_projects = root.join("does-not-exist");

    let doctor = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            missing_projects.to_str().unwrap(),
            "--doctor",
        ],
    );
    assert_success(&doctor);
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(stdout.contains("claude-code: missing"));
    assert!(stdout.contains("claude-code source path is missing"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_doctor_exits_nonzero_for_old_schema() {
    let root = temp_root("doctor-old-schema");
    fs::create_dir_all(&root).unwrap();
    let db = root.join("old.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_version (version INTEGER NOT NULL);
         INSERT INTO schema_version (version) VALUES (1);",
    )
    .unwrap();
    drop(conn);

    let doctor = run_tkstat(&root, ["--db", db.to_str().unwrap(), "--doctor"]);
    assert_failure(&doctor);
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(stdout.contains("Blocking Issues"));
    assert!(stdout.contains("older than expected"));

    let _ = fs::remove_dir_all(root);
}
