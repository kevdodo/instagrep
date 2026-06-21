# instagrep

Fast grep-style search using a persisted trigram inverted index.

## Usage

```bash
cargo build --release

./target/release/instagrep --build .
./target/release/instagrep "pattern" .
./target/release/instagrep -i "todo|fixme" src/
./target/release/instagrep --update .
./target/release/instagrep --stats .
./target/release/instagrep --no-index "pattern" .
```

On first indexed search, `instagrep` builds `.instantgrep/index.bin` automatically if no index exists.

## How It Works

1. Recursively scans indexable text files while skipping common generated, dependency, VCS, and binary paths.
2. Extracts overlapping 3-byte trigrams into an inverted index.
3. Decomposes regex patterns into trigrams that must be present.
4. Uses the index to pick candidate files.
5. Verifies candidates with Rust's `regex` engine and prints `path:line:content`.

The Rust implementation is inspired by cursor's implementation of instant grep.
