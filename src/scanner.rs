use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_MAX_FILE_SIZE: u64 = 1_048_576;

const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "svg", "woff", "woff2", "ttf", "eot", "mp3", "mp4",
    "avi", "mov", "pdf", "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "exe", "dll", "so", "dylib",
    "o", "a", "beam", "class", "jar", "war", "pyc", "pyo", "lock",
];

const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "_build",
    "deps",
    ".instantgrep",
    ".elixir_ls",
    ".idea",
    ".vscode",
    "target",
    "vendor",
];

#[derive(Debug, Clone)]
struct IgnoreRule {
    raw: String,
}

pub fn scan(path: &Path) -> Vec<PathBuf> {
    let base = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let ignore_base = if base.is_dir() {
        base.clone()
    } else {
        base.parent().unwrap_or(&base).to_path_buf()
    };
    let rules = load_gitignore(&ignore_base);
    let mut files = Vec::new();
    scan_inner(&base, &rules, &mut files);
    files.sort();
    files
}

fn scan_inner(path: &Path, rules: &[IgnoreRule], files: &mut Vec<PathBuf>) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };

    if meta.is_file() {
        if is_indexable_file(path, meta.len(), rules) {
            files.push(path.to_path_buf());
        }
        return;
    }

    if !meta.is_dir() || ignored_dir(path) {
        return;
    }

    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        scan_inner(&entry.path(), rules, files);
    }
}

pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0)
}

fn is_indexable_file(path: &Path, size: u64, rules: &[IgnoreRule]) -> bool {
    if size == 0 || size > DEFAULT_MAX_FILE_SIZE {
        return false;
    }

    let basename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if basename.starts_with('.') && path.extension().is_none() {
        return false;
    }

    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            BINARY_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
        .unwrap_or(false)
    {
        return false;
    }

    !rules.iter().any(|rule| rule.matches(path))
}

fn ignored_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| IGNORED_DIRS.contains(&name))
        .unwrap_or(false)
}

fn load_gitignore(base: &Path) -> Vec<IgnoreRule> {
    let path = base.join(".gitignore");
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };

    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        .map(|line| IgnoreRule {
            raw: line.trim_end_matches('/').to_string(),
        })
        .collect()
}

impl IgnoreRule {
    fn matches(&self, path: &Path) -> bool {
        let text = path.to_string_lossy();
        let normalized = text.replace('\\', "/");
        glob_match(&self.raw, &normalized)
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| glob_match(&self.raw, name))
                .unwrap_or(false)
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        return text.contains(pattern);
    }
    glob_match_bytes(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_bytes(pattern: &[u8], text: &[u8]) -> bool {
    let mut p = 0;
    let mut t = 0;
    let mut star = None;
    let mut star_text = 0;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_text = t;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}
