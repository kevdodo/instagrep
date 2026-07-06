//! File discovery.
//!
//! Directory traversal is delegated to the [`ignore`] crate (the same engine
//! ripgrep uses), giving correct, nested `.gitignore`/`.ignore` handling and a
//! parallel-aware walker. We then apply instagrep-specific filters: a maximum
//! file size, a binary-extension blocklist, and the user's `--glob` rules.
//!
//! Returned paths are **relative to the search root**, which keeps the index
//! portable and the output clean (`instagrep pat .` ⇒ `./src/foo.rs`).

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

pub const DEFAULT_MAX_FILE_SIZE: u64 = 1 << 20; // 1 MiB

/// Extensions we never bother indexing (detected at walk time; binary content is
/// also detected by NUL-byte sampling when a file is read).
const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "svg", "woff", "woff2", "ttf", "eot", "mp3", "mp4",
    "avi", "mov", "pdf", "zip", "tar", "gz", "bz2", "xz", "zst", "7z", "rar", "exe", "dll", "so",
    "dylib", "o", "a", "beam", "class", "jar", "war", "pyc", "pyo", "lock",
];

/// Directories the walker always skips (in addition to gitignore rules).
const IGNORED_DIRS: &[&str] = &[".git", "node_modules", ".instantgrep", "target"];

/// Controls which files the walker considers indexable.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    pub no_ignore: bool,
    pub hidden: bool,
    pub globs: Vec<String>,
    pub max_file_size: u64,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            no_ignore: false,
            hidden: false,
            globs: Vec::new(),
            max_file_size: DEFAULT_MAX_FILE_SIZE,
        }
    }
}

/// Recursively collect indexable files under `root`, as paths relative to
/// `root`, sorted for deterministic output.
pub fn scan(root: &Path, opts: &WalkOptions) -> Vec<PathBuf> {
    let walker = build_walker(root, opts);
    let (includes, excludes) = split_globs(&opts.globs);

    let mut files = Vec::new();
    for result in walker {
        let Ok(entry) = result else { continue };
        let ft = match entry.file_type() {
            Some(ft) => ft,
            None => continue,
        };
        if !ft.is_file() {
            continue;
        }
        let path = entry.path();
        if ignored_dir_component(path) {
            continue;
        }
        if let Ok(meta) = entry.metadata()
            && (meta.len() == 0 || meta.len() > opts.max_file_size)
        {
            continue;
        }
        if is_binary_ext(path) {
            continue;
        }
        let basename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !opts.hidden && basename.starts_with('.') && path.extension().is_none() {
            continue;
        }
        if !matches_globs(path, basename, &includes, &excludes) {
            continue;
        }

        let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
        files.push(rel);
    }
    files.sort();
    files
}

fn build_walker(root: &Path, opts: &WalkOptions) -> ignore::Walk {
    let mut wb = WalkBuilder::new(root);
    wb.hidden(!opts.hidden);
    let respect = !opts.no_ignore;
    wb.ignore(respect);
    wb.git_ignore(respect);
    wb.git_global(respect);
    wb.git_exclude(respect);
    wb.parents(true);
    // Match ripgrep: only honour .gitignore/.git globally inside a git worktree.
    // Outside one (no .git ancestor) both tools ignore those rules.
    wb.require_git(true);
    wb.follow_links(false);
    wb.build()
}

fn is_binary_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| BINARY_EXTENSIONS.iter().any(|known| ext.eq_ignore_ascii_case(known)))
        .unwrap_or(false)
}

fn ignored_dir_component(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|name| IGNORED_DIRS.contains(&name))
            .unwrap_or(false)
    })
}

/// Split `--glob` rules into include patterns and `!`-prefixed excludes.
fn split_globs(globs: &[String]) -> (Vec<String>, Vec<String>) {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    for g in globs {
        if let Some(rest) = g.strip_prefix('!') {
            excludes.push(rest.to_string());
        } else {
            includes.push(g.clone());
        }
    }
    (includes, excludes)
}

fn matches_globs(path: &Path, basename: &str, includes: &[String], excludes: &[String]) -> bool {
    let rel = path.to_string_lossy().replace('\\', "/");
    for pat in excludes {
        if glob_match(pat, &rel) || glob_match(pat, basename) {
            return false;
        }
    }
    if includes.is_empty() {
        return true;
    }
    includes.iter().any(|pat| glob_match(pat, &rel) || glob_match(pat, basename))
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
    let mut star: Option<usize> = None;
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

/// Heuristic binary check: a NUL byte in the leading sample marks a file binary.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_basename_and_path() {
        assert!(glob_match("*.rs", "foo.rs"));
        assert!(!glob_match("*.rs", "foo.py"));
        assert!(glob_match("src/*", "src/main"));
    }
}
