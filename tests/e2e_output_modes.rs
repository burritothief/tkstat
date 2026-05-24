//! Fixture-driven black-box CLI tests.

mod support;
use support::*;

#[test]
fn test_primary_report_output_modes_e2e() {
    let root = temp_root("primary-report-modes");
    let (projects, db) = setup_ingested_corpus(&root, "primary-report-modes");
    let base = [
        "--provider",
        "all",
        "--db",
        db.to_str().unwrap(),
        "--data-dir",
        projects.to_str().unwrap(),
    ];

    let daily = run_tkstat(&root, base.into_iter().chain(["-d", "--limit", "200"]));
    assert_success(&daily);
    let daily_stdout = String::from_utf8_lossy(&daily.stdout);
    assert!(daily_stdout.contains("all providers / daily"));
    assert!(daily_stdout.contains("2026-01-31"));
    assert!(daily_stdout.contains("2026-05-23"));
    assert!(daily_stdout.contains("$"));

    let summary = run_tkstat(&root, base.into_iter().chain(["--summary"]));
    assert_success(&summary);
    let summary_stdout = String::from_utf8_lossy(&summary.stdout);
    assert!(summary_stdout.contains("all providers / summary"));
    assert!(summary_stdout.contains("Requests:"));
    assert!(summary_stdout.contains("Cost:"));

    let top = run_tkstat(&root, base.into_iter().chain(["-t", "3"]));
    assert_success(&top);
    let top_stdout = String::from_utf8_lossy(&top.stdout);
    assert!(top_stdout.contains("all providers / top days"));
    assert!(top_stdout.contains("2026-05-23"));

    let by_model = run_tkstat(
        &root,
        base.into_iter().chain(["--by-model", "--limit", "50"]),
    );
    assert_success(&by_model);
    let by_model_stdout = String::from_utf8_lossy(&by_model.stdout);
    assert!(by_model_stdout.contains("claude-opus-4-5-20251101"));
    assert!(by_model_stdout.contains("gpt-5.5"));

    let by_provider = run_tkstat(&root, base.into_iter().chain(["--by-provider"]));
    assert_success(&by_provider);
    let by_provider_stdout = String::from_utf8_lossy(&by_provider.stdout);
    assert!(by_provider_stdout.contains("claude-code"));
    assert!(by_provider_stdout.contains("codex"));

    let by_project = run_tkstat(&root, base.into_iter().chain(["--by-project"]));
    assert_success(&by_project);
    let by_project_stdout = String::from_utf8_lossy(&by_project.stdout);
    assert!(by_project_stdout.contains("demo"));
    assert!(by_project_stdout.contains("api"));
    assert!(by_project_stdout.contains("tkstat"));

    let json_daily = run_tkstat(
        &root,
        base.into_iter().chain(["--json", "-d", "--limit", "200"]),
    );
    assert_success(&json_daily);
    let json = parse_stdout_json(&json_daily);
    assert!(
        json.as_array()
            .unwrap()
            .iter()
            .any(|row| { row["period"] == "2026-05-23" && row["provider"] == "all providers" })
    );

    let json_model = run_tkstat(&root, base.into_iter().chain(["--by-model", "--json"]));
    assert_success(&json_model);
    let json = parse_stdout_json(&json_model);
    assert!(
        json.as_array()
            .unwrap()
            .iter()
            .any(|row| { row["provider"] == "codex" && row["model_id"] == "gpt-5.5" })
    );

    let csv_daily = run_tkstat(
        &root,
        base.into_iter().chain(["--csv", "-d", "--limit", "200"]),
    );
    assert_success(&csv_daily);
    let csv_daily_stdout = String::from_utf8_lossy(&csv_daily.stdout);
    assert!(csv_daily_stdout.starts_with("period,provider,input_tokens"));
    assert!(csv_daily_stdout.contains("2026-05-23,all providers,"));

    let csv_model = run_tkstat(&root, base.into_iter().chain(["--by-model", "--csv"]));
    assert_success(&csv_model);
    let csv_model_stdout = String::from_utf8_lossy(&csv_model.stdout);
    assert!(csv_model_stdout.starts_with("period,provider,model_id"));
    assert!(csv_model_stdout.contains("codex/gpt-5.5,codex,gpt-5.5"));

    let chart = run_tkstat(&root, base.into_iter().chain(["--chart"]));
    assert_success(&chart);
    assert!(String::from_utf8_lossy(&chart.stdout).contains("all providers / chart"));

    let heatmap = run_tkstat(&root, base.into_iter().chain(["--heatmap", "--no-color"]));
    assert_success(&heatmap);
    assert!(String::from_utf8_lossy(&heatmap.stdout).contains("all providers / heatmap"));

    for output in [
        &daily,
        &summary,
        &top,
        &by_model,
        &by_provider,
        &by_project,
        &json_daily,
        &json_model,
        &csv_daily,
        &csv_model,
        &chart,
        &heatmap,
    ] {
        assert_no_pricing_coverage_error(output);
    }

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_codex_local_midnight_outputs_use_same_report_local_date() {
    let root = temp_root("codex-local-midnight");
    make_codex_fixture(&root);
    let db = root.join("tkstat.db");
    let seed = run_tkstat(&root, ["--pricing-seed", "--db", db.to_str().unwrap()]);
    assert_success(&seed);

    let base = [
        "--provider",
        "codex",
        "--db",
        db.to_str().unwrap(),
        "--no-color",
    ];

    let daily = run_tkstat(&root, base.into_iter().chain(["-d", "--limit", "10"]));
    assert_success(&daily);
    let daily_stdout = String::from_utf8_lossy(&daily.stdout);
    assert!(daily_stdout.contains("codex / daily"));
    assert!(daily_stdout.contains("2026-05-23"));
    assert!(!daily_stdout.contains("2026-05-24"));

    let json_daily = run_tkstat(
        &root,
        base.into_iter().chain(["--json", "-d", "--limit", "10"]),
    );
    assert_success(&json_daily);
    let json = parse_stdout_json(&json_daily);
    assert_eq!(json[0]["period"], "2026-05-23");

    let csv_daily = run_tkstat(
        &root,
        base.into_iter().chain(["--csv", "-d", "--limit", "10"]),
    );
    assert_success(&csv_daily);
    assert!(
        String::from_utf8_lossy(&csv_daily.stdout).contains("2026-05-23,codex,"),
        "CSV output should use the same report-local day as table and JSON"
    );

    let chart = run_tkstat(
        &root,
        base.into_iter()
            .chain(["--chart", "--chart-metric", "tokens"]),
    );
    assert_success(&chart);
    assert!(String::from_utf8_lossy(&chart.stdout).contains("2026-05-23"));

    let heatmap = run_tkstat(
        &root,
        base.into_iter()
            .chain(["--heatmap", "--chart-metric", "tokens"]),
    );
    assert_success(&heatmap);
    assert!(String::from_utf8_lossy(&heatmap.stdout).contains("codex / heatmap"));

    let budget_warning = run_tkstat(
        &root,
        base.into_iter()
            .chain(["-d", "--limit", "10", "--daily-budget-usd", "0.000001"]),
    );
    assert_success(&budget_warning);
    assert!(
        String::from_utf8_lossy(&budget_warning.stderr).contains("daily 2026-05-23"),
        "daily budget warning should use the same report-local day as reports"
    );

    for output in [
        &daily,
        &json_daily,
        &csv_daily,
        &chart,
        &heatmap,
        &budget_warning,
    ] {
        assert_no_pricing_coverage_error(output);
    }

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_default_daily_ingests_and_reuses_database() {
    let root = temp_root("default-daily");
    let projects = make_claude_fixture(&root);
    let db = root.join("tkstat.db");
    seed_pricing(&root, &db, &projects);

    let args = [
        "--db",
        db.to_str().unwrap(),
        "--data-dir",
        projects.to_str().unwrap(),
        "--provider",
        "claude",
        "--no-color",
    ];
    let first = run_tkstat(&root, args);
    assert_success(&first);
    let stdout = String::from_utf8_lossy(&first.stdout);
    assert!(stdout.contains("claude-code / daily"));
    assert!(stdout.contains("2026-04-07"));
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(stderr.contains("ingested 3 new records"));
    assert!(!stderr.contains("tkstat warning"));

    let second = run_tkstat(&root, args);
    assert_success(&second);
    assert!(!String::from_utf8_lossy(&second.stderr).contains("ingested"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_json_daily_filters_no_subagents_and_force_update() {
    let root = temp_root("json-filters");
    let projects = make_claude_fixture(&root);
    let db = root.join("tkstat.db");
    seed_pricing(&root, &db, &projects);

    let base_args = [
        "--db",
        db.to_str().unwrap(),
        "--data-dir",
        projects.to_str().unwrap(),
        "--provider",
        "claude",
        "--json",
        "-d",
    ];
    let output = run_tkstat(&root, base_args);
    assert_success(&output);
    let json = parse_stdout_json(&output);
    assert_eq!(json[0]["provider"], "claude-code");
    assert_eq!(json[0]["request_count"], 3);

    let no_subagents = run_tkstat(
        &root,
        base_args
            .into_iter()
            .chain(["--no-subagents"])
            .collect::<Vec<_>>(),
    );
    assert_success(&no_subagents);
    let json = parse_stdout_json(&no_subagents);
    assert_eq!(json[0]["request_count"], 2);

    let force = run_tkstat(
        &root,
        base_args
            .into_iter()
            .chain(["--force-update"])
            .collect::<Vec<_>>(),
    );
    assert_success(&force);
    let json = parse_stdout_json(&force);
    assert_eq!(json[0]["request_count"], 3);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_by_model_json_and_model_filters() {
    let root = temp_root("by-model");
    let projects = make_claude_fixture(&root);
    let db = root.join("tkstat.db");
    seed_pricing(&root, &db, &projects);

    let by_model = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--provider",
            "claude",
            "--by-model",
            "--json",
        ],
    );
    assert_success(&by_model);
    let rows = parse_stdout_json(&by_model);
    let rows = rows.as_array().unwrap();
    assert!(
        rows.iter().any(|row| {
            row["provider"] == "claude-code" && row["model_id"] == "claude-opus-4-6"
        })
    );

    let exact = run_tkstat(
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
            "--model",
            "claude-opus-4-6",
        ],
    );
    assert_success(&exact);
    let json = parse_stdout_json(&exact);
    assert_eq!(json[0]["request_count"], 1);
    assert_eq!(json[0]["input_tokens"], 30);

    let family = run_tkstat(
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
            "--model",
            "sonnet",
        ],
    );
    assert_success(&family);
    let json = parse_stdout_json(&family);
    assert_eq!(json[0]["request_count"], 2);
    assert_eq!(json[0]["input_tokens"], 60);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_by_provider_json_report() {
    let root = temp_root("by-provider");
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
            "--provider",
            "claude",
            "--by-provider",
            "--json",
        ],
    );
    assert_success(&output);
    let json = parse_stdout_json(&output);
    assert_eq!(json[0]["period"], "claude-code");
    assert_eq!(json[0]["provider"], "claude-code");
    assert_eq!(json[0]["request_count"], 3);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_by_project_json_report_and_project_filter() {
    let root = temp_root("by-project");
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
            "--provider",
            "claude",
            "--by-project",
            "--json",
        ],
    );
    assert_success(&output);
    let json = parse_stdout_json(&output);
    assert_eq!(json[0]["period"], "demo");
    assert_eq!(json[0]["project"], "demo");
    assert_eq!(json[0]["request_count"], 3);

    let filtered = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--provider",
            "claude",
            "--by-project",
            "--project",
            "demo",
            "--json",
        ],
    );
    assert_success(&filtered);
    let json = parse_stdout_json(&filtered);
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["project"], "demo");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_csv_daily_and_by_model_reports() {
    let root = temp_root("csv");
    let projects = make_claude_fixture(&root);
    let db = root.join("tkstat.db");
    seed_pricing(&root, &db, &projects);

    let daily = run_tkstat(
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
            "--columns",
            "input,total,reqs",
        ],
    );
    assert_success(&daily);
    let stdout = String::from_utf8_lossy(&daily.stdout);
    let rows: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        rows[0],
        "period,provider,input_tokens,total_tokens,request_count"
    );
    assert!(
        rows.iter()
            .any(|row| row.starts_with("2026-04-07,claude-code,90,"))
    );
    assert!(rows.iter().any(|row| row.ends_with(",3")));

    let by_model = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--provider",
            "claude",
            "--by-model",
            "--csv",
            "--columns",
            "input,reqs",
        ],
    );
    assert_success(&by_model);
    let stdout = String::from_utf8_lossy(&by_model.stdout);
    assert!(
        stdout
            .lines()
            .next()
            .unwrap()
            .starts_with("period,provider,model_id,input_tokens,request_count")
    );
    assert!(stdout.contains("claude-code/claude-opus-4-6,claude-code,claude-opus-4-6,30,1"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_codex_provider_specific_token_columns_csv() {
    let root = temp_root("codex-provider-columns");
    make_codex_fixture(&root);
    let projects = root.join("empty-projects");
    fs::create_dir_all(&projects).unwrap();
    let db = root.join("tkstat.db");
    seed_pricing(&root, &db, &projects);

    let output = run_tkstat(
        &root,
        [
            "--db",
            db.to_str().unwrap(),
            "--provider",
            "codex",
            "--by-model",
            "--csv",
            "--columns",
            "cached_input,reasoning_output",
        ],
    );
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("period,provider,model_id,cached_input_tokens,reasoning_output_tokens")
    );
    assert!(stdout.contains("codex/gpt-5.5,codex,gpt-5.5,40,7"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_budget_warnings_use_active_filters_and_keep_json_stdout_clean() {
    let root = temp_root("budget");
    let projects = make_claude_fixture(&root);
    let db = root.join("tkstat.db");
    seed_pricing(&root, &db, &projects);

    let warning = run_tkstat(
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
            "--daily-budget-usd",
            "0.002",
        ],
    );
    assert_success(&warning);
    let json = parse_stdout_json(&warning);
    assert_eq!(json[0]["request_count"], 3);
    let stderr = String::from_utf8_lossy(&warning.stderr);
    assert!(stderr.contains("budget warning"));
    assert!(stderr.contains("daily 2026-04-07"));
    assert!(stderr.contains("provider: claude-code"));

    let token_columns_warning = run_tkstat(
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
            "--columns",
            "input,total",
            "--daily-budget-usd",
            "0.002",
        ],
    );
    assert_success(&token_columns_warning);
    let stdout = String::from_utf8_lossy(&token_columns_warning.stdout);
    assert!(stdout.starts_with("period,provider,input_tokens,total_tokens"));
    assert!(stdout.contains("2026-04-07,claude-code,"));
    assert!(!stdout.contains("cost"));
    let stderr = String::from_utf8_lossy(&token_columns_warning.stderr);
    assert!(stderr.contains("budget warning"));
    assert!(stderr.contains("daily 2026-04-07"));
    assert_no_pricing_coverage_error(&token_columns_warning);

    let filtered = run_tkstat(
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
            "--model",
            "claude-opus-4-6",
            "--daily-budget-usd",
            "0.002",
        ],
    );
    assert_success(&filtered);
    assert!(!String::from_utf8_lossy(&filtered.stderr).contains("budget warning"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_budget_report_json_includes_selected_range_and_top_contributors() {
    let root = temp_root("budget-report");
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
            "--provider",
            "claude",
            "--budget",
            "--json",
            "-b",
            "2026-04-07",
            "-e",
            "2026-04-07",
            "--daily-budget-usd",
            "5.00",
            "--monthly-budget-usd",
            "100.00",
        ],
    );
    assert_success(&output);
    let json = parse_stdout_json(&output);
    let rows = json.as_array().unwrap();
    assert!(rows.iter().any(|row| row["label"] == "today"));
    assert!(rows.iter().any(|row| row["label"] == "month-to-date"));
    let selected = rows.iter().find(|row| row["label"] == "selected").unwrap();
    assert_eq!(selected["begin"], "2026-04-07");
    assert_eq!(selected["end"], "2026-04-07");
    assert!(selected["cost_usd"].as_f64().unwrap() > 0.0);
    assert_eq!(selected["top_provider"], "claude-code");
    assert_eq!(selected["top_project"], "demo");

    let _ = fs::remove_dir_all(root);
}
