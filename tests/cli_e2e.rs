//! Fixture-driven black-box CLI tests.
//!
//! Harness contract:
//! - each test owns a unique temp root, database path, Claude fixture tree, and Codex fixture tree;
//! - tests invoke the compiled `tkstat` binary through `CARGO_BIN_EXE_tkstat`;
//! - assertions are semantic and targeted rather than full golden snapshots;
//! - failure messages include command args, status, stdout, and stderr.

use std::ffi::OsStr;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::Value;
use tkstat::db::Database;
use tkstat::domain::pricing::{PricingInterval, TokenCategory};
use tkstat::domain::usage::{ModelFamily, TokenRecord};
use tkstat::ingest::{ClaudeCodeAdapter, CodexAdapter, ProviderAdapter};

const CLAUDE_CORPUS_DEMO_MAIN: &str = include_str!("fixtures/claude/demo/main.jsonl");
const CLAUDE_CORPUS_DEMO_SUBAGENT: &str =
    include_str!("fixtures/claude/demo/subagents/agent.jsonl");
const CLAUDE_CORPUS_API: &str = include_str!("fixtures/claude/api/api.jsonl");
const CODEX_CORPUS_SESSION: &str = include_str!("fixtures/codex/synthetic-codex-session.jsonl");

struct CommandOutput {
    args: Vec<String>,
    output: Output,
}

impl Deref for CommandOutput {
    type Target = Output;

    fn deref(&self) -> &Self::Target {
        &self.output
    }
}

fn temp_root(test_name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "tkstat-cli-e2e-{test_name}-{}-{nanos}",
        std::process::id()
    ))
}

fn assistant_line(
    request_id: &str,
    session_id: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"model":"{model}","usage":{{"input_tokens":{input_tokens},"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":{output_tokens}}}}},"requestId":"{request_id}","uuid":"{request_id}-uuid","timestamp":"2026-04-07T10:00:00Z","sessionId":"{session_id}"}}"#
    )
}

struct AssistantUsageLine<'a> {
    request_id: &'a str,
    session_id: &'a str,
    model: &'a str,
    timestamp: &'a str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
}

fn assistant_line_with_usage(line: AssistantUsageLine<'_>) -> String {
    let AssistantUsageLine {
        request_id,
        session_id,
        model,
        timestamp,
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
    } = line;
    format!(
        r#"{{"type":"assistant","message":{{"model":"{model}","usage":{{"input_tokens":{input_tokens},"cache_creation_input_tokens":{cache_creation_tokens},"cache_read_input_tokens":{cache_read_tokens},"output_tokens":{output_tokens}}}}},"requestId":"{request_id}","uuid":"{request_id}-uuid","timestamp":"{timestamp}","sessionId":"{session_id}"}}"#
    )
}

fn make_claude_fixture(root: &Path) -> PathBuf {
    let projects = root.join("claude").join("projects");
    let project_dir = projects.join("-home-tester-work-demo");
    fs::create_dir_all(&project_dir).unwrap();
    fs::write(
        project_dir.join("main.jsonl"),
        format!(
            "{}\n{}\n",
            assistant_line(
                "req-sonnet",
                "session-main",
                "claude-sonnet-4-5-20250929",
                10,
                20
            ),
            assistant_line("req-opus", "session-main", "claude-opus-4-6", 30, 40)
        ),
    )
    .unwrap();

    let subagent_dir = project_dir.join("session-main").join("subagents");
    fs::create_dir_all(&subagent_dir).unwrap();
    fs::write(
        subagent_dir.join("agent.jsonl"),
        format!(
            "{}\n",
            assistant_line(
                "req-subagent",
                "session-subagent",
                "claude-sonnet-4-5-20250929",
                50,
                60
            )
        ),
    )
    .unwrap();
    projects
}

fn make_claude_corpus_fixture(root: &Path) -> PathBuf {
    let projects = root.join("claude-corpus").join("projects");
    let demo_dir = projects.join("-home-tester-work-demo");
    fs::create_dir_all(&demo_dir).unwrap();
    fs::write(demo_dir.join("main.jsonl"), CLAUDE_CORPUS_DEMO_MAIN).unwrap();

    let subagent_dir = demo_dir.join("corpus-session-main").join("subagents");
    fs::create_dir_all(&subagent_dir).unwrap();
    fs::write(
        subagent_dir.join("agent.jsonl"),
        CLAUDE_CORPUS_DEMO_SUBAGENT,
    )
    .unwrap();

    let api_dir = projects.join("-home-tester-work-api");
    fs::create_dir_all(&api_dir).unwrap();
    fs::write(api_dir.join("api.jsonl"), CLAUDE_CORPUS_API).unwrap();

    projects
}

fn make_filter_corpus_fixture(root: &Path) -> PathBuf {
    let projects = make_claude_corpus_fixture(root);
    let demo_dir = projects.join("-home-tester-work-demo");
    fs::write(
        demo_dir.join("sonnet-46.jsonl"),
        format!(
            "{}\n",
            assistant_line_with_usage(AssistantUsageLine {
                request_id: "filter-sonnet-46",
                session_id: "filter-session-sonnet-46",
                model: "claude-sonnet-4-6",
                timestamp: "2026-02-03T09:00:00Z",
                input_tokens: 9,
                output_tokens: 10,
                cache_creation_tokens: 0,
                cache_read_tokens: 11,
            })
        ),
    )
    .unwrap();
    projects
}

fn make_codex_fixture(root: &Path) -> PathBuf {
    let codex_home = root.join("codex-home");
    let session_dir = codex_home.join("sessions").join("2026/05/24");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("synthetic-codex-session.jsonl"),
        CODEX_CORPUS_SESSION,
    )
    .unwrap();
    codex_home
}

fn parse_adapter_records(adapter: &dyn ProviderAdapter) -> Vec<TokenRecord> {
    let mut records = Vec::new();
    for file in adapter.discover().unwrap() {
        let parsed = adapter.parse_file(&file.path, 0, &file).unwrap();
        records.extend(parsed.records);
    }
    records
}

fn run_tkstat<I, S>(root: &Path, args: I) -> CommandOutput
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect();
    let codex_home = root.join("codex-home");
    let home = root.join("home");
    let claude_config = root.join("claude-config");
    fs::create_dir_all(&codex_home).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&claude_config).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tkstat"))
        .args(&args)
        .env("CODEX_HOME", &codex_home)
        .env("HOME", &home)
        .env("CLAUDE_CONFIG_DIR", &claude_config)
        .output()
        .unwrap();
    CommandOutput { args, output }
}

fn assert_success(output: &CommandOutput) {
    assert!(
        output.status.success(),
        "args: tkstat {}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure(output: &CommandOutput) {
    assert!(
        !output.status.success(),
        "args: tkstat {}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn seed_pricing(root: &Path, db: &Path, projects: &Path) {
    let output = run_tkstat(
        root,
        [
            "--db",
            db.to_str().unwrap(),
            "--data-dir",
            projects.to_str().unwrap(),
            "--provider",
            "claude",
            "--pricing-seed",
        ],
    );
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("seeded"));
}

fn parse_stdout_json(output: &CommandOutput) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "invalid json: {err}\nargs: tkstat {}\nstdout:\n{}\nstderr:\n{}",
            output.args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_no_pricing_coverage_error(output: &CommandOutput) {
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("missing pricing coverage"),
        "args: tkstat {}\noutput contained pricing coverage error:\n{combined}",
        output.args.join(" ")
    );
}

fn assert_missing_pricing_remediation(
    output: &CommandOutput,
    provider: &str,
    model_id: &str,
    category: Option<&str>,
) {
    assert_failure(output);
    assert!(
        output.stdout.is_empty(),
        "pricing failure should not write structured stdout\nargs: tkstat {}\nstdout:\n{}",
        output.args.join(" "),
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing pricing coverage"));
    assert!(stderr.contains(&format!("provider={provider}")));
    assert!(stderr.contains(&format!("model={model_id}")));
    match category {
        Some(category) => assert!(stderr.contains(&format!("category={category}"))),
        None => assert!(stderr.contains("category=")),
    }
    assert!(stderr.contains("usage range"));
    assert!(stderr.contains("tkstat --pricing-refresh"));
    assert!(stderr.contains("tkstat --pricing-seed"));
}

fn setup_ingested_corpus(root: &Path, test_name: &str) -> (PathBuf, PathBuf) {
    let projects = make_claude_corpus_fixture(root);
    make_codex_fixture(root);
    let db = root.join(format!("{test_name}.db"));
    let seed = run_tkstat(
        root,
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
        root,
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
    assert_success(&ingest);
    assert_no_pricing_coverage_error(&ingest);
    (projects, db)
}

#[test]
fn test_e2e_harness_uses_isolated_temp_roots_and_databases() {
    let root_a = temp_root("harness-isolation");
    let root_b = temp_root("harness-isolation");
    assert_ne!(root_a, root_b);

    let projects_a = make_claude_fixture(&root_a);
    let projects_b = make_claude_fixture(&root_b);
    let db_a = root_a.join("tkstat.db");
    let db_b = root_b.join("tkstat.db");
    assert_ne!(projects_a, projects_b);
    assert_ne!(db_a, db_b);
    assert_ne!(root_a.join("codex-home"), root_b.join("codex-home"));

    seed_pricing(&root_a, &db_a, &projects_a);
    seed_pricing(&root_b, &db_b, &projects_b);

    let run_a = run_tkstat(
        &root_a,
        [
            "--db",
            db_a.to_str().unwrap(),
            "--data-dir",
            projects_a.to_str().unwrap(),
            "--provider",
            "claude",
            "--json",
            "-d",
        ],
    );
    let run_b = run_tkstat(
        &root_b,
        [
            "--db",
            db_b.to_str().unwrap(),
            "--data-dir",
            projects_b.to_str().unwrap(),
            "--provider",
            "claude",
            "--json",
            "-d",
        ],
    );
    assert_success(&run_a);
    assert_success(&run_b);
    assert_eq!(parse_stdout_json(&run_a)[0]["request_count"], 3);
    assert_eq!(parse_stdout_json(&run_b)[0]["request_count"], 3);

    let _ = fs::remove_dir_all(root_a);
    let _ = fs::remove_dir_all(root_b);
}

#[test]
fn test_fixture_corpus_parses_claude_identity_categories_projects_and_subagents() {
    let root = temp_root("fixture-corpus-claude");
    let projects = make_claude_corpus_fixture(&root);
    let adapter = ClaudeCodeAdapter::new(&projects);
    let records = parse_adapter_records(&adapter);

    assert_eq!(records.len(), 4);
    assert!(records.iter().any(|record| {
        record.provider == "claude"
            && record.project == "demo"
            && record.model_id == "claude-opus-4-5-20251101"
            && record.timestamp.to_rfc3339() == "2026-01-31T21:20:19.858+00:00"
            && record.cache_creation_tokens == 100
            && record.cache_read_tokens == 200
    }));
    assert!(records.iter().any(|record| {
        record.project == "api"
            && record.session_id == "corpus-session-api"
            && record.model_id == "claude-sonnet-4-5-20250929"
    }));
    assert!(records.iter().any(|record| record.is_subagent));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_fixture_corpus_parses_codex_cached_and_reasoning_categories() {
    let root = temp_root("fixture-corpus-codex");
    let codex_home = make_codex_fixture(&root);
    let adapter = CodexAdapter::new(&codex_home);
    let records = parse_adapter_records(&adapter);

    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.provider, "codex");
    assert_eq!(record.project, "tkstat");
    assert_eq!(record.model_id, "gpt-5.5");
    assert_eq!(record.cached_input_tokens, 40);
    assert_eq!(record.reasoning_output_tokens, 7);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_fixture_corpus_is_covered_by_seeded_pricing() {
    let root = temp_root("fixture-corpus-pricing");
    let projects = make_claude_corpus_fixture(&root);
    let codex_home = make_codex_fixture(&root);
    let mut records = parse_adapter_records(&ClaudeCodeAdapter::new(&projects));
    records.extend(parse_adapter_records(&CodexAdapter::new(&codex_home)));

    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    for record in &records {
        db.calculate_record_cost(record).unwrap_or_else(|err| {
            panic!(
                "missing seeded pricing for {}/{}/{}: {err}",
                record.provider,
                record.model_id,
                record.timestamp.to_rfc3339()
            )
        });
    }

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_committed_provider_fixture_files_parse_expected_records() {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut fixture_files = Vec::new();
    collect_text_files(&fixture_root, &mut fixture_files);
    let jsonl_files = fixture_files
        .iter()
        .filter(|path| path.extension().and_then(OsStr::to_str) == Some("jsonl"))
        .count();
    assert_eq!(jsonl_files, 4);

    let root = temp_root("committed-fixture-smoke");
    let projects = make_claude_corpus_fixture(&root);
    let codex_home = make_codex_fixture(&root);
    let claude_records = parse_adapter_records(&ClaudeCodeAdapter::new(&projects));
    let codex_records = parse_adapter_records(&CodexAdapter::new(&codex_home));

    assert_eq!(claude_records.len(), 4);
    assert!(claude_records.iter().any(|record| {
        record.provider == "claude"
            && record.project == "demo"
            && record.session_id == "corpus-session-main"
            && record.model_id == "claude-opus-4-5-20251101"
            && record.timestamp.to_rfc3339() == "2026-01-31T21:20:19.858+00:00"
            && record.cache_creation_tokens == 100
            && record.cache_read_tokens == 200
    }));
    assert!(claude_records.iter().any(|record| {
        record.provider == "claude"
            && record.project == "demo"
            && record.session_id == "corpus-session-subagent"
            && record.is_subagent
    }));
    assert!(
        claude_records
            .iter()
            .any(|record| record.project == "api" && record.session_id == "corpus-session-api")
    );

    assert_eq!(codex_records.len(), 1);
    let codex = &codex_records[0];
    assert_eq!(codex.provider, "codex");
    assert_eq!(codex.project, "tkstat");
    assert_eq!(codex.session_id, "synthetic-codex-session");
    assert_eq!(codex.model_id, "gpt-5.5");
    assert_eq!(
        codex.timestamp.to_rfc3339(),
        "2026-05-24T00:40:04.988+00:00"
    );
    assert_eq!(codex.cached_input_tokens, 40);
    assert_eq!(codex.reasoning_output_tokens, 7);

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
    assert!(doctor_stdout.contains("claude: available"));
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
    assert!(force_stdout.contains("claude"));
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
    assert!(daily_stdout.contains("2026-05-24"));
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
fn test_e2e_smoke_script_runs_with_compiled_binary() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/e2e_smoke.sh");
    let output = Command::new("bash")
        .arg(script)
        .env("TKSTAT_BIN", env!("CARGO_BIN_EXE_tkstat"))
        .env("KEEP_TMP", "0")
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
    assert!(stderr.contains("--pricing-seed"));
    assert!(stderr.contains("--by-provider"));
    assert!(!stdout.contains("/.claude"));
    assert!(!stderr.contains("/.claude"));
    assert!(!stdout.contains("/.codex"));
    assert!(!stderr.contains("/.codex"));
}

#[test]
fn test_committed_fixtures_do_not_contain_personal_paths_or_transcripts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for relative in ["src", "tests", "scripts", "README.md", "CLAUDE.md"] {
        collect_text_files(&root.join(relative), &mut files);
    }

    let developer_name = ["je", "ff"].concat();
    let forbidden = [
        ["/Us", "ers/"].concat(),
        ["-Us", "ers-", &developer_name, "-"].concat(),
        developer_name,
        [".co", "dex/sessions"].concat(),
    ];
    let transcript_fields = [
        [r#"""#, "pro", "mpt", r#"""#].concat(),
        [r#"""#, "res", "ponse", r#"""#].concat(),
    ];

    for file in files {
        let content = fs::read_to_string(&file).unwrap();
        for fragment in &forbidden {
            assert!(
                !content.contains(fragment),
                "{} contains forbidden personal fragment {:?}",
                file.display(),
                fragment
            );
        }
        if file.starts_with(root.join("tests")) || file.starts_with(root.join("scripts")) {
            for field in &transcript_fields {
                assert!(
                    !content.contains(field),
                    "{} contains transcript-like fixture field {:?}",
                    file.display(),
                    field
                );
            }
        }
    }
}

fn collect_text_files(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_file() {
        files.push(path.to_path_buf());
        return;
    }

    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            collect_text_files(&path, files);
        } else if matches!(
            path.extension().and_then(OsStr::to_str),
            Some("rs" | "md" | "sh" | "toml" | "jsonl" | "json")
        ) {
            files.push(path);
        }
    }
}

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
    assert!(daily_stdout.contains("2026-05-24"));
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
    assert!(top_stdout.contains("2026-05-24"));

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
    assert!(by_provider_stdout.contains("claude"));
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
            .any(|row| { row["period"] == "2026-05-24" && row["provider"] == "all providers" })
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
    assert!(csv_daily_stdout.contains("2026-05-24,all providers,"));

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
fn test_pricing_failure_without_seed_then_seed_remediates_json_report() {
    let root = temp_root("pricing-failure-remediation-json");
    let projects = make_claude_corpus_fixture(&root);
    let db = root.join("tkstat.db");
    let report_args = [
        "--provider",
        "claude",
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
    assert_missing_pricing_remediation(&missing, "claude", "claude-opus-4-5-20251101", None);
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
        .find(|row| row["period"] == "2026-01-31" && row["provider"] == "claude")
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
        "claude",
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
             WHERE provider = 'claude'
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
        "claude",
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
    assert!(stdout.contains("2026-01-31,claude,"));

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
    assert!(stdout.contains("claude / daily"));
    assert!(stdout.contains("2026-01-31"));
    assert!(!stdout.contains("cost"));
    assert_no_pricing_coverage_error(&token_table);

    let token_chart = run_tkstat(
        &root,
        base.into_iter()
            .chain(["--chart", "--chart-metric", "tokens", "--no-color"]),
    );
    assert_success(&token_chart);
    assert!(String::from_utf8_lossy(&token_chart.stdout).contains("claude / chart"));
    assert_no_pricing_coverage_error(&token_chart);

    let input_heatmap = run_tkstat(
        &root,
        base.into_iter()
            .chain(["--heatmap", "--chart-metric", "input", "--no-color"]),
    );
    assert_success(&input_heatmap);
    assert!(String::from_utf8_lossy(&input_heatmap.stdout).contains("claude / heatmap"));
    assert_no_pricing_coverage_error(&input_heatmap);

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
        row["provider"] == "claude" && row["model_id"] == "claude-opus-4-5-20251101"
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
    assert_eq!(provider_request_count(&all_rows, "claude"), Some(4));
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
        row["provider"] == "claude" && row["model_id"] == "claude-opus-4-5-20251101"
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
    assert_eq!(provider_request_count(&all_again_rows, "claude"), Some(4));
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

fn audit_record(model_id: &str) -> TokenRecord {
    TokenRecord {
        provider: "codex".into(),
        request_id: "audit-request".into(),
        session_id: "audit-session".into(),
        uuid: "audit-uuid".into(),
        timestamp: "2026-04-07T10:00:00Z".parse().unwrap(),
        model: ModelFamily::Unknown,
        model_id: model_id.into(),
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        cached_input_tokens: 25,
        reasoning_output_tokens: 0,
        cost_usd: 0.0,
        project: "audit".into(),
        source_file: "/audit.jsonl".into(),
        is_subagent: false,
    }
}

fn audit_interval(from: &str, to: Option<&str>) -> PricingInterval {
    let mut interval = PricingInterval::usd(
        "codex",
        "gpt-audit",
        TokenCategory::Input,
        1.0,
        from.parse().unwrap(),
        "e2e",
    );
    interval.effective_to = to.map(|dt| dt.parse().unwrap());
    interval
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
    assert!(stdout.contains("claude / daily"));
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
    assert_eq!(json[0]["provider"], "claude");
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
        rows.iter()
            .any(|row| { row["provider"] == "claude" && row["model_id"] == "claude-opus-4-6" })
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
    assert_eq!(json[0]["period"], "claude");
    assert_eq!(json[0]["provider"], "claude");
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
            .any(|row| row.starts_with("2026-04-07,claude,90,"))
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
    assert!(stdout.contains("claude/claude-opus-4-6,claude,claude-opus-4-6,30,1"));

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
    assert!(stderr.contains("provider: claude"));

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
    assert_eq!(selected["top_provider"], "claude");
    assert_eq!(selected["top_project"], "demo");

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
    assert!(stderr.contains("claude source path is not configured"));
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
    assert!(stdout.contains("claude: available"));
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
    assert!(stdout.contains("claude: missing"));
    assert!(stdout.contains("claude source path is missing"));

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
