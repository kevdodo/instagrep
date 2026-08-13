# instagrep

Fast grep-style search over a directory tree, accelerated by a persisted
**roaring-bitmap trigram inverted index**.

```
cargo build --release
./target/release/instagrep --build .
./target/release/instagrep "pattern" .
```

instagrep indexes every text file under a root by the set of 3-byte trigrams it
contains. At search time it derives the trigrams a regex *must* require, uses
the index to prune the file set down to a handful of candidates, then verifies
only those candidates with a byte regex.

## Install

```bash
# one line (prebuilt binary, falls back to cargo install)
curl -fsSL https://raw.githubusercontent.com/kvyaswanth/instagrep/main/install.sh | sh

# or build from source with cargo
cargo install --git https://github.com/kvyaswanth/instagrep --locked

# or Homebrew (from HEAD until the first versioned release)
brew tap kvyaswanth/instagrep
brew install --HEAD instagrep
```

Prebuilt binaries are published for macOS (arm64/x86_64) and Linux
(x86_64/aarch64) on each `v*` tag.

## Usage

```
instagrep [OPTIONS] PATTERN [PATH]

Index management:
    --build             Build or rebuild the index, then exit
    --update            Incrementally refresh the index (added/changed/removed
                        files patched in surgically, no full rebuild)
    --stats             Show index statistics, then exit
    --no-index          Skip the index and brute-force scan every file

Search:
    -i, --ignore-case          Case-insensitive matching
    -l, --files-with-matches   Print only the paths of files with a match
    -c, --count                Print only a per-file match count
    -A, --after-context <N>    Lines of context after each match
    -B, --before-context <N>   Lines of context before each match
    -C, --context <N>          Lines of context around each match
    --color <auto|always|never> Colour matches (default: auto)
    -g, --glob <GLOB>          Include/exclude files (`!` prefix excludes)

Traversal:
    --hidden            Search hidden files and directories
    --no-ignore         Do not respect .gitignore / .ignore files

Diagnostics:
    --time              Print per-phase timing to stderr
    -h, --help          Show help
    -V, --version       Show version

Examples:
    instagrep --build .
    instagrep "pattern" .
    instagrep -i "todo|fixme" src/
    instagrep --update .
    instagrep --stats .
    instagrep -l --glob '!*.log' "TODO" .
    instagrep -C 3 "panic!" .
    instagrep --no-index "pattern" .
```

If no index exists when you search, instagrep builds one automatically.

## Agents (MCP)

`instagrep` is an MCP server, so coding agents — Claude Code, Cursor, anything
that speaks MCP — can use it as a native `search` tool. No per-query tree
rescan; the agent gets structured JSON back instead of scraping terminal output.

```
# run the stdio server
instagrep mcp

# wire it into Claude Code (one time)
claude mcp add instagrep -- instagrep mcp
```

The `search` tool accepts `pattern` (regex), `path`, `ignore_case`, `before`,
`after`, `glob`, `hidden`, `no_ignore`, `files_with_matches`, `count`, and
`limit`, and returns JSON per match: `{ path, line, text, spans }`.

The server runs **locally** as a subprocess and stays alive for the session, so
the index loads once and stays warm — the fastest way to use instagrep.

## How it works

1. **Walk** the tree (via the `ignore` crate, ripgrep's traversal engine) —
   respecting nested `.gitignore` / `.ignore`, skipping binary extensions,
   hidden files, and oversized files.
2. **Index**: each file's overlapping 3-byte trigrams (and their ASCII-lowercased
   counterparts, so a single index serves case-insensitive search) are stored in
   an inverted index mapping `trigram → RoaringBitmap` of file ids.
3. **Query**: the regex is parsed with `regex-syntax` and analyzed using Russ
   Cox's trigram-query algorithm (the one behind Google Code Search) to derive a
   *sound* boolean trigram query — it never produces false negatives.
4. **Prune**: the query is evaluated with roaring bitwise AND/OR, narrowing
   thousands of files to a few candidates in microseconds.
5. **Verify**: candidates are matched in parallel with a byte regex
   (`regex::bytes`), so invalid UTF-8 is handled correctly.

## Index format

A compact, versioned binary blob under `.instantgrep/index.bin`. Roaring bitmaps
keep the index small — roughly **a third the size of a naïve posting-list
index** and typically well under the size of the source tree. Paths are stored
relative to the root, so an index is portable.

## Acknowledgements

The trigram-index approach and the regex→trigram query analysis follow Russ
Cox's *Regular Expression Matching with a Trigram Index* (swtch.com/~rsc/regexp).
Directory traversal uses BurntSushi's `ignore` crate (ripgrep's engine).
