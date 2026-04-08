use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use walkdir::WalkDir;

/// Metadata about a JSONL session log file.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: PathBuf,
    pub project_name: String,
    pub is_subagent: bool,
    pub size_bytes: u64,
    pub mtime_secs: i64,
}

/// Walk the Claude data directory and find all JSONL session files.
pub fn discover_jsonl_files(data_dir: &Path) -> Result<Vec<SourceFile>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(data_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl")
            && let Some(file) = parse_file_metadata(path)
        {
            files.push(file);
        }
    }

    Ok(files)
}

/// Extract project name, session ID, and subagent status from a JSONL path.
///
/// Expected patterns:
///   ~/.claude/projects/{project-dir}/{uuid}.jsonl
///   ~/.claude/projects/{project-dir}/{uuid}/subagents/{agent-id}.jsonl
fn parse_file_metadata(path: &Path) -> Option<SourceFile> {
    let meta = std::fs::metadata(path).ok()?;
    let size_bytes = meta.len();
    let mtime_secs = meta
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;

    let is_subagent = path.components().any(|c| c.as_os_str() == "subagents");
    let path_str = path.to_string_lossy();
    let project_name = extract_project_name(&path_str);

    Some(SourceFile {
        path: path.to_path_buf(),
        project_name,
        is_subagent,
        size_bytes,
        mtime_secs,
    })
}

/// Derive a human-readable project name from the Claude projects directory name.
/// "-Users-alice-src-myapp" → "myapp"
/// "-Users-alice-src-my-project" → "my-project"
fn extract_project_name(path_str: &str) -> String {
    if let Some(idx) = path_str.find("/projects/") {
        let after = &path_str[idx + "/projects/".len()..];
        let dir_name = after.split('/').next().unwrap_or("");
        let parts: Vec<&str> = dir_name.split('-').collect();

        if let Some(pos) = parts.iter().rposition(|&p| p == "src") {
            let slug_parts = &parts[pos + 1..];
            if !slug_parts.is_empty() {
                return slug_parts.join("-");
            }
        }

        if let Some(last) = parts.iter().rev().find(|p| !p.is_empty()) {
            return last.to_string();
        }

        return dir_name.to_string();
    }

    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_project_name_src_path() {
        let path = "/home/alice/.claude/projects/-Users-alice-src-myapp/abc.jsonl";
        assert_eq!(extract_project_name(path), "myapp");
    }

    #[test]
    fn test_extract_project_name_nested_src() {
        let path = "/home/alice/.claude/projects/-Users-alice-src-my-project/abc.jsonl";
        assert_eq!(extract_project_name(path), "my-project");
    }

    #[test]
    fn test_extract_project_name_no_src() {
        let path = "/home/alice/.claude/projects/-Users-alice-dotfiles/abc.jsonl";
        assert_eq!(extract_project_name(path), "dotfiles");
    }

    #[test]
    fn test_extract_project_name_no_projects() {
        let path = "/some/other/path/file.jsonl";
        assert_eq!(extract_project_name(path), "unknown");
    }

    #[test]
    fn test_extract_project_name_deep_path() {
        let path = "/home/alice/.claude/projects/-Users-alice-src-data-apps/abc.jsonl";
        assert_eq!(extract_project_name(path), "data-apps");
    }
}
