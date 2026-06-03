use crate::brain::{secrets, BrainScope};
use crate::errors::{AppError, AppResult};
use crate::ssh::SshSession;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Cap bytes returned by any one tool call. Keeps a runaway model from
/// eating the whole context window in one read_file invocation.
const MAX_TOOL_RESULT_BYTES: usize = 32 * 1024;
const MAX_GREP_MATCHES: usize = 200;
const MAX_FIND_FILES_RESULTS: usize = 200;
const MAX_LIST_ENTRIES: usize = 200;
const REMOTE_EXEC_BYTES: usize = 64 * 1024;

/// OpenAI-compatible tool definitions. Same schema works against any
/// provider that follows the tool-calling spec (OpenAI, Anthropic via
/// OpenRouter, DeepSeek v4, Mistral, etc.).
pub fn tool_schemas() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a text file in the project. Use this to inspect implementations, configs, or docs. Returns up to 32 KB. Optionally pass start_line / end_line (1-indexed, inclusive) to fetch only a window.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Project-relative path, e.g. 'src/lib.rs' or './README.md'"},
                        "start_line": {"type": "integer", "minimum": 1},
                        "end_line": {"type": "integer", "minimum": 1}
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_directory",
                "description": "List entries in a project directory. Returns files and folders with size hints. Use this to discover what exists before reading.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Project-relative path, default '.'"}
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "find_files",
                "description": "Find project files or folders by name/path substring. Use this when the user mentions a component, file, feature, or folder but you do not know the exact path yet.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "File, folder, component, or path fragment to find, e.g. 'TerminalView', 'drawer', 'sftp'"},
                        "path": {"type": "string", "description": "Project-relative subtree to search, default '.'"},
                        "max_results": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search for a regex pattern across project files. Returns up to 200 matches as 'path:line: match'. Use this to locate where a symbol or string is defined or used.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Regex or fixed string"},
                        "path": {"type": "string", "description": "Project-relative subtree to search, default '.'"},
                        "case_sensitive": {"type": "boolean", "default": false}
                    },
                    "required": ["pattern"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "tree",
                "description": "Show the project directory layout up to a given depth. Use this once at the start to orient yourself.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Project-relative root, default '.'"},
                        "max_depth": {"type": "integer", "minimum": 1, "maximum": 5, "default": 2}
                    }
                }
            }
        }),
    ]
}

pub async fn execute_local(name: &str, args: &Value, root: &Path) -> AppResult<String> {
    match name {
        "read_file" => read_file_local(args, root).await,
        "list_directory" => list_directory_local(args, root).await,
        "find_files" => find_files_local(args, root).await,
        "grep" => grep_local(args, root).await,
        "tree" => tree_local(args, root).await,
        _ => Err(AppError::Invalid(format!("unknown tool: {name}"))),
    }
}

pub async fn execute_remote(
    name: &str,
    args: &Value,
    session: &Arc<SshSession>,
    root: &str,
) -> AppResult<String> {
    match name {
        "read_file" => read_file_remote(args, session, root).await,
        "list_directory" => list_directory_remote(args, session, root).await,
        "find_files" => find_files_remote(args, session, root).await,
        "grep" => grep_remote(args, session, root).await,
        "tree" => tree_remote(args, session, root).await,
        _ => Err(AppError::Invalid(format!("unknown tool: {name}"))),
    }
}

// ── LOCAL EXECUTORS ────────────────────────────────────────────────────────

async fn read_file_local(args: &Value, root: &Path) -> AppResult<String> {
    let rel = arg_str(args, "path")?;
    let start = arg_uint_opt(args, "start_line");
    let end = arg_uint_opt(args, "end_line");
    let abs = resolve_in_root(root, &rel)?;

    let abs_str = abs.to_string_lossy().into_owned();
    if let Some(reason) = secrets::path_skip_reason(&abs_str) {
        return Ok(format!("[redacted: {reason}]"));
    }

    let meta = tokio::fs::metadata(&abs)
        .await
        .map_err(|e| AppError::Invalid(format!("stat {rel}: {e}")))?;
    if !meta.is_file() {
        return Err(AppError::Invalid(format!("{rel} is not a file")));
    }
    if meta.len() > 4 * 1024 * 1024 {
        return Err(AppError::Invalid(format!(
            "{rel} is too large ({} bytes)",
            meta.len()
        )));
    }

    let bytes = tokio::fs::read(&abs)
        .await
        .map_err(|e| AppError::Invalid(format!("read {rel}: {e}")))?;
    if let Some(reason) = secrets::content_secret_kind(&bytes) {
        return Ok(format!("[redacted: content matches {reason}]"));
    }
    let text = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return Ok("[binary file]".into()),
    };
    let sliced = slice_lines(text, start, end);
    Ok(truncate_for_tool(&sliced))
}

async fn list_directory_local(args: &Value, root: &Path) -> AppResult<String> {
    let rel = arg_str(args, "path").unwrap_or_else(|_| ".".into());
    let abs = resolve_in_root(root, &rel)?;
    let meta = tokio::fs::metadata(&abs)
        .await
        .map_err(|e| AppError::Invalid(format!("stat {rel}: {e}")))?;
    if !meta.is_dir() {
        return Err(AppError::Invalid(format!("{rel} is not a directory")));
    }
    let mut rd = tokio::fs::read_dir(&abs)
        .await
        .map_err(|e| AppError::Invalid(format!("read_dir {rel}: {e}")))?;
    let mut entries: Vec<(String, bool, u64)> = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !matches!(name.as_str(), ".github" | ".gitignore") {
            continue;
        }
        let m = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        entries.push((name, m.is_dir(), m.len()));
        if entries.len() >= MAX_LIST_ENTRIES {
            break;
        }
    }
    entries.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.0.cmp(&b.0),
    });
    let mut out = String::new();
    for (name, is_dir, size) in entries {
        if is_dir {
            out.push_str(&format!("{name}/\n"));
        } else {
            out.push_str(&format!("{name}  ({size}B)\n"));
        }
    }
    if out.is_empty() {
        out.push_str("(empty)\n");
    }
    Ok(out)
}

async fn grep_local(args: &Value, root: &Path) -> AppResult<String> {
    let pattern = arg_str(args, "pattern")?;
    let rel = arg_str(args, "path").unwrap_or_else(|_| ".".into());
    let case_sensitive = arg_bool(args, "case_sensitive").unwrap_or(false);
    let abs = resolve_in_root(root, &rel)?;
    let pattern_l = pattern.to_lowercase();
    let mut matches: Vec<String> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![abs];
    'walk: while let Some(dir) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && !matches!(name.as_str(), ".github" | ".gitignore") {
                continue;
            }
            let m = match entry.metadata().await {
                Ok(m) => m,
                Err(_) => continue,
            };
            if m.is_dir() {
                let p_str = p.to_string_lossy().into_owned();
                if secrets::path_skip_reason(&format!("/{p_str}/")).is_some() {
                    continue;
                }
                stack.push(p);
                continue;
            }
            if !m.is_file() || m.len() > 1024 * 1024 {
                continue;
            }
            let p_str = p.to_string_lossy().into_owned();
            if secrets::path_skip_reason(&p_str).is_some() {
                continue;
            }
            let bytes = match tokio::fs::read(&p).await {
                Ok(b) => b,
                Err(_) => continue,
            };
            if secrets::content_secret_kind(&bytes).is_some() {
                continue;
            }
            let text = match std::str::from_utf8(&bytes) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let rel_path = p
                .strip_prefix(root)
                .ok()
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_else(|| p_str.clone());
            for (i, line) in text.lines().enumerate() {
                let hit = if case_sensitive {
                    line.contains(&pattern)
                } else {
                    line.to_lowercase().contains(&pattern_l)
                };
                if hit {
                    matches.push(format!("{}:{}: {}", rel_path, i + 1, line.trim()));
                    if matches.len() >= MAX_GREP_MATCHES {
                        break 'walk;
                    }
                }
            }
        }
    }
    if matches.is_empty() {
        Ok(format!("no matches for {pattern}"))
    } else {
        Ok(matches.join("\n"))
    }
}

async fn find_files_local(args: &Value, root: &Path) -> AppResult<String> {
    let query = arg_str(args, "query")?;
    let rel = arg_str(args, "path").unwrap_or_else(|_| ".".into());
    let max_results = arg_uint_opt(args, "max_results")
        .unwrap_or(50)
        .clamp(1, MAX_FIND_FILES_RESULTS as u32) as usize;
    let needle = normalize_search_text(&query);
    if needle.is_empty() {
        return Err(AppError::Invalid("find_files query is empty".into()));
    }

    let abs = resolve_in_root(root, &rel)?;
    let mut hits: Vec<(u32, String, bool)> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![abs];
    while let Some(dir) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if should_skip_tool_entry(&name) {
                continue;
            }
            let meta = match entry.metadata().await {
                Ok(m) => m,
                Err(_) => continue,
            };
            let p_str = p.to_string_lossy().into_owned();
            if secrets::path_skip_reason(&p_str).is_some() {
                continue;
            }
            let rel_path = p
                .strip_prefix(root)
                .ok()
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_else(|| p_str.clone());
            let score = score_file_match(&rel_path, &name, &needle);
            if score > 0 {
                hits.push((score, rel_path, meta.is_dir()));
            }
            if meta.is_dir() {
                stack.push(p);
            }
        }
    }

    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    hits.truncate(max_results);
    if hits.is_empty() {
        Ok(format!("no files matched {query}"))
    } else {
        Ok(hits
            .into_iter()
            .map(|(_, path, is_dir)| if is_dir { format!("{path}/") } else { path })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

async fn tree_local(args: &Value, root: &Path) -> AppResult<String> {
    let rel = arg_str(args, "path").unwrap_or_else(|_| ".".into());
    let depth = arg_uint_opt(args, "max_depth").unwrap_or(2).clamp(1, 5);
    let abs = resolve_in_root(root, &rel)?;
    let mut out = String::new();
    out.push_str(&format!(
        "{}/\n",
        abs.file_name().and_then(|s| s.to_str()).unwrap_or(".")
    ));
    walk_tree_local(&abs, &mut out, 0, depth as usize).await?;
    Ok(truncate_for_tool(&out))
}

fn walk_tree_local<'a>(
    dir: &'a Path,
    out: &'a mut String,
    indent: usize,
    max_depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<()>> + Send + 'a>> {
    Box::pin(async move {
        if indent >= max_depth {
            return Ok(());
        }
        let mut rd = match tokio::fs::read_dir(dir).await {
            Ok(rd) => rd,
            Err(_) => return Ok(()),
        };
        let mut entries: Vec<(String, bool)> = Vec::new();
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && !matches!(name.as_str(), ".github" | ".gitignore") {
                continue;
            }
            if should_skip_tool_entry(&name) {
                continue;
            }
            let m = match e.metadata().await {
                Ok(m) => m,
                Err(_) => continue,
            };
            entries.push((name, m.is_dir()));
        }
        entries.sort_by(|a, b| match (a.1, b.1) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.0.cmp(&b.0),
        });
        for (name, is_dir) in entries {
            for _ in 0..indent {
                out.push_str("  ");
            }
            if is_dir {
                out.push_str(&format!("{name}/\n"));
                walk_tree_local(&dir.join(&name), out, indent + 1, max_depth).await?;
            } else {
                out.push_str(&format!("{name}\n"));
            }
            if out.len() > MAX_TOOL_RESULT_BYTES {
                return Ok(());
            }
        }
        Ok(())
    })
}

// ── REMOTE EXECUTORS (via SSH) ─────────────────────────────────────────────

async fn read_file_remote(
    args: &Value,
    session: &Arc<SshSession>,
    root: &str,
) -> AppResult<String> {
    let rel = arg_str(args, "path")?;
    let abs = join_remote(root, &rel)?;
    if let Some(reason) = secrets::path_skip_reason(&abs) {
        return Ok(format!("[redacted: {reason}]"));
    }
    let start = arg_uint_opt(args, "start_line").unwrap_or(0);
    let end = arg_uint_opt(args, "end_line").unwrap_or(0);
    let cmd = if start > 0 && end >= start {
        format!(
            "sed -n '{start},{end}p' -- {abs_q} 2>&1 | head -c {cap}",
            start = start,
            end = end,
            abs_q = shell_quote(&abs),
            cap = MAX_TOOL_RESULT_BYTES
        )
    } else {
        format!(
            "head -c {cap} -- {abs_q} 2>&1",
            cap = MAX_TOOL_RESULT_BYTES,
            abs_q = shell_quote(&abs)
        )
    };
    let out = tokio::time::timeout(
        Duration::from_secs(15),
        session.exec_oneshot(&cmd, REMOTE_EXEC_BYTES),
    )
    .await
    .map_err(|_| AppError::Invalid("remote read_file timed out".into()))??;
    let text = String::from_utf8_lossy(&out).into_owned();
    if let Some(reason) = secrets::content_secret_kind(text.as_bytes()) {
        return Ok(format!("[redacted: content matches {reason}]"));
    }
    Ok(text)
}

async fn list_directory_remote(
    args: &Value,
    session: &Arc<SshSession>,
    root: &str,
) -> AppResult<String> {
    let rel = arg_str(args, "path").unwrap_or_else(|_| ".".into());
    let abs = join_remote(root, &rel)?;
    let cmd = format!(
        "ls -lA -- {} 2>&1 | head -n {MAX_LIST_ENTRIES}",
        shell_quote(&abs)
    );
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        session.exec_oneshot(&cmd, REMOTE_EXEC_BYTES),
    )
    .await
    .map_err(|_| AppError::Invalid("remote list_directory timed out".into()))??;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

async fn grep_remote(args: &Value, session: &Arc<SshSession>, root: &str) -> AppResult<String> {
    let pattern = arg_str(args, "pattern")?;
    let rel = arg_str(args, "path").unwrap_or_else(|_| ".".into());
    let case_sensitive = arg_bool(args, "case_sensitive").unwrap_or(false);
    let abs = join_remote(root, &rel)?;
    let flags = if case_sensitive { "-rn" } else { "-rni" };
    let cmd = format!(
        "grep {flags} --binary-files=without-match \
            --exclude-dir=.git --exclude-dir=node_modules --exclude-dir=target \
            --exclude-dir=dist --exclude-dir=build --exclude-dir=.next --exclude-dir=.turbo \
            -e {pat_q} -- {abs_q} 2>/dev/null | head -n {max}",
        pat_q = shell_quote(&pattern),
        abs_q = shell_quote(&abs),
        max = MAX_GREP_MATCHES
    );
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        session.exec_oneshot(&cmd, REMOTE_EXEC_BYTES),
    )
    .await
    .map_err(|_| AppError::Invalid("remote grep timed out".into()))??;
    let s = String::from_utf8_lossy(&out).into_owned();
    if s.trim().is_empty() {
        Ok(format!("no matches for {pattern}"))
    } else {
        Ok(s)
    }
}

async fn find_files_remote(
    args: &Value,
    session: &Arc<SshSession>,
    root: &str,
) -> AppResult<String> {
    let query = arg_str(args, "query")?;
    let rel = arg_str(args, "path").unwrap_or_else(|_| ".".into());
    let max_results = arg_uint_opt(args, "max_results")
        .unwrap_or(50)
        .clamp(1, MAX_FIND_FILES_RESULTS as u32);
    let abs = join_remote(root, &rel)?;
    let pattern = format!("*{}*", query.trim());
    let cmd = format!(
        "find {abs_q} \
            \\( -name node_modules -o -name target -o -name dist -o -name build -o -name .next -o -name .turbo -o -name .git \\) -prune -o \
            -iname {pattern_q} -print 2>/dev/null | head -n {max}",
        abs_q = shell_quote(&abs),
        pattern_q = shell_quote(&pattern),
        max = max_results
    );
    let out = tokio::time::timeout(
        Duration::from_secs(15),
        session.exec_oneshot(&cmd, REMOTE_EXEC_BYTES),
    )
    .await
    .map_err(|_| AppError::Invalid("remote find_files timed out".into()))??;
    let s = String::from_utf8_lossy(&out).into_owned();
    if s.trim().is_empty() {
        Ok(format!("no files matched {query}"))
    } else {
        Ok(s)
    }
}

async fn tree_remote(args: &Value, session: &Arc<SshSession>, root: &str) -> AppResult<String> {
    let rel = arg_str(args, "path").unwrap_or_else(|_| ".".into());
    let depth = arg_uint_opt(args, "max_depth").unwrap_or(2).clamp(1, 5);
    let abs = join_remote(root, &rel)?;
    // `find` is portable across all *nix; gives a flat listing the model can parse.
    let cmd = format!(
        "find {abs_q} -mindepth 1 -maxdepth {depth} \
            \\( -name node_modules -o -name target -o -name dist -o -name build -o -name .next -o -name .turbo -o -name .git \\) -prune -o \
            -print 2>/dev/null | head -c {cap}",
        abs_q = shell_quote(&abs),
        depth = depth,
        cap = MAX_TOOL_RESULT_BYTES
    );
    let out = tokio::time::timeout(
        Duration::from_secs(15),
        session.exec_oneshot(&cmd, REMOTE_EXEC_BYTES),
    )
    .await
    .map_err(|_| AppError::Invalid("remote tree timed out".into()))??;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

// ── HELPERS ────────────────────────────────────────────────────────────────

fn arg_str(args: &Value, key: &str) -> AppResult<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::Invalid(format!("tool arg '{key}' missing")))
}

fn arg_uint_opt(args: &Value, key: &str) -> Option<u32> {
    args.get(key)
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
}

fn arg_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

fn should_skip_tool_entry(name: &str) -> bool {
    if name.starts_with('.') && !matches!(name, ".github" | ".gitignore") {
        return true;
    }
    matches!(
        name,
        "node_modules" | "target" | "dist" | "build" | ".next" | ".turbo"
    )
}

fn normalize_search_text(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|ch| !matches!(ch, '-' | '_' | ' ' | '.'))
        .collect()
}

fn score_file_match(rel_path: &str, name: &str, needle: &str) -> u32 {
    let path_l = rel_path.to_lowercase();
    let name_l = name.to_lowercase();
    let compact_path = normalize_search_text(rel_path);
    let compact_name = normalize_search_text(name);
    if name_l == needle || compact_name == needle {
        100
    } else if name_l.contains(needle) || compact_name.contains(needle) {
        80
    } else if path_l.contains(needle) || compact_path.contains(needle) {
        50
    } else {
        0
    }
}

/// Canonicalize a project-relative path; refuse any path that escapes root
/// via `..` components. Also refuses absolute paths from the agent.
fn resolve_in_root(root: &Path, rel: &str) -> AppResult<PathBuf> {
    let rel = rel.trim_start_matches("./").trim_start_matches('/');
    if rel == ".." || rel.contains("/../") || rel.ends_with("/..") {
        return Err(AppError::Invalid("path escapes project root".into()));
    }
    let p = Path::new(rel);
    for c in p.components() {
        if matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(AppError::Invalid("path escapes project root".into()));
        }
    }
    Ok(root.join(p))
}

fn join_remote(root: &str, rel: &str) -> AppResult<String> {
    let rel = rel.trim_start_matches("./");
    if rel.starts_with('/') {
        return Err(AppError::Invalid(
            "tool path must be project-relative, not absolute".into(),
        ));
    }
    if rel == ".." || rel.contains("/../") || rel.ends_with("/..") {
        return Err(AppError::Invalid("path escapes project root".into()));
    }
    if rel.contains('\0') {
        return Err(AppError::Invalid("path contains NUL".into()));
    }
    let base = root.trim_end_matches('/');
    if rel.is_empty() || rel == "." {
        Ok(base.to_string())
    } else {
        Ok(format!("{base}/{rel}"))
    }
}

fn slice_lines(text: &str, start: Option<u32>, end: Option<u32>) -> String {
    match (start, end) {
        (Some(s), Some(e)) if e >= s => text
            .lines()
            .skip((s.saturating_sub(1)) as usize)
            .take((e - s + 1) as usize)
            .collect::<Vec<_>>()
            .join("\n"),
        (Some(s), None) => text
            .lines()
            .skip((s.saturating_sub(1)) as usize)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => text.to_string(),
    }
}

fn truncate_for_tool(s: &str) -> String {
    if s.len() <= MAX_TOOL_RESULT_BYTES {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(MAX_TOOL_RESULT_BYTES).collect();
        t.push_str("\n[…truncated]");
        t
    }
}

fn shell_quote(value: &str) -> String {
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

pub async fn execute_for_scope(
    scope: &BrainScope,
    name: &str,
    args: &Value,
    sessions: &Arc<crate::sessions::SessionRegistry>,
) -> AppResult<String> {
    match scope {
        BrainScope::Local { root } => execute_local(name, args, root).await,
        BrainScope::Remote {
            host_id,
            remote_root,
            ..
        } => {
            let sessions_for_host = sessions.list_for_host(host_id).await;
            let session = sessions_for_host.into_iter().next().ok_or_else(|| {
                AppError::Invalid(format!(
                    "no active SSH session for host {host_id} — connect first"
                ))
            })?;
            execute_remote(name, args, &session, remote_root).await
        }
    }
}
