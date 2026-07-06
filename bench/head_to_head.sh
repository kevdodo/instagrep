#!/usr/bin/env bash
# Head-to-head benchmark: instagrep (ours) vs ripgrep vs the Elixir instantgrep.
#
# Same corpus, same patterns, same machine. Measures build time, index size,
# cold single-shot latency, warm (daemon) latency, and result-parity vs ripgrep.
# The Elixir tool is auto-detected (`ig` / `instantgrep` on PATH); if absent its
# columns are skipped, so this is always useful for ours-vs-ripgrep too.
#
# Usage:
#   ./bench/head_to_head.sh                 # generate a synthetic 20k corpus
#   ./bench/head_to_head.sh /path/to/repo   # use a real repo as the corpus
set -uo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$DIR/.." && pwd)"
IG="$REPO_ROOT/target/release/instagrep"
RG=(rg --no-heading)
ELIXIR_BIN="$(command -v ig || command -v instantgrep || true)"
HYPERFINE="$(command -v hyperfine || true)"
CORPUS="${1:-$(mktemp -d)/corpus}"
RUNS="${RUNS:-15}"

if [ ! -x "$IG" ]; then echo "Build first: cargo build --release"; exit 1; fi

# ---------------------------------------------------------------------------
# 1. Corpus (generate a high-diversity synthetic tree if none given)
# ---------------------------------------------------------------------------
if [ ! -d "$CORPUS" ] || [ -z "$(ls -A "$CORPUS" 2>/dev/null)" ]; then
  echo "Generating synthetic 20k-file corpus at $CORPUS ..."
  mkdir -p "$CORPUS"
  python3 - "$CORPUS" 20000 <<'PY'
import os,sys,random,string
out=sys.argv[1]; random.seed(11)
dirs=["","src","src/auth","src/api","src/db","logs","tests","vendor","bench"]
for d in set(dirs): os.makedirs(os.path.join(out,d),exist_ok=True)
words=["fn","let","mut","impl","struct","enum","return","async","await","match",
"trait","pub","use","mod","crate","vec","map","filter","collect","unwrap","Result",
"Option","Vec","HashMap","error","warn","info","debug","trace","request","response",
"client","server","token","session","cache","retry","timeout","queue","worker",
"partition","offset","commit","batch","schema","migration","index"]
for i in range(int(sys.argv[2])):
    lines=[f"// file {i} module {random.choice(words)}"]
    for _ in range(random.randint(40,120)):
        toks=[random.choice(words) for _ in range(random.randint(4,10))]
        toks.append("".join(random.choice(string.ascii_letters+string.digits) for _ in range(random.randint(6,16))))
        lines.append(" ".join(toks))
    if i%2000==0: lines.append("// NEEDLE_RARE_"+str(i))
    with open(os.path.join(out,random.choice(dirs),f"f{i:06d}.rs"),"w") as f:
        f.write("\n".join(lines)+"\n")
PY
fi
echo "Corpus: $CORPUS  ($(du -sh "$CORPUS" | awk '{print $1}'))"

# best-of-N wall-clock (seconds), used when hyperfine is unavailable
bestof() {
  local best=""
  for _ in $(seq 1 "$RUNS"); do
    local t; t=$(/usr/bin/time -p "$@" 2>&1 >/dev/null | awk '/^real/{print $2}')
    [ -n "$best" ] && awk -v a="$t" -v b="$best" 'BEGIN{exit !(a<b)}' || t=$best
    best=$t
  done
  echo "${best}"
}
latency() { # label, cmd...
  local label="$1"; shift
  if [ -n "$HYPERFINE" ]; then
    # hyperfine prints "Time (mean ± σ):   12.3 ms"
    hyperfine --runs "$RUNS" --warmup 3 --shell=none "$*" 2>/dev/null \
      | awk -v L="$label" '/Time \(mean/{printf "  %-28s %s\n", L, substr($0,index($0,":")+2)}'
  else
    printf "  %-28s %ss\n" "$label" "$(bestof "$@")"
  fi
}
# matched-file set (sorted, content/format-agnostic) for parity checks
file_set() { "$@" 2>/dev/null | sed 's#:.*##' | sort -u | md5; }

# ---------------------------------------------------------------------------
# 2. Build + index size
# ---------------------------------------------------------------------------
echo ""
echo "=== Build ==="
rm -rf "$CORPUS"/.instantgrep
latency "ours: --build" "$IG" --build "$CORPUS" >/dev/null 2>&1 || true
$IG --stats "$CORPUS" | awk '/Index on disk|Files indexed|Unique trigrams/{printf "    %s\n",$0}'
if [ -n "$ELIXIR_BIN" ]; then
  ( cd "$(dirname "$ELIXIR_BIN")/.." 2>/dev/null; true )
  echo "  elixir bin: $ELIXIR_BIN (build via its own docs if not already indexed)"
fi

# ---------------------------------------------------------------------------
# 3. Cold single-shot latency + parity vs ripgrep
# ---------------------------------------------------------------------------
PATTERNS=("NEEDLE_RARE_0" "fn match" "struct.*{]" "token|session" "NEEDLE")
echo ""
echo "=== Cold latency (best/mean of $RUNS) ==="
for p in "${PATTERNS[@]}"; do
  echo "  pattern: $p"
  latency "  ours"   "$IG" "$p" "$CORPUS"
  latency "  rg"     "${RG[@]}" "$p" "$CORPUS"
  [ -n "$ELIXIR_BIN" ] && latency "  elixir" "$ELIXIR_BIN" "$p" "$CORPUS"
  # parity: matched-file set must equal ripgrep's
  rg_set=$(file_set "${RG[@]}" -l "$p" "$CORPUS")
  ours_set=$(file_set "$IG" -l "$p" "$CORPUS")
  if [ "$rg_set" = "$ours_set" ]; then echo "    parity ours==rg: YES"; else echo "    parity ours==rg: NO  ($ours_set vs $rg_set)"; fi
done

# ---------------------------------------------------------------------------
# 4. Warm (daemon) latency — ours only; elixir daemon if it has one
# ---------------------------------------------------------------------------
echo ""
echo "=== Warm latency (daemon, index resident) ==="
SOCK="$CORPUS/.instantgrep/daemon.sock"
nohup "$IG" --daemon "$CORPUS" >/tmp/igd_h2h.log 2>&1 &
DPID=$!
for _ in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.1; done
if [ -S "$SOCK" ]; then
  for p in "NEEDLE_RARE_0" "fn match"; do
    latency "  ours(daemon) [$p]" "$IG" "$p" "$CORPUS"
  done
else
  echo "  daemon did not start"
fi
kill "$DPID" 2>/dev/null; wait "$DPID" 2>/dev/null

echo ""
echo "=== done ==="
[ "${1:-}" = "" ] && echo "(corpus left at $CORPUS for inspection; remove with rm -rf)"
