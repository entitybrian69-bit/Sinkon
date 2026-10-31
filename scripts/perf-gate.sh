#!/usr/bin/env bash
# =============================================================================
#  sinkon perf gate  —  fixed contract, do not change the CLI or exit codes
# =============================================================================
#  Compares criterion results against a per-backend baseline and fails on a
#  regression larger than THRESHOLD percent.
#
#  Bench ids must be prefixed with the backend (`vulkan_`, `gles3_`, `gles2_`);
#  the baseline schema is pinned:
#      { "<backend>_<bench_id>": { "mean_ns": <u64> }, ... }
#
#  Exit codes
#      0  no FAIL rows (PASS/WARN/NEW/REMOVED/SKIP only)
#      1  at least one FAIL row, or --strict without a baseline
#      2  tooling error (missing jq/cargo, bench run failed)
# =============================================================================
set -eu

LOG="[sinkon-perf-gate]"

usage() {
    cat <<'USAGE'
[sinkon-perf-gate] Sinkon criterion regression gate

Usage:
  scripts/perf-gate.sh [options]

Options:
  --accept-baseline        Record CURRENT as the new baseline (never implicit).
                           Uses CURRENT if it exists, otherwise runs
                           `cargo bench --bench sinkon_benches -- --save-baseline main`
                           followed by `cargo criterion --message-format=json`.
  --strict                 Fail (exit 1) instead of soft-passing when no
                           baseline exists.
  --backends=LIST          Comma-separated backends to compare
                           (default: vulkan,gles3,gles2). Rows for other
                           backends are reported as SKIP.
  --threshold=PCT          Regression budget in percent (default: 5).
  --baseline=PATH          Baseline JSON (default: $BASELINE or benches/baseline.json).
  --current=PATH           Current results JSON (default: $CURRENT or benches/current.json).
  --report=PATH            Markdown report output (default: $REPORT or benches/perf-gate-report.md).
  --help, -h               Show this help and exit 0.

Environment variables (options override them):
  BASELINE   default benches/baseline.json
  CURRENT    default benches/current.json
  THRESHOLD  default 5 (percent)
  REPORT     default benches/perf-gate-report.md
  BACKENDS   default vulkan,gles3,gles2

Examples:
  scripts/perf-gate.sh
  scripts/perf-gate.sh --strict --backends=vulkan,gles3
  scripts/perf-gate.sh --accept-baseline --backends=vulkan,gles3,gles2
USAGE
}

BASELINE="${BASELINE:-benches/baseline.json}"
CURRENT="${CURRENT:-benches/current.json}"
THRESHOLD="${THRESHOLD:-5}"
REPORT="${REPORT:-benches/perf-gate-report.md}"
BACKENDS="${BACKENDS:-vulkan,gles3,gles2}"

STRICT=0
ACCEPT_BASELINE=0

while [ $# -gt 0 ]; do
    case "$1" in
        --help|-h)
            usage
            exit 0
            ;;
        --strict)
            STRICT=1
            ;;
        --accept-baseline)
            ACCEPT_BASELINE=1
            ;;
        --backends=*)
            BACKENDS="${1#--backends=}"
            ;;
        --threshold=*)
            THRESHOLD="${1#--threshold=}"
            ;;
        --baseline=*)
            BASELINE="${1#--baseline=}"
            ;;
        --current=*)
            CURRENT="${1#--current=}"
            ;;
        --report=*)
            REPORT="${1#--report=}"
            ;;
        *)
            printf '%s Unknown option: %s\n' "$LOG" "$1" >&2
            printf '%s Run --help for the supported options.\n' "$LOG" >&2
            exit 2
            ;;
    esac
    shift
done

# ---------------------------------------------------------------- tooling ---
missing=""
command -v jq >/dev/null 2>&1 || missing="$missing jq"
command -v cargo >/dev/null 2>&1 || missing="$missing cargo"
if [ -n "$missing" ]; then
    printf '%s error: required tool(s) not found:%s\n' "$LOG" "$missing" >&2
    printf '%s   jq     https://jqlang.github.io/jq/download/\n' "$LOG" >&2
    printf '%s   cargo  https://rustup.rs\n' "$LOG" >&2
    exit 2
fi

case "$THRESHOLD" in
    ''|*[!0-9.]*)
        printf '%s error: --threshold must be a number (got "%s")\n' "$LOG" "$THRESHOLD" >&2
        exit 2
        ;;
esac

report_dir=$(dirname "$REPORT")
[ -d "$report_dir" ] || mkdir -p "$report_dir"
current_dir=$(dirname "$CURRENT")
[ -d "$current_dir" ] || mkdir -p "$current_dir"
baseline_dir=$(dirname "$BASELINE")
[ -d "$baseline_dir" ] || mkdir -p "$baseline_dir"

# ------------------------------------------------------- normalise results ---
# Accepts both the pinned sinkon schema and `cargo-criterion` NDJSON:
#   { "vulkan_mesh_upload": { "mean_ns": 1234 } }
#   {"type":"benchmark-complete","id":"vulkan_mesh_upload","mean":{"estimate":1234.0}}
NORMALISE='
def mean_of:
    if type == "number" then .
    elif type == "object" then (.mean_ns // (.mean.estimate) // .mean // .estimate // 0)
    else 0 end;

def entries_of:
    if type == "array" then
        (if (map(select(type == "object" and has("id"))) | length) > 0
         then .[] | select(type == "object" and has("id")) | {id: .id, mean_ns: (.mean | mean_of)}
         else .[] | entries_of end)
    elif type == "object" then
        to_entries[] | select(.value != null) | {id: .key, mean_ns: (.value | mean_of)}
    else empty end;

[entries_of] | map(select(.id != null and .id != "")) | map({key: .id, value: .mean_ns}) | from_entries
'

normalise() {
    jq -rs "$NORMALISE" "$1"
}

run_criterion() {
    printf '%s running: cargo criterion --message-format=json > %s\n' "$LOG" "$CURRENT"
    if ! cargo criterion --message-format=json > "$CURRENT"; then
        printf '%s error: `cargo criterion` failed\n' "$LOG" >&2
        exit 2
    fi
    if [ ! -s "$CURRENT" ]; then
        printf '%s error: criterion produced no JSON (is cargo-criterion installed?)\n' "$LOG" >&2
        exit 2
    fi
}

# ------------------------------------------------------------ accept mode ---
if [ "$ACCEPT_BASELINE" -eq 1 ]; then
    if [ ! -s "$CURRENT" ]; then
        printf '%s no CURRENT at %s — benchmarking first\n' "$LOG" "$CURRENT"
        if ! cargo bench --bench sinkon_benches -- --save-baseline main; then
            printf '%s error: `cargo bench` failed\n' "$LOG" >&2
            exit 2
        fi
        run_criterion
    else
        printf '%s using existing CURRENT at %s\n' "$LOG" "$CURRENT"
    fi
    normalise "$CURRENT" \
        | jq 'with_entries(.value |= { mean_ns: (. * 1 | round | tostring | tonumber) })
              | to_entries | sort_by(.key) | from_entries' > "$BASELINE"
    count=$(jq 'length' "$BASELINE")
    printf '%s baseline recorded: %s (%s benchmark(s))\n' "$LOG" "$BASELINE" "$count"
    printf '%s accepted baseline for backends: %s\n' "$LOG" "$BACKENDS"
    exit 0
fi

# -------------------------------------------------------- baseline check ---
if [ ! -f "$BASELINE" ]; then
    printf '%s No baseline found at %s — run with --accept-baseline to create one.\n' \
        "$LOG" "$BASELINE"
    if [ "$STRICT" -eq 1 ]; then
        printf '%s --strict given, so this is a failure (exit 1)\n' "$LOG" >&2
        exit 1
    fi
    exit 0
fi

run_criterion

BASE_JSON=$(mktemp)
CUR_JSON=$(mktemp)
ROWS=$(mktemp)
cleanup() {
    rm -f "$BASE_JSON" "$CUR_JSON" "$ROWS"
}
trap cleanup EXIT INT TERM

normalise "$BASELINE" > "$BASE_JSON"
normalise "$CURRENT" > "$CUR_JSON"

# ----------------------------------------------------------- compare rows ---
jq -rn \
    --slurpfile base "$BASE_JSON" \
    --slurpfile current "$CUR_JSON" \
    --arg backends "$BACKENDS" \
    --argjson threshold "$THRESHOLD" '
    ($base[0] // {}) as $b
    | ($current[0] // {}) as $c
    | ($backends | split(",") | map(gsub("^\\s+|\\s+$"; ""))) as $allowed
    | (($b | keys_unsorted) + ($c | keys_unsorted) | unique) as $ids
    | $ids[]
    | . as $id
    | ($id | split("_")[0]) as $backend
    | (if ($allowed | index($backend)) then "y" else "n" end) as $in_scope
    | ($b[$id] // null) as $base_ns
    | ($c[$id] // null) as $cur_ns
    | (if ($base_ns != null and $base_ns != 0 and $cur_ns != null)
       then ((($cur_ns - $base_ns) / $base_ns) * 100)
       else null end) as $pct
    | (if $in_scope == "n" then "SKIP"
       elif $base_ns == null then "NEW"
       elif $cur_ns == null then "REMOVED"
       elif $pct == null then "PASS"
       elif $pct > $threshold then "FAIL"
       elif $pct > 0 then "WARN"
       else "PASS" end) as $status
    | [ $backend,
        $id,
        (if $base_ns == null then "-" else (($base_ns | round) | tostring) end),
        (if $cur_ns == null then "-" else (($cur_ns * 100 | round) / 100 | tostring) end),
        (if $pct == null then "-" else ($pct * 100 | round / 100 | tostring) end),
        $status ]
    | @tsv
' > "$ROWS"

fails=$(awk -F'\t' '$6 == "FAIL"' "$ROWS" | wc -l | tr -d ' ')
warns=$(awk -F'\t' '$6 == "WARN"' "$ROWS" | wc -l | tr -d ' ')
passes=$(awk -F'\t' '$6 == "PASS"' "$ROWS" | wc -l | tr -d ' ')
rows=$(awk 'END { print NR }' "$ROWS")

# ------------------------------------------------------------- render out ---
{
    printf '# Sinkon perf gate\n\n'
    printf 'baseline: `%s` · current: `%s` · threshold: %s%% · backends: %s\n\n' \
        "$BASELINE" "$CURRENT" "$THRESHOLD" "$BACKENDS"
    printf '| backend | benchmark | baseline_mean_ns | current_mean_ns | delta_pct | status |\n'
    printf '| --- | --- | --- | --- | --- | --- |\n'
    awk -F'\t' '{
        delta = $5
        if (delta != "-" && delta + 0 > 0) delta = "+" delta
        printf "| %s | %s | %s | %s | %s | %s |\n", $1, $2, $3, $4, delta, $6
    }' "$ROWS"
} > "$REPORT"

cat "$REPORT"

printf '\n%s %s row(s): %s pass, %s warn, %s fail\n' "$LOG" "$rows" "$passes" "$warns" "$fails"
printf '%s report: %s\n' "$LOG" "$REPORT"

if [ "$fails" -gt 0 ]; then
    printf '%s RESULT: FAIL — %s benchmark(s) regressed more than %s%%\n' "$LOG" "$fails" "$THRESHOLD" >&2
    exit 1
fi

printf '%s RESULT: PASS\n' "$LOG"
exit 0
