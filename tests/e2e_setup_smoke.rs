//! Fixture-driven black-box CLI tests.

mod support;
use support::*;

#[cfg(unix)]
use std::process::Command;

#[test]
fn test_help_documents_local_report_timezone_and_utc_override() {
    let root = temp_root("help-timezone");
    let output = run_tkstat(&root, ["--help"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("system-local report date"));
    assert!(stdout.contains("UTC calendar boundaries"));
    assert!(stdout.contains("tkstat --utc -d"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_recommended_setup_and_reset_command_sequence() {
    let root = temp_root("recommended-sequence");
    let projects = make_claude_corpus_fixture(&root);
    make_codex_fixture(&root);
    let db = root.join("tkstat.db");

    let doctor = run_tkstat(
        &root,
        [
            "--doctor",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
        ],
    );
    assert_success(&doctor);
    let doctor_stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(doctor_stdout.contains(&db.display().to_string()));
    assert!(doctor_stdout.contains("claude-code: available"));
    assert!(doctor_stdout.contains("codex: available"));

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
    assert!(String::from_utf8_lossy(&seed.stdout).contains("seeded"));

    let refresh = run_tkstat(
        &root,
        [
            "--pricing-refresh",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
        ],
    );
    assert_success(&refresh);
    assert!(String::from_utf8_lossy(&refresh.stdout).contains("refreshed pricing catalog"));

    let force = run_tkstat(
        &root,
        [
            "--force-update",
            "--provider",
            "all",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-provider",
        ],
    );
    assert_success(&force);
    assert_no_pricing_coverage_error(&force);
    assert!(String::from_utf8_lossy(&force.stderr).contains("ingested 5 new records"));
    let force_stdout = String::from_utf8_lossy(&force.stdout);
    assert!(force_stdout.contains("all providers / by provider"));
    assert!(force_stdout.contains("claude-code"));
    assert!(force_stdout.contains("codex"));

    let daily = run_tkstat(
        &root,
        [
            "--provider",
            "all",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "-d",
            "--limit",
            "14",
        ],
    );
    assert_success(&daily);
    assert_no_pricing_coverage_error(&daily);
    let daily_stdout = String::from_utf8_lossy(&daily.stdout);
    assert!(daily_stdout.contains("all providers / daily"));
    assert!(daily_stdout.contains("2026-05-23"));
    assert!(daily_stdout.contains("$"));

    let by_model = run_tkstat(
        &root,
        [
            "--provider",
            "all",
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--by-model",
            "--limit",
            "50",
        ],
    );
    assert_success(&by_model);
    assert_no_pricing_coverage_error(&by_model);
    let by_model_stdout = String::from_utf8_lossy(&by_model.stdout);
    assert!(by_model_stdout.contains("all providers / by model"));
    assert!(by_model_stdout.contains("claude-opus-4-5-20251101"));
    assert!(by_model_stdout.contains("claude-sonnet-4-5-20250929"));
    assert!(by_model_stdout.contains("gpt-5.5"));

    let _ = fs::remove_dir_all(root);
}

#[test]
#[cfg(unix)]
fn test_release_gate_builds_cli_ingests_temp_db_and_prints_table() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/e2e_smoke.sh");
    let output = Command::new("bash")
        .arg(script)
        .env_remove("TKSTAT_BIN")
        .env("KEEP_TMP", "0")
        .env("TZ", "America/Los_Angeles")
        .env("TKSTAT_PRICING_REFRESH_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("tkstat e2e smoke passed"));
    assert!(stderr.contains("--force-update"));
    assert!(stderr.contains("--by-provider"));
    assert!(stderr.contains("--by-model"));
    assert!(stderr.contains("-d"));
    assert!(stderr.contains("/target/debug/tkstat"));
    assert!(!stdout.contains("missing pricing coverage"));
    assert!(!stderr.contains("missing pricing coverage"));
    assert!(!stdout.contains("/.claude"));
    assert!(!stderr.contains("/.claude"));
    assert!(!stdout.contains("/.codex"));
    assert!(!stderr.contains("/.codex"));
}

#[test]
#[cfg(unix)]
fn test_operational_script_smoke_runs_with_compiled_binary() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/script_smoke.sh");
    let output = Command::new("bash")
        .arg(script)
        .env("TKSTAT_BIN", env!("CARGO_BIN_EXE_tkstat"))
        .env("TZ", "America/Los_Angeles")
        .env("TKSTAT_PRICING_REFRESH_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("tkstat operational script smoke passed"));
    assert!(stderr.contains("fixture_smoke.sh --help"));
    assert!(stderr.contains("pricing_check.sh --provider invalid"));
    assert!(!stdout.contains("/.claude"));
    assert!(!stderr.contains("/.claude"));
    assert!(!stdout.contains("/.codex"));
    assert!(!stderr.contains("/.codex"));
}
