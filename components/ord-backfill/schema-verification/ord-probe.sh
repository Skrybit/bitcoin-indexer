#!/usr/bin/env bash
# ord-probe.sh — fetch a representative slice of ord HTTP API responses
# for one block and dump them to schema-verification/samples/<height>/.
#
# Used while building the mapping between ord's response shapes and our
# postgres schema. Takes a block height, fires a curated set of GETs,
# and saves each as JSON (or text where the endpoint returns text/plain).
# Re-running on the same height overwrites — fine, we want fresh dumps
# when ord's API surface changes.
#
# Configuration via env:
#   ORD_URL        full URL of ord HTTP API (default http://10.20.20.61:8080)
#   ORD_CURL_OPTS  extra curl flags (e.g. "-k --max-time 30")
#
# Usage:
#   ./ord-probe.sh <block_height> [output_dir]
#
# Example:
#   ORD_URL=http://10.20.20.61:8080 ./ord-probe.sh 825000

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <block_height> [output_dir]" >&2
    exit 2
fi

HEIGHT="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${2:-$SCRIPT_DIR/samples/$HEIGHT}"
ORD_URL="${ORD_URL:-http://10.20.20.61:8080}"
ORD_CURL_OPTS="${ORD_CURL_OPTS:-}"

mkdir -p "$OUT_DIR"

# Wrapper: every call goes through this so options + accept header are
# uniform and we can swap in -v / --max-time across the board easily.
ord_get() {
    local path="$1"
    local out="$2"
    # shellcheck disable=SC2086
    curl -sS $ORD_CURL_OPTS \
        -H 'Accept: application/json' \
        -o "$out" \
        -w 'HTTP %{http_code} %{size_download}B %{time_total}s %{url}\n' \
        "$ORD_URL$path" \
        || { echo "FAIL  $path"; return 1; }
}

# Helper: pretty-print a JSON file in place if jq is available.
prettify() {
    local f="$1"
    if command -v jq >/dev/null 2>&1 && jq empty "$f" >/dev/null 2>&1; then
        local tmp; tmp="$(mktemp)"
        jq . "$f" > "$tmp" && mv "$tmp" "$f"
    fi
}

echo "ord       = $ORD_URL"
echo "height    = $HEIGHT"
echo "out_dir   = $OUT_DIR"
echo

# 1. Chain-level
ord_get "/r/blockheight"            "$OUT_DIR/blockheight.txt"
ord_get "/r/blockhash"              "$OUT_DIR/blockhash-tip.txt"
ord_get "/r/blockhash/$HEIGHT"      "$OUT_DIR/blockhash-$HEIGHT.txt"

# 2. Inscriptions in this block (paginated; capture page 0 and 1).
# ord 0.27 exposes this on the human path with Accept: json — the
# /r/recursive prefix doesn't carry it. Returns {ids[], more, page_index}.
ord_get "/inscriptions/block/$HEIGHT/0" "$OUT_DIR/inscriptions-block-page0.json"
prettify "$OUT_DIR/inscriptions-block-page0.json"
ord_get "/inscriptions/block/$HEIGHT/1" "$OUT_DIR/inscriptions-block-page1.json" || true
prettify "$OUT_DIR/inscriptions-block-page1.json"

# 2b. Block metadata (fees, tx count, hashes, timestamps).
ord_get "/r/blockinfo/$HEIGHT" "$OUT_DIR/blockinfo.json"
prettify "$OUT_DIR/blockinfo.json"

# 3. Per-inscription metadata for the first few inscriptions of the block.
# Response shape: {id, number, sat, output, satpoint, address, content_type,
# content_length, fee, height, value, timestamp, charms, delegate}.
if command -v jq >/dev/null 2>&1; then
    ids="$(jq -r '.ids[]?' "$OUT_DIR/inscriptions-block-page0.json" 2>/dev/null | head -5 || true)"
    if [[ -n "$ids" ]]; then
        i=0
        while IFS= read -r id; do
            [[ -z "$id" ]] && continue
            ord_get "/r/inscription/$id"  "$OUT_DIR/inscription-$i.json" || true
            prettify "$OUT_DIR/inscription-$i.json"
            i=$((i + 1))
        done <<< "$ids"
    fi
else
    echo "(jq not present — skipping per-inscription dumps)" >&2
fi

# 4. Sat info — pick a satoshi number derived from height for variety
SAT_FOR_HEIGHT="$(( HEIGHT * 1000000 ))"
ord_get "/r/sat/$SAT_FOR_HEIGHT" "$OUT_DIR/sat-sample.json" || true
prettify "$OUT_DIR/sat-sample.json"

# 5. Runes — top page
ord_get "/r/runes/0"  "$OUT_DIR/runes-page0.json" || true
prettify "$OUT_DIR/runes-page0.json"

echo
echo "done. samples in $OUT_DIR"
