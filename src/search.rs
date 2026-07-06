//! The shared indexed-search core.
//!
//! Both the one-shot CLI path and the daemon call into [`run_indexed_search`] so
//! their output is byte-identical — the daemon is purely a "keep the index hot
//! and skip the per-call disk load" optimization.

use std::io;
use std::path::Path;
use std::time::Instant;

use crate::index::Index;
use crate::matcher::{self, SearchOptions};
use crate::query::{self, CandidateSet};

/// The outcome of an indexed search.
pub struct SearchOutcome {
    /// Formatted output bytes (what would be printed to stdout).
    pub output: Vec<u8>,
    /// Process exit code: 0 if anything was printed, 1 otherwise.
    pub exit: i32,
    pub candidate_count: usize,
    pub total_files: usize,
    pub eval_us: u128,
    pub verify_us: u128,
}

/// Compile the regex, prune candidates via the trigram index, and verify them.
pub fn run_indexed_search(
    index: &Index,
    root: &Path,
    pattern: &str,
    ignore_case: bool,
    opts: &SearchOptions,
) -> io::Result<SearchOutcome> {
    let regex = matcher::compile_regex(pattern, ignore_case)?;

    let started = Instant::now();
    let query_pattern = if ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    let query = query::analyze(&query_pattern);
    let candidate_set = query::evaluate(&query, |t| index.postings.get(&t).cloned());
    let eval_us = started.elapsed().as_micros();

    let (candidate_files, candidate_count) = match &candidate_set {
        CandidateSet::All => (index.resolve_files(None), index.files.len()),
        CandidateSet::Some(bm) => (index.resolve_files(Some(bm)), bm.len() as usize),
    };

    let started = Instant::now();
    let results = matcher::search_files(&candidate_files, root, &regex, opts);
    let verify_us = started.elapsed().as_micros();

    let mut output = Vec::new();
    matcher::write_results(&mut output, &results, root, opts)?;
    let exit = if output.is_empty() { 1 } else { 0 };

    Ok(SearchOutcome {
        output,
        exit,
        candidate_count,
        total_files: index.files.len(),
        eval_us,
        verify_us,
    })
}
