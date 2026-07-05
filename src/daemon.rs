//! Optional persistent daemon.
//!
//! `instagrep --daemon <root>` loads (or builds) the index once, keeps it
//! resident in memory, and serves queries over a Unix domain socket at
//! `<root>/.instantgrep/daemon.sock`. The normal CLI transparently uses a
//! running daemon for that root when one exists, and otherwise falls back to
//! the one-shot path — so the only difference a daemon makes is skipping the
//! per-call index load, which is exactly the win for repeated queries (e.g. an
//! agent harness calling `grep_search` in a loop).
//!
//! Unix-only.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::index;
use crate::matcher::{ColorWhen, SearchMode, SearchOptions};
use crate::scanner::WalkOptions;
use crate::search::run_indexed_search;

const MODE_NORMAL: u8 = 0;
const MODE_COUNT: u8 = 1;
const MODE_FILES: u8 = 2;

/// A query from the CLI client to the daemon.
pub struct Request {
    pub pattern: String,
    pub ignore_case: bool,
    pub mode: u8,
    pub before: u32,
    pub after: u32,
    pub color_always: bool,
}

impl Request {
    pub fn new(pattern: &str, ignore_case: bool, opts: &SearchOptions) -> Request {
        let mode = match opts.mode {
            SearchMode::Normal => MODE_NORMAL,
            SearchMode::Count => MODE_COUNT,
            SearchMode::FilesWithMatches => MODE_FILES,
        };
        Request {
            pattern: pattern.to_string(),
            ignore_case,
            mode,
            before: opts.before as u32,
            after: opts.after as u32,
            color_always: opts.color == ColorWhen::Always,
        }
    }

    fn to_opts(&self) -> SearchOptions {
        let mode = match self.mode {
            MODE_COUNT => SearchMode::Count,
            MODE_FILES => SearchMode::FilesWithMatches,
            _ => SearchMode::Normal,
        };
        SearchOptions {
            mode,
            before: self.before as usize,
            after: self.after as usize,
            color: if self.color_always { ColorWhen::Always } else { ColorWhen::Never },
        }
    }

    fn encode(&self) -> Vec<u8> {
        let pat = self.pattern.as_bytes();
        let mut buf = Vec::with_capacity(16 + pat.len());
        buf.extend_from_slice(&(pat.len() as u32).to_le_bytes());
        buf.extend_from_slice(pat);
        buf.push(u8::from(self.ignore_case));
        buf.push(self.mode);
        buf.extend_from_slice(&self.before.to_le_bytes());
        buf.extend_from_slice(&self.after.to_le_bytes());
        buf.push(u8::from(self.color_always));
        buf
    }
}

fn read_u32_le(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn decode_request(r: &mut impl Read) -> io::Result<Request> {
    let plen = read_u32_le(r)? as usize;
    let mut pat = vec![0u8; plen];
    r.read_exact(&mut pat)?;
    let mut byte = [0u8; 1];
    r.read_exact(&mut byte)?;
    let ignore_case = byte[0] != 0;
    r.read_exact(&mut byte)?;
    let mode = byte[0];
    let before = read_u32_le(r)?;
    let after = read_u32_le(r)?;
    r.read_exact(&mut byte)?;
    let color_always = byte[0] != 0;
    Ok(Request {
        pattern: String::from_utf8_lossy(&pat).into_owned(),
        ignore_case,
        mode,
        before,
        after,
        color_always,
    })
}

/// Socket path for `root`: `<canonical-root>/.instantgrep/daemon.sock`.
fn socket_path(root: &Path) -> Option<PathBuf> {
    fs::canonicalize(root).ok().map(|c| c.join(".instantgrep").join("daemon.sock"))
}

/// Run the daemon: load/build the index once, then serve queries forever.
pub fn serve(root: &Path, walk_opts: &WalkOptions) -> io::Result<i32> {
    let index = match index::load(root)? {
        Some(i) => i,
        None => {
            eprintln!("No index found, building...");
            let i = index::build(root, walk_opts);
            index::save(&i, root)?;
            i
        }
    };
    let index = Arc::new(index);
    let live = index.files.len() - index.removed_ids.len() as usize;

    let sock = match socket_path(root) {
        Some(p) => p,
        None => return Err(io::Error::new(io::ErrorKind::NotFound, "cannot canonicalize root")),
    };
    if let Some(parent) = sock.parent() {
        fs::create_dir_all(parent)?;
    }
    let _ = fs::remove_file(&sock); // clear any stale socket from a killed daemon
    let listener = UnixListener::bind(&sock)?;
    eprintln!(
        "instagrep daemon listening on {} ({} files indexed; Ctrl-C to stop)",
        sock.display(),
        live
    );

    for stream in listener.incoming() {
        if let Ok(s) = stream {
            let index = Arc::clone(&index);
            let root = root.to_path_buf();
            thread::spawn(move || handle_client(s, index, root));
        }
    }
    Ok(0)
}

fn handle_client(mut stream: UnixStream, index: Arc<index::Index>, root: PathBuf) {
    let req = match decode_request(&mut stream) {
        Ok(r) => r,
        Err(_) => return,
    };
    let opts = req.to_opts();
    let outcome = match run_indexed_search(&index, &root, &req.pattern, req.ignore_case, &opts) {
        Ok(o) => o,
        Err(_) => return,
    };
    let mut resp = Vec::with_capacity(8 + outcome.output.len());
    resp.extend_from_slice(&outcome.exit.to_le_bytes());
    resp.extend_from_slice(&(outcome.output.len() as u32).to_le_bytes());
    resp.extend_from_slice(&outcome.output);
    let _ = stream.write_all(&resp);
}

/// If a daemon is serving `root`, send `req` and return `(exit, output)`;
/// otherwise return `None` so the caller falls back to the one-shot path.
pub fn try_query(root: &Path, req: &Request) -> io::Result<Option<(i32, Vec<u8>)>> {
    let Some(sock) = socket_path(root) else {
        return Ok(None);
    };
    let mut stream = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(_) => return Ok(None), // no daemon running for this root
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    stream.write_all(&req.encode())?;
    stream.flush()?;

    let mut b4 = [0u8; 4];
    stream.read_exact(&mut b4)?;
    let exit = i32::from_le_bytes(b4);
    let len = read_u32_le(&mut stream)? as usize;
    let mut out = vec![0u8; len];
    stream.read_exact(&mut out)?;
    Ok(Some((exit, out)))
}
