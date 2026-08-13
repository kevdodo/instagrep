//! MCP (Model Context Protocol) server over stdio.
//!
//! Exposes `instagrep` as a `search` tool to coding agents — Claude Code,
//! Cursor, or any MCP client — so they get fast, index-accelerated code search
//! without scanning the whole tree per query.
//!
//! Launch with:  instagrep mcp
//!
//! Wire into Claude Code:
//!   claude mcp add instagrep -- instagrep mcp

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::index;
use crate::matcher::{self, ColorWhen, FileResult, SearchMode, SearchOptions};
use crate::query;
use crate::scanner::{self, WalkOptions};

/// Default cap on results returned to an agent (protects its context window).
const DEFAULT_LIMIT: usize = 100;

/// Run the stdio JSON-RPC server until stdin closes.
pub fn serve() -> io::Result<i32> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut input = stdin.lock();
    let mut line = String::new();

    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue, // not JSON-RPC; ignore
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let result: Option<Value> = match method {
            "initialize" => Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "instagrep",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            })),
            "tools/list" => Some(json!({
                "tools": [{
                    "name": "search",
                    "description": "Fast gitignore-aware code search over a project, accelerated by a persisted roaring-bitmap trigram index. Returns matching lines (path, line number, text, match spans). Correct for regex and literal queries.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "pattern": { "type": "string", "description": "Regular expression to search for." },
                            "path": { "type": "string", "default": ".", "description": "Project path to search." },
                            "ignore_case": { "type": "boolean", "default": false },
                            "before": { "type": "integer", "default": 0, "description": "Lines of context before each match." },
                            "after": { "type": "integer", "default": 0, "description": "Lines of context after each match." },
                            "glob": { "type": "string", "description": "Include/exclude glob, e.g. '*.rs'." },
                            "hidden": { "type": "boolean", "default": false },
                            "no_ignore": { "type": "boolean", "default": false },
                            "files_with_matches": { "type": "boolean", "default": false, "description": "Return only file paths containing matches." },
                            "count": { "type": "boolean", "default": false, "description": "Return match counts per file." },
                            "limit": { "type": "integer", "default": 100, "description": "Max results to return." }
                        },
                        "required": ["pattern"]
                    }
                }]
            })),
            "tools/call" => match handle_call(req.get("params")) {
                Ok(text) => Some(json!({
                    "content": [ { "type": "text", "text": text } ],
                    "isError": false
                })),
                Err(e) => Some(json!({
                    "content": [ { "type": "text", "text": e } ],
                    "isError": true
                })),
            },
            // notifications and unknown methods: ignore
            _ => None,
        };

        if let Some(r) = result {
            let resp = json!({ "jsonrpc": "2.0", "id": id, "result": r });
            writeln!(out, "{resp}")?;
            out.flush()?;
        }
    }
    Ok(0)
}

fn handle_call(params: Option<&Value>) -> Result<String, String> {
    let params = params.ok_or_else(|| "missing params".to_string())?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'pattern'".to_string())?
        .to_string();
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or(".")
        .to_string();
    let ignore_case = args.get("ignore_case").and_then(|v| v.as_bool()).unwrap_or(false);
    let before = args.get("before").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let after = args.get("after").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let hidden = args.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false);
    let no_ignore = args.get("no_ignore").and_then(|v| v.as_bool()).unwrap_or(false);
    let files_with_matches = args.get("files_with_matches").and_then(|v| v.as_bool()).unwrap_or(false);
    let count = args.get("count").and_then(|v| v.as_bool()).unwrap_or(false);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_LIMIT);
    let mut globs = Vec::new();
    if let Some(g) = args.get("glob").and_then(|v| v.as_str()) {
        globs.push(g.to_string());
    }

    let root = PathBuf::from(&path);
    let walk_opts = WalkOptions {
        no_ignore,
        hidden,
        globs,
        max_file_size: scanner::DEFAULT_MAX_FILE_SIZE,
    };
    let mode = if files_with_matches {
        SearchMode::FilesWithMatches
    } else if count {
        SearchMode::Count
    } else {
        SearchMode::Normal
    };
    let opts = SearchOptions {
        mode,
        before,
        after,
        color: ColorWhen::Never,
    };

    let results = run_search(&pattern, &root, &walk_opts, &opts, ignore_case)
        .map_err(|e| e.to_string())?;

    Ok(render(results, &mode, limit))
}

/// Indexed search that returns raw results instead of printing them. Builds the
/// index on first use (mirrors the CLI's indexed path).
fn run_search(
    pattern: &str,
    root: &Path,
    walk_opts: &WalkOptions,
    opts: &SearchOptions,
    ignore_case: bool,
) -> io::Result<Vec<FileResult>> {
    let regex = matcher::compile_regex(pattern, ignore_case)?;

    let index = match index::load(root)? {
        Some(index) => index,
        None => {
            let index = index::build(root, walk_opts);
            index::save(&index, root)?;
            index
        }
    };

    let query_pattern = if ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    let query = query::analyze(&query_pattern);
    let candidate_set = query::evaluate(&query, |trigram| index.postings.get(&trigram).cloned());
    let candidate_files = match &candidate_set {
        query::CandidateSet::All => index.resolve_files(None),
        query::CandidateSet::Some(bitmap) => index.resolve_files(Some(bitmap)),
    };

    Ok(matcher::search_files(&candidate_files, root, &regex, opts))
}

/// Serialize results to a pretty JSON string, capped at `limit` items.
fn render(results: Vec<FileResult>, mode: &SearchMode, limit: usize) -> String {
    let mut items: Vec<Value> = Vec::new();
    for file in &results {
        let path = file.rel_path.display().to_string();
        match mode {
            SearchMode::FilesWithMatches => {
                items.push(json!({ "type": "file", "path": path }));
            }
            SearchMode::Count => {
                items.push(json!({ "type": "summary", "path": path, "count": file.match_count }));
            }
            SearchMode::Normal => {
                for entry in &file.entries {
                    let text = String::from_utf8_lossy(&entry.content).into_owned();
                    let spans: Vec<Value> = entry.spans.iter().map(|(s, e)| json!([s, e])).collect();
                    items.push(json!({
                        "type": "match",
                        "path": path,
                        "line": entry.line,
                        "text": text,
                        "spans": spans,
                        "match": entry.is_match,
                    }));
                }
            }
        }
    }

    let total = items.len();
    let truncated = total > limit;
    if total > limit {
        items.truncate(limit);
    }
    let body = json!({
        "total": total,
        "returned": items.len(),
        "truncated": truncated,
        "results": items,
    });
    serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string())
}
