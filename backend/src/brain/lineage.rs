//! Git history lineage layer.
//!
//! Augment's "Context Lineage" uses an LLM pass over commit history.
//! Tersh's approach is lighter: parse `git log --name-only`, count how
//! many commits touched each file in the last 30 / 90 days, and surface
//! the hottest files. The retrieval scorer biases bug-fix / refactor
//! prompts toward recently-active files.
//!
//! Skips silently when:
//! - `.git` is not present at the project root
//! - the `git` binary is not on PATH
//! - the log command times out or returns nothing

use crate::errors::AppResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

/// How much git log we read. Big enough to cover ~6 months of moderate
/// activity, capped so a giant monorepo doesn't blow the parser.
const MAX_LOG_BYTES: usize = 256 * 1024;
const LOG_LIMIT_COMMITS: usize = 200;
const LOG_TIMEOUT: Duration = Duration::from_secs(5);
/// Files touched in the last 30 days are "very hot."
const HOT_WINDOW_SECS_30: i64 = 60 * 60 * 24 * 30;
/// 90-day window for "warm."
const HOT_WINDOW_SECS_90: i64 = 60 * 60 * 24 * 90;
/// Cap on how many recently-active files we surface.
const MAX_HOT_FILES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Lineage {
    /// Files ranked highest by recent-commit activity, capped.
    pub recently_active: Vec<String>,
    /// Per-file activity bucket — keyed by project-relative path.
    pub file_activity: HashMap<String, FileActivity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileActivity {
    pub commits_touched: usize,
    pub last_touched_at: i64,
    /// Up to 3 most recent commit subjects. Useful as a tiny human
    /// signal in the prompt context — e.g. "bug fix landed last week".
    pub recent_subjects: Vec<String>,
}

impl Lineage {
    /// Hot-file multiplier for retrieval scoring. 1.2 for very-recent
    /// touches, 1.1 for warm, 1.0 otherwise. Keeps the boost gentle so
    /// keyword/structural signals still dominate.
    pub fn hotness_multiplier(&self, path: &str, now: i64) -> f32 {
        let Some(act) = self.file_activity.get(path) else {
            return 1.0;
        };
        let age = now.saturating_sub(act.last_touched_at);
        if age <= HOT_WINDOW_SECS_30 {
            1.2
        } else if age <= HOT_WINDOW_SECS_90 {
            1.1
        } else {
            1.0
        }
    }
}

/// Collect git lineage from a local project root. Returns Ok(None) if the
/// project isn't a git checkout or git isn't available.
pub async fn collect_git_lineage(root: &Path) -> AppResult<Option<Lineage>> {
    if !root.join(".git").exists() {
        return Ok(None);
    }
    let output = tokio::time::timeout(
        LOG_TIMEOUT,
        Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("log")
            .arg(format!("-n{LOG_LIMIT_COMMITS}"))
            .arg("--name-only")
            .arg("--pretty=format:%H|%ct|%s")
            .arg("--no-merges")
            .output(),
    )
    .await;
    let output = match output {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::debug!("git log failed: {e}");
            return Ok(None);
        }
        Err(_) => {
            tracing::debug!("git log timed out");
            return Ok(None);
        }
    };
    if !output.status.success() {
        return Ok(None);
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.len() > MAX_LOG_BYTES {
        text.truncate(MAX_LOG_BYTES);
    }
    Ok(Some(parse_git_log(&text)))
}

/// Pure parser, isolated for testability. Input is git log output in the
/// format produced by the `--pretty` template above:
///
/// ```text
/// <sha>|<unix_ts>|<subject>
/// path/one.rs
/// path/two.ts
///
/// <sha>|<unix_ts>|<subject>
/// path/three.py
/// ```
pub fn parse_git_log(text: &str) -> Lineage {
    let mut activity: HashMap<String, FileActivity> = HashMap::new();
    let mut current_ts: i64 = 0;
    let mut current_subject = String::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some((_, rest)) = line.split_once('|') {
            // Header line: <sha>|<ts>|<subject>
            let (ts_part, subject) = rest.split_once('|').unwrap_or((rest, ""));
            current_ts = ts_part.parse::<i64>().unwrap_or(0);
            current_subject = subject.to_string();
            continue;
        }
        // Otherwise a file path.
        if line.starts_with('/') || line.contains("..") {
            continue;
        }
        let entry = activity.entry(line.to_string()).or_default();
        entry.commits_touched = entry.commits_touched.saturating_add(1);
        if current_ts > entry.last_touched_at {
            entry.last_touched_at = current_ts;
        }
        if !current_subject.is_empty() && entry.recent_subjects.len() < 3 {
            // Insert at the front so most-recent stays first.
            entry.recent_subjects.insert(0, current_subject.clone());
            if entry.recent_subjects.len() > 3 {
                entry.recent_subjects.truncate(3);
            }
        }
    }
    let now = crate::brain::unix_secs();
    let mut ranked: Vec<(String, FileActivity)> = activity.clone().into_iter().collect();
    ranked.sort_by(|a, b| {
        let score_a = hot_score(&a.1, now);
        let score_b = hot_score(&b.1, now);
        score_b
            .cmp(&score_a)
            .then_with(|| b.1.last_touched_at.cmp(&a.1.last_touched_at))
            .then_with(|| a.0.cmp(&b.0))
    });
    let recently_active = ranked
        .into_iter()
        .take(MAX_HOT_FILES)
        .map(|(path, _)| path)
        .collect();
    Lineage {
        recently_active,
        file_activity: activity,
    }
}

fn hot_score(act: &FileActivity, now: i64) -> i64 {
    let age = now.saturating_sub(act.last_touched_at);
    let bucket: i64 = if age <= HOT_WINDOW_SECS_30 {
        3
    } else if age <= HOT_WINDOW_SECS_90 {
        2
    } else {
        1
    };
    (act.commits_touched as i64).saturating_mul(bucket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_groups_files_under_commits() {
        let log = "abc123|1700000000|fix sftp\nfile/one.rs\nfile/two.ts\n\ndef456|1700000100|update theme\nfile/three.tsx\n";
        let lineage = parse_git_log(log);
        assert_eq!(lineage.file_activity.len(), 3);
        assert_eq!(
            lineage
                .file_activity
                .get("file/one.rs")
                .unwrap()
                .commits_touched,
            1
        );
        assert!(lineage
            .file_activity
            .get("file/three.tsx")
            .unwrap()
            .recent_subjects
            .contains(&"update theme".to_string()));
    }

    #[test]
    fn hot_files_rank_above_cold_ones() {
        // Build a log where two files have many recent commits and one is
        // ancient. The hot files should appear first in recently_active.
        let now = crate::brain::unix_secs();
        let recent = now - 86_400; // 1 day ago
        let ancient = now - HOT_WINDOW_SECS_90 - 86_400;
        let log = format!(
            "abc|{recent}|hot fix\nhot/one.rs\n\ndef|{recent}|hot work\nhot/one.rs\n\nghi|{ancient}|old\nold/file.rs\n"
        );
        let lineage = parse_git_log(&log);
        assert_eq!(
            lineage.recently_active.first().map(String::as_str),
            Some("hot/one.rs")
        );
        assert!(lineage
            .recently_active
            .iter()
            .position(|p| p == "old/file.rs")
            .map(|idx| idx > 0)
            .unwrap_or(true));
    }

    #[test]
    fn hotness_multiplier_bands_correctly() {
        let mut lineage = Lineage::default();
        let now = crate::brain::unix_secs();
        lineage.file_activity.insert(
            "warm/file.rs".to_string(),
            FileActivity {
                commits_touched: 1,
                last_touched_at: now - HOT_WINDOW_SECS_30 - 1,
                recent_subjects: vec![],
            },
        );
        lineage.file_activity.insert(
            "hot/file.rs".to_string(),
            FileActivity {
                commits_touched: 1,
                last_touched_at: now - 60,
                recent_subjects: vec![],
            },
        );
        assert_eq!(lineage.hotness_multiplier("hot/file.rs", now), 1.2);
        assert_eq!(lineage.hotness_multiplier("warm/file.rs", now), 1.1);
        assert_eq!(lineage.hotness_multiplier("missing/file.rs", now), 1.0);
    }

    #[test]
    fn skips_paths_with_traversal_or_absolute() {
        let log = "abc|1700000000|sketchy\n/etc/passwd\n../escape\nok/file.rs\n";
        let lineage = parse_git_log(log);
        assert!(!lineage.file_activity.contains_key("/etc/passwd"));
        assert!(!lineage.file_activity.contains_key("../escape"));
        assert!(lineage.file_activity.contains_key("ok/file.rs"));
    }
}
