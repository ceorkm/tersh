//! Resolve per-language import strings to project-relative paths.
//!
//! Used by the dependency-graph builder to convert raw import statements
//! (`use crate::brain::tools` / `import {x} from "./foo"`) into edges in a
//! file-to-file graph. Resolutions that point at external packages (npm
//! deps, crates.io, etc.) return None.

use std::collections::HashSet;
use std::path::Path;

/// Try to resolve a single import string from `from_path` (project-relative
/// path of the file containing the import) against the known set of
/// project paths. Returns the matching project-relative path if it
/// resolves to something inside the project, else None.
pub fn resolve_import(
    from_path: &str,
    import: &str,
    language: &str,
    all_paths: &HashSet<String>,
) -> Option<String> {
    let import = import
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`');
    if import.is_empty() {
        return None;
    }

    match language {
        "typescript" | "javascript" | "tsx" | "jsx" => {
            resolve_js_like(from_path, import, all_paths)
        }
        "rust" => resolve_rust(import, all_paths),
        "python" => resolve_python(from_path, import, all_paths),
        "go" => resolve_go(import, all_paths),
        _ => None,
    }
}

fn resolve_js_like(from_path: &str, import: &str, all_paths: &HashSet<String>) -> Option<String> {
    if !import.starts_with('.') && !import.starts_with('/') && !import.starts_with('@') {
        // Bare package — not a project file.
        return None;
    }
    let from_dir = Path::new(from_path).parent()?;
    let joined = if let Some(stripped) = import.strip_prefix('@') {
        // @-aliased imports usually map to src/. Best-effort: treat as
        // project-rooted.
        let mut p = std::path::PathBuf::from("src");
        for component in stripped.split('/').skip(1) {
            p.push(component);
        }
        p
    } else {
        from_dir.join(import)
    };
    let normalized = normalize_path(&joined)?;
    let candidates = [
        format!("{normalized}.ts"),
        format!("{normalized}.tsx"),
        format!("{normalized}.js"),
        format!("{normalized}.jsx"),
        format!("{normalized}/index.ts"),
        format!("{normalized}/index.tsx"),
        format!("{normalized}/index.js"),
        format!("{normalized}/index.jsx"),
        normalized.clone(),
    ];
    for candidate in candidates {
        if all_paths.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn resolve_rust(import: &str, all_paths: &HashSet<String>) -> Option<String> {
    // `use crate::brain::tools` → look for backend/src/brain/tools.rs or
    // backend/src/brain/tools/mod.rs.
    let path = import
        .trim_start_matches("use ")
        .trim_end_matches(';')
        .trim();
    let parts: Vec<&str> = path.split("::").collect();
    if parts.is_empty() {
        return None;
    }
    let first = parts.first()?;
    if !matches!(*first, "crate" | "super" | "self") {
        return None;
    }
    if parts.len() < 2 {
        return None;
    }
    let tail = parts[1..].join("/");
    let candidates = [
        format!("backend/src/{tail}.rs"),
        format!("backend/src/{tail}/mod.rs"),
        format!("src/{tail}.rs"),
        format!("src/{tail}/mod.rs"),
    ];
    for candidate in candidates {
        if all_paths.contains(&candidate) {
            return Some(candidate);
        }
    }
    // Some imports name only the first segment after crate (e.g. `crate::foo`).
    // Try a fuzzier suffix match.
    for path in all_paths {
        if path.ends_with(&format!("/{tail}.rs")) || path.ends_with(&format!("/{tail}/mod.rs")) {
            return Some(path.clone());
        }
    }
    None
}

fn resolve_python(from_path: &str, import: &str, all_paths: &HashSet<String>) -> Option<String> {
    let trimmed = import.trim();
    // `from .foo` / `from ..foo.bar` — relative to current package.
    if let Some(rel) = trimmed.strip_prefix('.') {
        let dots = trimmed.chars().take_while(|c| *c == '.').count();
        let rest = rel.trim_start_matches('.');
        let from_dir = Path::new(from_path).parent()?;
        let mut up = from_dir.to_path_buf();
        for _ in 1..dots {
            up = up.parent()?.to_path_buf();
        }
        let target = up.join(rest.replace('.', "/"));
        let target_str = normalize_path(&target)?;
        let candidates = [
            format!("{target_str}.py"),
            format!("{target_str}/__init__.py"),
        ];
        for candidate in candidates {
            if all_paths.contains(&candidate) {
                return Some(candidate);
            }
        }
        return None;
    }
    // Absolute package import — try `src/<package>.py` etc.
    let dotted = trimmed.replace('.', "/");
    let candidates = [
        format!("{dotted}.py"),
        format!("{dotted}/__init__.py"),
        format!("src/{dotted}.py"),
        format!("src/{dotted}/__init__.py"),
    ];
    for candidate in candidates {
        if all_paths.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn resolve_go(import: &str, all_paths: &HashSet<String>) -> Option<String> {
    // Best-effort: Go modules use full import paths; we only match if the
    // import path ends with one of our project subdirs.
    for path in all_paths {
        if let Some(parent) = Path::new(path).parent().and_then(|p| p.to_str()) {
            if import.ends_with(parent) {
                return Some(path.clone());
            }
        }
    }
    None
}

fn normalize_path(p: &Path) -> Option<String> {
    let mut out = std::path::PathBuf::new();
    for component in p.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::Normal(s) => out.push(s),
            std::path::Component::RootDir => return None,
            std::path::Component::Prefix(_) => return None,
        }
    }
    Some(out.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn js_relative_import_resolves_with_tsx_extension() {
        let all = paths(&["frontend/src/components/Drawer.tsx"]);
        let resolved = resolve_import(
            "frontend/src/App.tsx",
            "./components/Drawer",
            "typescript",
            &all,
        );
        assert_eq!(
            resolved.as_deref(),
            Some("frontend/src/components/Drawer.tsx")
        );
    }

    #[test]
    fn js_index_import_resolves() {
        let all = paths(&["frontend/src/lib/api/index.ts"]);
        let resolved = resolve_import("frontend/src/App.tsx", "./lib/api", "typescript", &all);
        assert_eq!(resolved.as_deref(), Some("frontend/src/lib/api/index.ts"));
    }

    #[test]
    fn js_bare_package_is_external() {
        let all = paths(&["frontend/src/App.tsx"]);
        let resolved = resolve_import("frontend/src/App.tsx", "react", "typescript", &all);
        assert!(resolved.is_none());
    }

    #[test]
    fn rust_crate_path_resolves_to_mod_file() {
        let all = paths(&["backend/src/brain/tools.rs"]);
        let resolved = resolve_import(
            "backend/src/commands.rs",
            "use crate::brain::tools",
            "rust",
            &all,
        );
        assert_eq!(resolved.as_deref(), Some("backend/src/brain/tools.rs"));
    }

    #[test]
    fn rust_crate_path_falls_back_to_suffix_match() {
        let all = paths(&["backend/src/ssh/mod.rs"]);
        let resolved = resolve_import("backend/src/commands.rs", "use crate::ssh", "rust", &all);
        assert_eq!(resolved.as_deref(), Some("backend/src/ssh/mod.rs"));
    }

    #[test]
    fn python_relative_import_resolves() {
        let all = paths(&["app/utils/helpers.py"]);
        let resolved = resolve_import("app/views.py", ".utils.helpers", "python", &all);
        // `from .utils.helpers import x` → relative to app/
        assert_eq!(resolved.as_deref(), Some("app/utils/helpers.py"));
    }
}
