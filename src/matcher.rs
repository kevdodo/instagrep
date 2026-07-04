//! Regex verification of candidate files.
//!
//! Candidate files (narrowed by the trigram index, or all of them in
//! brute-force mode) are matched in parallel with [`regex::bytes::Regex`].
//! Operating on raw bytes — not `String` — means files with invalid UTF-8 are
//! still searched correctly (a regression in the previous implementation, which
//! silently dropped them via `read_to_string`).

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use regex::bytes::Regex;
use rustc_hash::FxHashMap;

use crate::scanner;

const COLOR_RED: &[u8] = b"\x1b[31m";
const COLOR_RESET: &[u8] = b"\x1b[0m";
const BINARY_SAMPLE: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Normal,
    Count,
    FilesWithMatches,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorWhen {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub mode: SearchMode,
    pub before: usize,
    pub after: usize,
    pub color: ColorWhen,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            mode: SearchMode::Normal,
            before: 0,
            after: 0,
            color: ColorWhen::Auto,
        }
    }
}

/// A line to print, with the byte spans (relative to `content`) of any matches
/// on it — kept as bytes so colouring can splice ANSI codes at exact offsets.
#[derive(Debug)]
pub struct PrintEntry {
    pub line: usize,
    pub content: Vec<u8>,
    pub spans: Vec<(usize, usize)>,
    pub is_match: bool,
}

#[derive(Debug)]
pub struct FileResult {
    pub rel_path: PathBuf,
    pub match_count: usize,
    pub entries: Vec<PrintEntry>,
}

/// Verify `rel_paths` (relative to `base`) against `regex` in parallel.
pub fn search_files(
    rel_paths: &[PathBuf],
    base: &Path,
    regex: &Regex,
    opts: &SearchOptions,
) -> Vec<FileResult> {
    rel_paths
        .par_iter()
        .filter_map(|rel| match_file(base.join(rel), rel.clone(), regex, opts))
        .collect()
}

fn match_file(abs: PathBuf, rel_path: PathBuf, regex: &Regex, opts: &SearchOptions) -> Option<FileResult> {
    let bytes = fs::read(&abs).ok()?;
    if scanner::looks_binary(&bytes[..bytes.len().min(BINARY_SAMPLE)]) {
        return None;
    }

    let line_starts = line_start_offsets(&bytes);
    let total_lines = line_starts.len().max(1);

    // Match line number → byte spans within that line.
    let mut spans_by_line: FxHashMap<usize, Vec<(usize, usize)>> = FxHashMap::default();
    for m in regex.find_iter(&bytes) {
        let lineno = line_number(&line_starts, m.start());
        let lstart = line_starts[lineno - 1];
        spans_by_line.entry(lineno).or_default().push((m.start() - lstart, m.end() - lstart));
    }
    if spans_by_line.is_empty() {
        return None;
    }

    let mut matched_lines: Vec<usize> = spans_by_line.keys().copied().collect();
    matched_lines.sort_unstable();
    let match_count = matched_lines.len();

    let want_context = opts.before > 0 || opts.after > 0;
    let entries = if want_context {
        build_context_entries(&bytes, &line_starts, total_lines, &matched_lines, &spans_by_line, opts)
    } else {
        matched_lines
            .iter()
            .map(|&lineno| {
                let lstart = line_starts[lineno - 1];
                let lend = line_end(&bytes, &line_starts, lineno);
                let spans = spans_by_line.get(&lineno).cloned().unwrap_or_default();
                PrintEntry {
                    line: lineno,
                    content: bytes[lstart..lend].to_vec(),
                    spans,
                    is_match: true,
                }
            })
            .collect()
    };

    Some(FileResult { rel_path, match_count, entries })
}

/// Emit the lines to print for context mode: each match plus `before`/`after`
/// surrounding lines, with a separator marker wherever there is a gap.
fn build_context_entries(
    bytes: &[u8],
    line_starts: &[usize],
    total_lines: usize,
    matched_lines: &[usize],
    spans_by_line: &FxHashMap<usize, Vec<(usize, usize)>>,
    opts: &SearchOptions,
) -> Vec<PrintEntry> {
    let mut want: Vec<(usize, bool)> = Vec::new();
    let mut seen = rustc_hash::FxHashSet::default();
    for &m in matched_lines {
        let lo = m.saturating_sub(opts.before).max(1);
        let hi = (m + opts.after).min(total_lines);
        for ln in lo..=hi {
            if seen.insert(ln) {
                want.push((ln, ln == m));
            }
        }
    }
    want.sort_unstable();
    want.into_iter()
        .map(|(ln, is_match)| {
            let lstart = line_starts[ln - 1];
            let lend = line_end(bytes, line_starts, ln);
            let spans = if is_match {
                spans_by_line.get(&ln).cloned().unwrap_or_default()
            } else {
                Vec::new()
            };
            PrintEntry {
                line: ln,
                content: bytes[lstart..lend].to_vec(),
                spans,
                is_match,
            }
        })
        .collect()
}

/// Byte offsets at which each line begins (line 1 starts at 0).
fn line_start_offsets(bytes: &[u8]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(bytes.len() / 64 + 1);
    starts.push(0);
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' && i + 1 < bytes.len() {
            starts.push(i + 1);
        }
    }
    starts
}

/// 1-based line number containing byte offset `pos`.
fn line_number(line_starts: &[usize], pos: usize) -> usize {
    line_starts.partition_point(|&s| s <= pos)
}

/// End byte offset (exclusive) of `lineno`, excluding any trailing newline.
/// Works for both interior lines (whose end is the start of the next line) and
/// the final line (whose end is the end of the buffer).
fn line_end(bytes: &[u8], line_starts: &[usize], lineno: usize) -> usize {
    let mut end = if lineno < line_starts.len() {
        line_starts[lineno]
    } else {
        bytes.len()
    };
    if end > 0 && bytes.get(end - 1) == Some(&b'\n') {
        end -= 1;
        if end > 0 && bytes.get(end - 1) == Some(&b'\r') {
            end -= 1;
        }
    }
    end
}

pub fn compile_regex(pattern: &str, ignore_case: bool) -> io::Result<Regex> {
    let pattern = if ignore_case {
        format!("(?i:{pattern})")
    } else {
        pattern.to_string()
    };
    Regex::new(&pattern).map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, format!("Invalid regex: {err}")))
}

/// Resolve a `--color` value, honouring the terminal for `auto`.
pub fn resolve_color(value: &str) -> ColorWhen {
    match value {
        "always" | "yes" | "force" => ColorWhen::Always,
        "never" | "no" => ColorWhen::Never,
        _ => {
            if io::stdout().is_terminal() {
                ColorWhen::Always
            } else {
                ColorWhen::Never
            }
        }
    }
}

/// Write results to `out` in `path:line:content` form (mode-dependent), with
/// optional ANSI colour on matched substrings. Returns the number of matched
/// lines printed across all files.
pub fn write_results<W: Write>(
    out: &mut W,
    results: &[FileResult],
    base: &Path,
    opts: &SearchOptions,
) -> io::Result<usize> {
    let color = opts.color == ColorWhen::Always;
    let mut printed_lines = 0usize;

    for (i, file) in results.iter().enumerate() {
        match opts.mode {
            SearchMode::FilesWithMatches => {
                writeln!(out, "{}", base.join(&file.rel_path).display())?;
            }
            SearchMode::Count => {
                writeln!(out, "{}:{}", base.join(&file.rel_path).display(), file.match_count)?;
            }
            SearchMode::Normal => {
                let want_context = opts.before > 0 || opts.after > 0;
                if want_context && i > 0 {
                    writeln!(out)?;
                }
                for entry in &file.entries {
                    if want_context && !entry.is_match {
                        // Context line: no match highlight.
                        writeln!(
                            out,
                            "{}-{}-{}",
                            base.join(&file.rel_path).display(),
                            entry.line,
                            lossy(&entry.content)
                        )?;
                    } else {
                        printed_lines += 1;
                        if color && !entry.spans.is_empty() {
                            writeln!(out, "{}:{}:{}", base.join(&file.rel_path).display(), entry.line, colored(&entry.content, &entry.spans))?;
                        } else {
                            writeln!(out, "{}:{}:{}", base.join(&file.rel_path).display(), entry.line, lossy(&entry.content))?;
                        }
                    }
                }
            }
        }
    }
    Ok(printed_lines)
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn colored(content: &[u8], spans: &[(usize, usize)]) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(content.len() + 16);
    let mut prev = 0;
    for &(s, e) in spans {
        if s < prev || e > content.len() || s > e {
            continue;
        }
        out.extend_from_slice(&content[prev..s]);
        out.extend_from_slice(COLOR_RED);
        out.extend_from_slice(&content[s..e]);
        out.extend_from_slice(COLOR_RESET);
        prev = e;
    }
    out.extend_from_slice(&content[prev..]);
    lossy(&out)
}
