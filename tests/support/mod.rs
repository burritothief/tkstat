// Shared helpers for fixture-driven black-box CLI tests.
#![allow(dead_code, unused_imports)]

use std::ffi::OsStr;
pub use std::fs;
use std::ops::Deref;
pub use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

pub use rusqlite::Connection;
pub use serde_json::Value;
pub use tkstat::db::Database;
pub use tkstat::domain::pricing::{PricingInterval, TokenCategory};
pub use tkstat::domain::usage::{ModelFamily, TokenRecord};
pub use tkstat::ingest::{ClaudeCodeAdapter, CodexAdapter, ProviderAdapter};

const CLAUDE_CORPUS_DEMO_MAIN: &str = include_str!("../fixtures/claude/demo/main.jsonl");
const CLAUDE_CORPUS_DEMO_SUBAGENT: &str =
    include_str!("../fixtures/claude/demo/subagents/agent.jsonl");
const CLAUDE_CORPUS_API: &str = include_str!("../fixtures/claude/api/api.jsonl");
const CODEX_CORPUS_SESSION: &str = include_str!("../fixtures/codex/synthetic-codex-session.jsonl");

pub struct CommandOutput {
    args: Vec<String>,
    output: Output,
}

impl Deref for CommandOutput {
    type Target = Output;

    fn deref(&self) -> &Self::Target {
        &self.output
    }
}

pub fn temp_root(test_name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "tkstat-cli-e2e-{test_name}-{}-{nanos}",
        std::process::id()
    ))
}

pub fn assistant_line(
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

pub fn make_claude_fixture(root: &Path) -> PathBuf {
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

pub fn make_claude_corpus_fixture(root: &Path) -> PathBuf {
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

pub fn make_filter_corpus_fixture(root: &Path) -> PathBuf {
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

pub fn make_codex_fixture(root: &Path) -> PathBuf {
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

pub fn parse_adapter_records(adapter: &dyn ProviderAdapter) -> Vec<TokenRecord> {
    let mut records = Vec::new();
    for file in adapter.discover().unwrap() {
        let parsed = adapter.parse_file(&file.path, 0, &file).unwrap();
        records.extend(parsed.records);
    }
    records
}

pub fn run_tkstat<I, S>(root: &Path, args: I) -> CommandOutput
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
        .env("TZ", "America/Los_Angeles")
        .env("TKSTAT_PRICING_REFRESH_OFFLINE", "1")
        .output()
        .unwrap();
    CommandOutput { args, output }
}

pub fn assert_success(output: &CommandOutput) {
    assert!(
        output.status.success(),
        "args: tkstat {}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn assert_failure(output: &CommandOutput) {
    assert!(
        !output.status.success(),
        "args: tkstat {}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn seed_pricing(root: &Path, db: &Path, projects: &Path) {
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

pub fn parse_stdout_json(output: &CommandOutput) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "invalid json: {err}\nargs: tkstat {}\nstdout:\n{}\nstderr:\n{}",
            output.args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

pub fn assert_no_pricing_coverage_error(output: &CommandOutput) {
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

pub fn assert_missing_pricing_remediation(
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
    assert_eq!(
        stderr_field_value(&stderr, "provider"),
        Some(provider),
        "pricing remediation provider did not match exactly\nstderr:\n{stderr}"
    );
    assert_eq!(
        stderr_field_value(&stderr, "model"),
        Some(model_id),
        "pricing remediation model did not match exactly\nstderr:\n{stderr}"
    );
    match category {
        Some(category) => assert_eq!(
            stderr_field_value(&stderr, "category"),
            Some(category),
            "pricing remediation category did not match exactly\nstderr:\n{stderr}"
        ),
        None => assert!(
            stderr_field_value(&stderr, "category").is_some(),
            "pricing remediation did not include category\nstderr:\n{stderr}"
        ),
    }
    assert!(stderr.contains("usage range"));
    assert!(stderr.contains("tkstat --pricing-refresh"));
    assert!(stderr.contains("tkstat --pricing-seed"));
}

pub fn stderr_field_value<'a>(stderr: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    stderr
        .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == ';')
        .find_map(|segment| segment.strip_prefix(&prefix))
        .filter(|value| !value.is_empty())
}

pub fn setup_ingested_corpus(root: &Path, test_name: &str) -> (PathBuf, PathBuf) {
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

pub fn audit_record(model_id: &str) -> TokenRecord {
    TokenRecord {
        provider: tkstat::domain::provider::ProviderId::Codex,
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
        cache_creation_5m_tokens: 0,
        cache_creation_1h_tokens: 0,
        service_tier: None,
        speed: None,
        region: None,
        processing_mode: None,
        cost_usd: 0.0,
        project: "audit".into(),
        source_file: "/audit.jsonl".into(),
        is_subagent: false,
    }
}

pub fn audit_interval(from: &str, to: Option<&str>) -> PricingInterval {
    let mut interval = PricingInterval::usd(
        tkstat::domain::provider::ProviderId::Codex,
        "gpt-audit",
        TokenCategory::Input,
        1.0,
        from.parse().unwrap(),
        "e2e",
    );
    interval.effective_to = to.map(|dt| dt.parse().unwrap());
    interval
}
