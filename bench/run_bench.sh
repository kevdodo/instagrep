#!/usr/bin/env bash
# instagrep vs ripgrep on a synthetic multi-file source tree.
#
# Usage: ./bench/run_bench.sh [NFILES]            (default 4000 files, ~35 MB)
#
# Generates a corpus, builds the instagrep index, and compares indexed search,
# brute-force search, and incremental update against ripgrep.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$DIR/.." && pwd)"
IG="$REPO_ROOT/target/release/instagrep"
RG=(rg --no-heading)
NFILES="${1:-4000}"
CORPUS="$(mktemp -d)/corpus"
RUNS=3

# ---------------------------------------------------------------------------
# Generate a deterministic source-like corpus (code + log files) with a few
# rare "needle" strings so rare-literal searches are meaningful.
# ---------------------------------------------------------------------------
generate_corpus() {
  python3 - "$CORPUS" "$NFILES" <<'PY'
import os, sys, random
out, n = sys.argv[1], int(sys.argv[2])
random.seed(1337)
levels = ["INFO","DEBUG","WARN","ERROR","TRACE"]
mods = ["auth","db","api","cache","queue","scheduler","web","rpc","worker","health"]
exts = [".rs",".py",".ts",".go",".log"]
needles = ["CRITICAL_BUG_7782","security_audit_vault_99","defragmentation_consolidator"]
dirs = [out]
for d in ["src","src/auth","src/api","logs","tests"]:
    p = os.path.join(out, d); os.makedirs(p, exist_ok=True); dirs.append(p)
boiler = "pub fn handle(req: Request) -> Response {{ /* module {mod} */ todo!() }}\n"
for i in range(n):
    d = random.choice(dirs); ext = random.choice(exts); mod = random.choice(mods)
    lines = [boiler.format(mod=mod)]
    for _ in range(random.randint(40, 200)):
        lines.append(f"{random.choice(levels)} [{mod}] id={random.randint(1,10**6)} latency={random.randint(1,200)}ms")
    if random.random() < 0.004:
        lines.append(f"// {random.choice(needles)} at {i} in {mod}")
    with open(os.path.join(d, f"f{i:06d}{ext}"), "w") as f:
        f.write("\n".join(lines) + "\n")
print(f"  generated {n} files under {out}", file=sys.stderr)
PY
}

bench() {
    local label="$1"; shift
    local best=""
    for i in $(seq 1 "$RUNS"); do
        # /usr/bin/time -p is portable (BSD + GNU); it prints "real <secs>".
        local t
        t=$(/usr/bin/time -p "$@" 2>&1 >/dev/null | awk '/^real/{print $2}') || true
        if [ -z "$best" ] || [ "$(awk -v a="$t" -v b="$best" 'BEGIN{print (a<b)?1:0}')" = "1" ]; then
            best="$t"
        fi
    done
    printf "  %-34s %ss\n" "$label" "$best"
}

echo "=========================================="
echo " instagrep vs ripgrep — $NFILES-file corpus"
echo "=========================================="
generate_corpus

echo ""
echo "── Build ──"
rm -rf "$CORPUS/.instantgrep"
bench "instagrep --build" $IG --build --no-ignore "$CORPUS"
$IG --stats "$CORPUS" | sed 's/^/  /'

PATTERNS=(
    "CRITICAL_BUG_7782|Rare literal"
    "security_audit_vault_99|Uncommon literal"
    "function|Common literal"
    "INFO|Very common"
    "fn|Short (full-scan fallback)"
    "[A-Z]{4,5}|Regex char class"
)
for entry in "${PATTERNS[@]}"; do
    pat="${entry%%|*}"; label="${entry##*|}"
    echo ""
    echo "── $label  ($pat) ──"
    bench "instagrep (indexed)" $IG --no-ignore "$pat" "$CORPUS"
    bench "ripgrep"             "${RG[@]}" --no-ignore "$pat" "$CORPUS"
done

echo ""
echo "── Brute force (--no-index, parallel) ──"
bench "instagrep --no-index rare"   $IG --no-index CRITICAL_BUG_7782 "$CORPUS"
bench "instagrep --no-index common" $IG --no-index INFO "$CORPUS"

echo ""
echo "── Incremental update ──"
for n in $(seq 1 25); do echo "added_$n incremental_probe" > "$CORPUS/new_$n.txt"; done
bench "instagrep --update (+25)" $IG --update "$CORPUS"
rm -f "$CORPUS"/new_*.txt

echo ""
echo "───────  done  ───────"
rm -rf "$CORPUS"
