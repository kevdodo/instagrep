use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::index;
use crate::matcher;
use crate::query::{self, CandidateSet};

const HELP: &str = "\
instagrep

Usage:
    instagrep [OPTIONS] PATTERN [PATH]

Options:
    --build             Build/rebuild index only
    --update            Update index
    --no-index          Skip index, brute-force scan
    -i, --ignore-case   Case-insensitive matching
    --stats             Show index statistics
    --time              Print per-phase timing to stderr
    -h, --help          Show this help message

Examples:
    instagrep --build .
    instagrep --update .
    instagrep \"pattern\" .
    instagrep -i \"todo|fixme\" src/
    instagrep --no-index \"pattern\" .
";

#[derive(Debug, Default)]
struct Args {
    build: bool,
    update: bool,
    no_index: bool,
    ignore_case: bool,
    stats: bool,
    time: bool,
    help: bool,
    pattern: Option<String>,
    path: PathBuf,
}

pub fn run(args: impl IntoIterator<Item = String>) -> io::Result<i32> {
    let args = parse_args(args)?;
    if args.help {
        println!("{HELP}");
        return Ok(0);
    }

    if args.build {
        println!("Building index for {}...", args.path.display());
        let index = index::build(&args.path);
        index::save(&index, &args.path)?;
        index.print_stats();
        println!(
            "Index saved to {}/",
            args.path.join(".instantgrep").display()
        );
        return Ok(0);
    }

    if args.update {
        println!("Updating index for {}...", args.path.display());
        let index = index::update(&args.path)?;
        index.print_stats();
        return Ok(0);
    }

    if args.stats {
        return match index::load(&args.path)? {
            Some(index) => {
                index.print_stats();
                Ok(0)
            }
            None => {
                eprintln!(
                    "No index found. Run: instagrep --build {}",
                    args.path.display()
                );
                Ok(1)
            }
        };
    }

    let Some(pattern) = args.pattern.as_deref() else {
        eprintln!("Error: no pattern specified. Run: instagrep --help");
        return Ok(1);
    };

    if args.no_index {
        return search_brute_force(pattern, &args);
    }
    search_indexed(pattern, &args)
}

fn parse_args(args: impl IntoIterator<Item = String>) -> io::Result<Args> {
    let mut out = Args {
        path: PathBuf::from("."),
        ..Args::default()
    };
    let mut positional = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--build" => out.build = true,
            "--update" => out.update = true,
            "--no-index" => out.no_index = true,
            "-i" | "--ignore-case" => out.ignore_case = true,
            "--stats" => out.stats = true,
            "--time" => out.time = true,
            "-h" | "--help" => out.help = true,
            "--stop" => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--stop is not supported; this Rust port does not run a daemon",
                ));
            }
            "--daemon" => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--daemon is not supported in this Rust port",
                ));
            }
            flag if flag.starts_with('-') => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown option: {flag}"),
                ));
            }
            _ => positional.push(arg),
        }
    }

    if out.build || out.update || out.stats {
        out.path = positional
            .first()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
    } else {
        out.pattern = positional.first().cloned();
        out.path = positional
            .get(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
    }

    Ok(out)
}

fn search_brute_force(pattern: &str, args: &Args) -> io::Result<i32> {
    let regex = matcher::compile_regex(pattern, args.ignore_case)?;
    let results = matcher::brute_force(&args.path, &regex);
    print_results(&results);
    Ok(0)
}

fn search_indexed(pattern: &str, args: &Args) -> io::Result<i32> {
    let regex = matcher::compile_regex(pattern, args.ignore_case)?;

    let load_started = Instant::now();
    let index = match index::load(&args.path)? {
        Some(index) => index,
        None => {
            eprintln!("No index found, building...");
            let index = index::build(&args.path);
            index::save(&index, &args.path)?;
            index
        }
    };
    let load_us = load_started.elapsed().as_micros();

    let eval_started = Instant::now();
    let query_pattern = if args.ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    let query = query::decompose(&query_pattern);
    let candidate_set = query::evaluate_masked(&query, |trigram| index.lookup_with_masks(trigram));
    let eval_us = eval_started.elapsed().as_micros();

    let (candidate_files, candidate_count) = match &candidate_set {
        CandidateSet::All => (index.resolve_files(None), index.files.len()),
        CandidateSet::Some(ids) => (index.resolve_files(Some(ids)), ids.len()),
    };

    let match_started = Instant::now();
    let results = matcher::match_files(&candidate_files, &regex);
    let match_us = match_started.elapsed().as_micros();

    print_results(&results);

    if args.time {
        let total_us = load_us + eval_us + match_us;
        eprintln!();
        eprintln!("--- timing (pattern: {pattern:?}) ---");
        eprintln!("  index load:       {}", fmt_us(load_us));
        eprintln!(
            "  trigram eval:     {}  ({}/{} files candidates)",
            fmt_us(eval_us),
            candidate_count,
            index.files.len()
        );
        eprintln!(
            "  regex verify:     {}  ({} matches)",
            fmt_us(match_us),
            results.len()
        );
        eprintln!("  total:            {}", fmt_us(total_us));
    }

    Ok(0)
}

fn print_results(results: &[matcher::Match]) {
    let output = matcher::format_results(results);
    if !output.is_empty() {
        println!("{output}");
    }
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

#[allow(dead_code)]
fn _assert_path(_: &Path) {}
