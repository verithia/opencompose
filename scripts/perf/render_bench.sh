#!/usr/bin/env bash
# Measure offline render throughput on the reference session.
# The harness never builds: build first, then point OCP_BIN at that binary.
#
# Usage:
#   scripts/perf/render_bench.sh
#   OCP_BENCH_PROJECT=path/to/other.ocp scripts/perf/render_bench.sh
#   OCP_RENDER_THREADS=0 scripts/perf/render_bench.sh    # machine default
#
# Environment:
#   OCP_BIN             release binary (default: target/release/ocp)
#   OCP_BENCH_PROJECT   project to render (default: benchmarks/session-20tracks.ocp)
#   OCP_RENDER_THREADS  worker pin (default: 4; 0 means machine default)
#   OCP_BENCH_REPEAT    runs to time, the median decides (default: 3)
#   OCP_SF2             SoundFont fallback (default: the checked-in test fixture)

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

TARGET_RATIO=10.0

resolve_path() {
    local path="$1"
    if [[ "$path" = /* ]]; then
        printf '%s\n' "$path"
    else
        printf '%s\n' "$ROOT/$path"
    fi
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 2
}

# --- inputs ---------------------------------------------------------------

if [[ -n "${OCP_BENCH_PROJECT:-}" ]]; then
    PROJECT="$(resolve_path "$OCP_BENCH_PROJECT")"
    PROJECT_SOURCE="OCP_BENCH_PROJECT override"
else
    PROJECT="$ROOT/benchmarks/session-20tracks.ocp"
    PROJECT_SOURCE="default"
fi
[[ -f "$PROJECT" ]] || fail "project not found: $PROJECT"

if [[ -n "${OCP_SF2:-}" ]]; then
    SOUNDFONT="$(resolve_path "$OCP_SF2")"
    SOUNDFONT_SOURCE="OCP_SF2 override"
else
    SOUNDFONT="$ROOT/crates/oc-engine/tests/fixtures/minimal-test.sf2"
    SOUNDFONT_SOURCE="default fixture (set OCP_SF2 for a GM font)"
fi
[[ -f "$SOUNDFONT" ]] || fail "SoundFont not found: $SOUNDFONT"

if [[ -n "${OCP_RENDER_THREADS:-}" ]]; then
    THREADS="$OCP_RENDER_THREADS"
    THREADS_SOURCE="OCP_RENDER_THREADS override"
else
    THREADS=4
    THREADS_SOURCE="default pin"
fi
[[ "$THREADS" =~ ^[0-9]+$ ]] || fail "OCP_RENDER_THREADS must be a non-negative integer, got '$THREADS'"
if [[ "$THREADS" == 0 ]]; then
    THREADS_LABEL="machine default"
else
    THREADS_LABEL="$THREADS"
fi

REPEAT="${OCP_BENCH_REPEAT:-3}"
[[ "$REPEAT" =~ ^[1-9][0-9]*$ ]] || fail "OCP_BENCH_REPEAT must be a positive integer, got '$REPEAT'"

load_average() {
    uptime | sed 's/.*load averages*: *//'
}

command -v python3 >/dev/null 2>&1 || fail "python3 is required for timing and WAV analysis"

BIN="$(resolve_path "${OCP_BIN:-target/release/ocp}")"
[[ -f "$BIN" ]] || fail "binary not found: $BIN (build it with: cargo build --release -p oc-cli)"
[[ -x "$BIN" ]] || fail "binary is not executable: $BIN"

# --- render ---------------------------------------------------------------

WORK="$(mktemp -d "${TMPDIR:-/tmp}/ocp-perf.XXXXXX")" || fail "cannot create temporary directory"
trap 'rm -rf "$WORK"' EXIT

WAV="$WORK/perf.wav"
LOG="$WORK/render.log"

printf 'OpenCompose render benchmark\n'
printf '  project    : %s (%s)\n' "$PROJECT" "$PROJECT_SOURCE"
printf '  binary     : %s\n' "$BIN"
printf '  threads    : %s (%s)\n' "$THREADS_LABEL" "$THREADS_SOURCE"
printf '  soundfont  : %s (%s)\n' "$SOUNDFONT" "$SOUNDFONT_SOURCE"
printf '  format     : 48 kHz stereo, no loudness normalisation\n'
printf '  runs       : %s (median decides)\n' "$REPEAT"
printf '  load       : %s\n\n' "$(load_average)"

WAV_READER=$(cat <<'PY'
import array
import math
import sys
import wave

path = sys.argv[1]
with wave.open(path, "rb") as reader:
    frames = reader.getnframes()
    rate = reader.getframerate()
    width = reader.getsampwidth()
    raw = reader.readframes(frames)

if width != 2:
    raise SystemExit(f"expected 16-bit PCM, got {width * 8}-bit")
samples = array.array("h")
samples.frombytes(raw)
if sys.byteorder == "big":
    samples.byteswap()

scale = 1.0 / 32768.0
peak = max((abs(value) for value in samples), default=0) * scale
means = math.fsum(value * value for value in samples) / max(len(samples), 1)
rms = math.sqrt(means) * scale
duration = frames / float(rate)
peak_db = 20.0 * math.log10(max(peak, 1e-12))
rms_db = 20.0 * math.log10(max(rms, 1e-12))
print(f"{duration:.3f} {peak_db:.2f} {rms_db:.2f}")
PY
)

median() {
    printf '%s\n' "$@" | sort -n | awk -v n="$#" 'NR == int((n + 1) / 2) { print }'
}

# One line per run, so a noisy neighbour is visible instead of averaged away.
RATIOS=()
WALLS=()
for ((run = 1; run <= REPEAT; run++)); do
    start="$(python3 -c 'import time; print(f"{time.monotonic():.9f}")')"
    OCP_RENDER_THREADS="$THREADS" "$BIN" render "$PROJECT" \
        --output "$WAV" \
        --soundfont "$SOUNDFONT" \
        --sample-rate 48000 \
        --no-loudness-normalisation >"$LOG" 2>&1
    status=$?
    end="$(python3 -c 'import time; print(f"{time.monotonic():.9f}")')"

    if [[ $status -ne 0 ]]; then
        printf 'render failed on run %s (exit %s):\n' "$run" "$status" >&2
        cat "$LOG" >&2
        exit "$status"
    fi

    if ! read -r DURATION PEAK_DBFS RMS_DBFS < <(python3 -c "$WAV_READER" "$WAV"); then
        fail "could not read rendered audio from $WAV"
    fi

    WALL="$(awk -v a="$start" -v b="$end" 'BEGIN { printf "%.3f", b - a }')"
    RATIO="$(awk -v d="$DURATION" -v w="$WALL" 'BEGIN { printf "%.2f", d / w }')"
    RATIOS+=("$RATIO")
    WALLS+=("$WALL")
    printf '  run %s/%s: %ss audio in %ss wall = %sx  (load %s)\n' \
        "$run" "$REPEAT" "$DURATION" "$WALL" "$RATIO" "$(load_average)"
    # Keep the renderer's summary, not the per-event and engine chatter.
    grep -E '(Rendering:|Parsed:|Done!|Loudness normalisation)' "$LOG" | sed 's/^/  | /' || true
done
printf '\n'

MEDIAN_RATIO="$(median "${RATIOS[@]}")"
if awk -v r="$MEDIAN_RATIO" -v t="$TARGET_RATIO" 'BEGIN { exit !(r >= t) }'; then
    RESULT="PASS"
    exit_code=0
else
    RESULT="FAIL"
    exit_code=1
fi

printf '+------------------------+----------------+\n'
printf '| %-22s | %14s |\n' "metric" "median"
printf '+------------------------+----------------+\n'
printf '| %-22s | %11ss |\n' "audio duration" "$DURATION"
printf '| %-22s | %11ss |\n' "wall clock" "$(median "${WALLS[@]}")"
printf '| %-22s | %12sx |\n' "throughput" "$MEDIAN_RATIO"
printf '| %-22s | %12sx |\n' "target" "$TARGET_RATIO"
printf '| %-22s | %14s |\n' "result" "$RESULT"
printf '+------------------------+----------------+\n'
printf '  peak %.2f dBFS, RMS %.2f dBFS\n' "$PEAK_DBFS" "$RMS_DBFS"
printf '  runs %s: %s\n' "$REPEAT" "${RATIOS[*]}"
printf '  load at end: %s\n' "$(load_average)"

exit "$exit_code"

