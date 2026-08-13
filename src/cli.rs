//! Command-line interface and top-level orchestration.

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;

use crate::index;
use crate::matcher::{self, SearchMode, SearchOptions};
use crate::query::{self, CandidateSet};
use crate::scanner::WalkOptions;

#[derive(Parser, Debug)]
#[command(
    name = "instagrep",
    version,
    about = "Fast grep-style search using a persisted roaring-bitmap trigram index"
)]
struct Cli {
    /// Search pattern (regular expression)
    pattern: Option<String>,
    /// Path to search (defaults to current directory)
    path: Option<PathBuf>,

    /// Build or rebuild the index, then exit
    #[arg(long)]
    build: bool,
    /// Update the index incrementally, then exit
    #[arg(long)]
    update: bool,
    /// Skip the index and brute-force scan every file
    #[arg(long = "no-index")]
    no_index: bool,
    /// Case-insensitive matching
    #[arg(short = 'i', long)]
    ignore_case: bool,
    /// Show index statistics, then exit
    #[arg(long)]
    stats: bool,
    /// Print per-phase timing to stderr
    #[arg(long)]
    time: bool,
    /// Print only the path of files with at least one match
    #[arg(short = 'l', long = "files-with-matches")]
    files_with_matches: bool,
    /// Print only a match count per file
    #[arg(short = 'c', long = "count")]
    count: bool,
    /// Lines of context to show after each match
    #[arg(short = 'A', long = "after-context", value_name = "N")]
    after: Option<usize>,
    /// Lines of context to show before each match
    #[arg(short = 'B', long = "before-context", value_name = "N")]
    before: Option<usize>,
    /// Lines of context to show around each match
    #[arg(short = 'C', long = "context", value_name = "N")]
    context: Option<usize>,
    /// Include/exclude files matching a glob (repeatable; `!` excludes)
    #[arg(short = 'g', long = "glob", value_name = "GLOB")]
    globs: Vec<String>,
    /// Search hidden files and directories
    #[arg(long)]
    hidden: bool,
    /// Do not respect .gitignore / .ignore files
    #[arg(long = "no-ignore")]
    no_ignore: bool,
    /// When to colour matches [auto, always, never]
    #[arg(long, value_name = "WHEN", default_value = "auto")]
    color: String,
}

pub fn run() -> io::Result<i32> {
    // `instagrep mcp` runs the MCP server (a coding-agent `search` tool).
    // Intercept before clap so "mcp" isn't parsed as a search pattern.
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        return crate::mcp::serve();
    }

    let cli = Cli::parse();

    let root = if cli.build || cli.update || cli.stats {
        cli.path.clone().or_else(|| cli.pattern.clone().map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("."))
    } else {
        cli.path.clone().unwrap_or_else(|| PathBuf::from("."))
    };

    let walk_opts = WalkOptions {
        no_ignore: cli.no_ignore,
        hidden: cli.hidden,
        globs: cli.globs.clone(),
        max_file_size: crate::scanner::DEFAULT_MAX_FILE_SIZE,
    };

    if cli.build {
        println!("Building index for {}...", root.display());
        let index = index::build(&root, &walk_opts);
        index::save(&index, &root)?;
        index.print_stats(&root);
        println!("Index saved to {}/", root.join(".instantgrep").display());
        return Ok(0);
    }

    if cli.update {
        println!("Updating index for {}...", root.display());
        let index = index::update(&root, &walk_opts)?;
        index.print_stats(&root);
        return Ok(0);
    }

    if cli.stats {
        return match index::load(&root)? {
            Some(index) => {
                index.print_stats(&root);
                Ok(0)
            }
            None => {
                eprintln!("No index found. Run: instagrep --build {}", root.display());
                Ok(1)
            }
        };
    }

    let Some(pattern) = cli.pattern.as_deref() else {
        eprintln!("Error: no pattern specified. Run: instagrep --help");
        return Ok(1);
    };

    let search_opts = build_search_options(&cli);

    if cli.no_index {
        return search_brute_force(pattern, &root, &walk_opts, &search_opts, cli.ignore_case, cli.time);
    }
    search_indexed(pattern, &root, &walk_opts, &search_opts, cli.ignore_case, cli.time)
}

fn build_search_options(cli: &Cli) -> SearchOptions {
    let context = cli.context.unwrap_or(0);
    let before = cli.before.unwrap_or(context);
    let after = cli.after.unwrap_or(context);
    let mode = if cli.files_with_matches {
        SearchMode::FilesWithMatches
    } else if cli.count {
        SearchMode::Count
    } else {
        SearchMode::Normal
    };
    SearchOptions {
        mode,
        before,
        after,
        color: matcher::resolve_color(&cli.color),
    }
}

fn search_brute_force(
    pattern: &str,
    root: &Path,
    walk_opts: &WalkOptions,
    opts: &SearchOptions,
    ignore_case: bool,
    time: bool,
) -> io::Result<i32> {
    let regex = matcher::compile_regex(pattern, ignore_case)?;
    let started = Instant::now();
    let files = crate::scanner::scan(root, walk_opts);
    let scan_us = started.elapsed().as_micros();

    let started = Instant::now();
    let results = matcher::search_files(&files, root, &regex, opts);
    let match_us = started.elapsed().as_micros();

    let printed = emit(results, root, opts)?;

    if time {
        eprintln!();
        eprintln!("--- timing (pattern: {pattern:?}, brute-force) ---");
        eprintln!("  scan:        {}", fmt_us(scan_us));
        eprintln!("  regex match: {}  ({} files)", fmt_us(match_us), files.len());
        eprintln!("  matches:     {}", printed);
    }
    Ok(if printed > 0 { 0 } else { 1 })
}

fn search_indexed(
    pattern: &str,
    root: &Path,
    walk_opts: &WalkOptions,
    opts: &SearchOptions,
    ignore_case: bool,
    time: bool,
) -> io::Result<i32> {
    let regex = matcher::compile_regex(pattern, ignore_case)?;

    let started = Instant::now();
    let index = match index::load(root)? {
        Some(index) => index,
        None => {
            eprintln!("No index found, building...");
            let index = index::build(root, walk_opts);
            index::save(&index, root)?;
            index
        }
    };
    let load_us = started.elapsed().as_micros();

    let started = Instant::now();
    let query_pattern = if ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    let query = query::analyze(&query_pattern);
    let candidate_set = query::evaluate(&query, |trigram| index.postings.get(&trigram).cloned());
    let eval_us = started.elapsed().as_micros();

    let (candidate_files, candidate_count) = match &candidate_set {
        CandidateSet::All => (index.resolve_files(None), index.files.len()),
        CandidateSet::Some(bm) => (index.resolve_files(Some(bm)), bm.len() as usize),
    };

    let started = Instant::now();
    let results = matcher::search_files(&candidate_files, root, &regex, opts);
    let match_us = started.elapsed().as_micros();

    let printed = emit(results, root, opts)?;

    if time {
        let total_us = load_us + eval_us + match_us;
        eprintln!();
        eprintln!("--- timing (pattern: {pattern:?}) ---");
        eprintln!("  index load:   {}", fmt_us(load_us));
        eprintln!(
            "  trigram eval: {}  ({}/{} files candidates)",
            fmt_us(eval_us),
            candidate_count,
            index.files.len()
        );
        eprintln!("  regex verify: {}  ({} matches)", fmt_us(match_us), printed);
        eprintln!("  total:        {}", fmt_us(total_us));
    }
    Ok(if printed > 0 { 0 } else { 1 })
}

/// Stream results to buffered stdout; returns the number of matched lines
/// printed (0 for `-l`, which prints paths, not lines).
fn emit(results: Vec<matcher::FileResult>, root: &Path, opts: &SearchOptions) -> io::Result<usize> {
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let printed = matcher::write_results(&mut out, &results, root, opts)?;
    out.flush()?;
    Ok(printed)
}

fn fmt_us(us: u128) -> String {
    if us < 1_000 {
        format!("{us}us")
    } else if us < 1_000_000 {
        format!("{:.2}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.3}s", us as f64 / 1_000_000.0)
    }
}
