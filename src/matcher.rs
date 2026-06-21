use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::scanner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub file: PathBuf,
    pub line: usize,
    pub content: String,
}

pub fn match_files(files: &[PathBuf], regex: &Regex) -> Vec<Match> {
    let mut results = Vec::new();
    for file in files {
        results.extend(match_single_file(file, regex));
    }
    results.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    results
}

pub fn brute_force(path: &Path, regex: &Regex) -> Vec<Match> {
    let files = scanner::scan(path);
    match_files(&files, regex)
}

pub fn format_results(results: &[Match]) -> String {
    results
        .iter()
        .map(|m| format!("{}:{}:{}", m.file.display(), m.line, m.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn match_single_file(path: &Path, regex: &Regex) -> Vec<Match> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };

    contents
        .split('\n')
        .enumerate()
        .filter(|&(_, line)| regex.is_match(line))
        .map(|(idx, line)| Match {
            file: path.to_path_buf(),
            line: idx + 1,
            content: line.to_string(),
        })
        .collect()
}

pub fn compile_regex(pattern: &str, ignore_case: bool) -> io::Result<Regex> {
    let pattern = if ignore_case {
        format!("(?i:{pattern})")
    } else {
        pattern.to_string()
    };
    Regex::new(&pattern)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, format!("Invalid regex: {err}")))
}
