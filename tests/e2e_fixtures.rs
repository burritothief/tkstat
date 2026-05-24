//! Fixture-driven black-box CLI tests.

mod support;
use support::*;

use std::ffi::OsStr;

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
        record.provider == "claude-code"
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
        record.provider == "claude-code"
            && record.project == "demo"
            && record.session_id == "corpus-session-main"
            && record.model_id == "claude-opus-4-5-20251101"
            && record.timestamp.to_rfc3339() == "2026-01-31T21:20:19.858+00:00"
            && record.cache_creation_tokens == 100
            && record.cache_read_tokens == 200
    }));
    assert!(claude_records.iter().any(|record| {
        record.provider == "claude-code"
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
