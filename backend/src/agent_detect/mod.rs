use crate::errors::AppResult;
use crate::ssh::SshSession;

/// Best-effort discovery of candidate project roots on the remote host, used to
/// populate the Project Index "pick a VPS project" dropdown. Searches under
/// `$HOME` for language-manifest markers (shallow + pruned for speed) and
/// dedupes nested matches to their top-level root. Returns absolute paths.
///
/// SECURITY: the command is fully static (no interpolation), so there's no
/// injection surface. The returned paths come from a semi-trusted remote and
/// are only ever shown to the user as dropdown options or passed back as an
/// explicit, user-chosen index target.
pub async fn discover_project_roots(session: &SshSession) -> AppResult<Vec<String>> {
    // Prune heavy dirs, match common language manifests, strip each match to
    // its parent dir, then `sort -u | awk` collapses children under an already
    // listed root (the awk strict-prefix pass REQUIRES the preceding `sort -u`).
    // Prune ALL hidden dirs (.npm, .cache, .config, .cargo, .nvm, .git, …) plus
    // common build/dep dirs, so package-manager caches (e.g. ~/.npm/_npx/<hash>,
    // which contain a package.json) never look like a real project.
    const ROOTS_CMD: &str = r#"find "$HOME" -maxdepth 4 \( -name '.*' -o -name node_modules -o -name vendor -o -name target -o -name dist -o -name build -o -name __pycache__ -o -name site-packages \) -prune -o \( -name package.json -o -name Cargo.toml -o -name go.mod -o -name pyproject.toml -o -name pom.xml -o -name Gemfile -o -name composer.json -o -name requirements.txt \) -print 2>/dev/null | sed 's#/[^/]*$##' | sort -u | awk 'NR==1{print;prev=$0"/";next} index($0,prev)!=1{print;prev=$0"/"}' | head -n 80"#;
    let out = session.exec_oneshot(ROOTS_CMD, 64 * 1024).await?;
    let text = String::from_utf8_lossy(&out);
    let mut roots: Vec<String> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| l.starts_with('/') && !is_noise_project_path(l))
        .map(|l| l.to_string())
        .collect();
    roots.dedup();
    Ok(roots)
}

/// True for paths that are package-manager caches, hidden dirs, or build output
/// rather than real user projects (e.g. `/root/.npm/_npx/<hash>`). Keeps the
/// VPS-project dropdown free of noise — used for discovery AND for the detected
/// agent cwd before it's offered as a project.
pub fn is_noise_project_path(path: &str) -> bool {
    path.split('/').any(|seg| {
        (seg.starts_with('.') && seg.len() > 1)
            || matches!(
                seg,
                "node_modules"
                    | "_npx"
                    | "vendor"
                    | "target"
                    | "dist"
                    | "build"
                    | "__pycache__"
                    | "site-packages"
            )
    })
}

/// Best-effort: ask the remote what's running and infer the AI agent.
/// Process listing scoped to THIS SSH connection's interactive session, not the
/// whole host. The agent the user runs (`claude`, `codex`, …) is a descendant
/// of this connection's login shell, so it shares the shell's session id (SID).
///
/// `$SSH_CONNECTION` ("clientip clientport serverip serverport") is unique per
/// SSH connection and inherited by every channel/process under it — including a
/// second tab opened to the SAME VPS (different client port). On Linux we find
/// the session leaders whose `/proc/<pid>/environ` carries our SSH_CONNECTION
/// and list only `ps --sid` of those, so we never see another user's or another
/// terminal's agent. `/proc/<pid>/environ` is only readable for our own uid, so
/// other users' processes are invisible to us by construction.
///
/// Hosts without `/proc` (macOS/BSD servers) fall back to current-user scope —
/// still strictly narrower than the old global `ps -e`.
const SCOPED_PS_CMD: &str = r#"sc="$SSH_CONNECTION"
if [ -n "$sc" ] && [ -d /proc ]; then
  sids=
  for d in /proc/[0-9]*; do
    p=${d#/proc/}
    if tr '\0' '\n' < "$d/environ" 2>/dev/null | grep -qxF "SSH_CONNECTION=$sc"; then
      s=$(awk '{print $6}' "$d/stat" 2>/dev/null)
      [ "$s" = "$p" ] && sids="$sids $p"
    fi
  done
  if [ -n "$sids" ]; then
    for s in $sids; do ps -o pid= -o command= --sid "$s" 2>/dev/null; done
    exit 0
  fi
fi
ps -u "$(id -un)" -o pid= -o command= 2>/dev/null || ps -e -o pid,command 2>/dev/null"#;

/// Map a (lowercased) command line to the agent it represents, if any.
fn classify(lower: &str) -> Option<AgentKind> {
    for kind in AgentKind::ALL {
        if has_word(lower, kind.keywords()) {
            return Some(*kind);
        }
    }
    None
}

/// Returns (kind, pid) of the matched agent process, or None.
///
/// SECURITY: the output of `ps` comes from a semi-trusted remote and
/// could be forged. The renderer must show the detected name to the user
/// and allow manual override before formatting any paste content.
pub async fn detect(session: &SshSession) -> AppResult<Option<(AgentKind, u32)>> {
    let out = session.exec_oneshot(SCOPED_PS_CMD, 128 * 1024).await?;
    let text = String::from_utf8_lossy(&out);
    tracing::debug!(bytes = out.len(), "ran scoped ps for agent detect");

    let mut found: Option<(AgentKind, u32)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (pid_str, cmd) = match line.split_once(char::is_whitespace) {
            Some((p, c)) => (p.trim(), c.trim()),
            None => continue,
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        let lower = cmd.to_ascii_lowercase();
        let Some(kind) = classify(&lower) else {
            continue;
        };
        // Prefer the most-recent (highest PID) match — newest user process.
        if found.as_ref().map_or(true, |(_, p)| pid > *p) {
            found = Some((kind, pid));
        }
    }
    Ok(found)
}

/// Confirm a cached `(kind, pid)` still refers to a live process whose command
/// still matches that agent. Guards against the agent exiting and its PID being
/// recycled by an unrelated process, which would otherwise make us resolve the
/// wrong process's cwd. Cheap: a single `ps -p`.
pub async fn pid_matches(session: &SshSession, kind: AgentKind, pid: u32) -> bool {
    let cmd = format!("ps -p {pid} -o command= 2>/dev/null");
    let Ok(out) = session.exec_oneshot(&cmd, 8192).await else {
        return false;
    };
    let text = String::from_utf8_lossy(&out);
    let line = text.trim();
    if line.is_empty() {
        return false;
    }
    classify(&line.to_ascii_lowercase()) == Some(kind)
}

/// Resolve the agent process's current working directory. Linux uses
/// `/proc/<pid>/cwd`; macOS/BSD falls back to `lsof`. Returns None if either
/// the command fails or the path looks suspicious (e.g., still `/proc/...`).
pub async fn agent_cwd(session: &SshSession, pid: u32) -> Option<String> {
    let cmd = format!(
        "readlink /proc/{pid}/cwd 2>/dev/null || lsof -a -p {pid} -d cwd -Fn 2>/dev/null | awk '/^n/{{print substr($0,2); exit}}'"
    );
    let out = session.exec_oneshot(&cmd, 4096).await.ok()?;
    let s = String::from_utf8_lossy(&out).trim().to_string();
    if s.is_empty() || s.starts_with("readlink:") || s.starts_with("lsof:") || !s.starts_with('/') {
        return None;
    }
    Some(s)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Aider,
    Codex,
    Gemini,
    CursorAgent,
}

impl AgentKind {
    /// All variants, in detection-priority order.
    pub const ALL: &'static [AgentKind] = &[
        AgentKind::Claude,
        AgentKind::Aider,
        AgentKind::Codex,
        AgentKind::Gemini,
        AgentKind::CursorAgent,
    ];

    /// Command-line keywords (word-boundary matched) that identify this agent.
    pub fn keywords(&self) -> &'static [&'static str] {
        match self {
            AgentKind::Claude => &["claude", "claude-code"],
            AgentKind::Aider => &["aider"],
            AgentKind::Codex => &["codex"],
            AgentKind::Gemini => &["gemini"],
            AgentKind::CursorAgent => &["cursor-agent"],
        }
    }

    /// Format the path reference the user is about to paste into the remote prompt.
    /// Never includes a trailing newline — the user always presses Enter manually.
    ///
    /// Format choices per agent:
    /// - Claude Code: `@path` — the CLI's documented file-reference syntax that
    ///   actually attaches the file to the model call (plain path is treated
    ///   as literal text and the model sees no image).
    /// - Codex: `@path` — same convention.
    /// - Aider: `/add path` — aider's chat command for adding files.
    /// - Gemini CLI: `@path` — Gemini's file-attach syntax.
    /// - cursor-agent: `@path` — Cursor's file-attach syntax.
    pub fn format_path(&self, remote_path: &str) -> String {
        match self {
            AgentKind::Aider => format!("/add {remote_path}"),
            // Every other modern AI CLI uses @path as the file-attach syntax.
            _ => format!("@{remote_path}"),
        }
    }
}

fn has_word(haystack: &str, needles: &[&str]) -> bool {
    let h = haystack.to_ascii_lowercase();
    needles.iter().any(|n| {
        // word boundary check: surrounded by non-alnum or string edges.
        h.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
            .any(|tok| tok == *n)
    })
}
