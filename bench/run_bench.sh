#!/usr/bin/env bash
DIR="/home/kdo/instagrep/bench"
DATA="$DIR/data.log"
IG="/home/kdo/instagrep/target/release/instagrep"
RG="rg --no-heading"
RUNS=3

bench() {
    local label="$1"
    shift
    local best=""
    echo -n "  $label ... "

    for i in $(seq 1 $RUNS); do
        sync
        local t
        t=$(/usr/bin/time -f '%e' "$@" 2>&1 >/dev/null) || true
        if [ -z "$best" ] || [ "$(echo "$t < $best" | bc 2>/dev/null)" = "1" ]; then
            best="$t"
        fi
    done
    echo "${best}s"
}

echo "========================================"
echo " instagrep vs ripgrep — 1.1 GB log file"
echo "========================================"
echo ""

echo "── Rare literal ──"
bench "instagrep (indexed)" $IG CRITICAL_BUG_123 $DATA
bench "ripgrep"            $RG CRITICAL_BUG_123 $DATA

echo "── Moderately common literal ──"
bench "instagrep (indexed)" $IG security_audit $DATA
bench "ripgrep"            $RG security_audit $DATA

echo "── Common literal ──"
bench "instagrep (indexed)" $IG "Health check passed" $DATA
bench "ripgrep"            $RG "Health check passed" $DATA

echo "── Alternation ──"
bench "instagrep (indexed)" $IG "login successful|failed attempt" $DATA
bench "ripgrep"            $RG "login successful|failed attempt" $DATA

echo "── Case-insensitive ──"
bench "instagrep (indexed)" $IG -i CRITICAL_BUG_123 $DATA
bench "ripgrep"            $RG -i CRITICAL_BUG_123 $DATA

echo "── Regex char class ──"
bench "instagrep (indexed)" $IG "[A-Z]{4,5}" $DATA
bench "ripgrep"            $RG "[A-Z]{4,5}" $DATA

echo "── Email regex ──"
bench "instagrep (indexed)" $IG '[a-z]+@[a-z]+\.[a-z]+' $DATA
bench "ripgrep"            $RG '[a-z]+@[a-z]+\.[a-z]+' $DATA

echo "── Very common word ──"
bench "instagrep (indexed)" $IG from $DATA
bench "ripgrep"            $RG from $DATA

echo ""
echo "── Brute-force (no index) ──"
bench "instagrep (no-index)" $IG --no-index CRITICAL_BUG_123 $DATA
bench "instagrep (no-index)" $IG --no-index from $DATA

echo ""
echo "───────  done  ───────"
