#!/usr/bin/env bash
# Run the internal benchmark: the same cases through the unprepared, prepared and direct-IR paths
# in Python and TypeScript (Node), plus the engine-only (Rust) and driver-only rows.
#
#   ORM_BENCH_URL=postgres://... bench/internal/run.sh [--rounds N] [--skip-build] [--ignore-load] [--out DIR]
#   bench/internal/run.sh --quick
#
# The run drops and creates the tables `users` and `posts` of the database: use a disposable one.
# Without ORM_BENCH_URL it uses a SQLite file in bench/internal/build/.
# The Python runner seeds the database and writes the dump that the Rust runner replays; then Node, then Rust.
#
#   --quick         smoke: no build, the dev artifacts (.venv, languages/js/monorm.node, languages/js/dist),
#                   one call per case and path, no Rust row. Each case still checks that the paths agree. The numbers mean nothing.
#   --ignore-load   run although the 1-minute load is above 2. The numbers are not valid. The result directory ends in -smoke.
#   --skip-build    reuse the artifacts in build/ from an earlier run
#   --out DIR       write the results to DIR (default results/<date>-<sha>-<backend>[-quick|-smoke])
#
# Builds go to bench/internal/build/ (git-ignored). The default .venv and languages/js/monorm.node are never touched
# by a full run.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
BUILD=$HERE/build
ROUNDS=15
QUICK=0
SKIP_BUILD=0
IGNORE_LOAD=0
OUT=""
while [ $# -gt 0 ]; do
  case $1 in
    --rounds) ROUNDS=$2; shift ;;
    --quick) QUICK=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --ignore-load) IGNORE_LOAD=1 ;;
    --out) OUT=$2; shift ;;
    *) echo "unknown flag $1" >&2; exit 2 ;;
  esac
  shift
done

SHA=$(git -C "$ROOT" rev-parse --short HEAD)
STAMP=$(date +%Y-%m-%d)
PYTHON=${PYTHON:-python3}
# monorm needs Python 3.11 or newer
"$PYTHON" -c 'import sys; sys.exit(sys.version_info < (3, 11))' || { echo "$PYTHON is older than 3.11: set PYTHON=/path/to/python3.11+" >&2; exit 1; }
case "$(uname -s)" in Darwin) DYLIB=dylib ;; *) DYLIB=so ;; esac

URL=${ORM_BENCH_URL:-}
if [ -z "$URL" ]; then
  URL=sqlite://$BUILD/bench.db
  rm -f "$BUILD/bench.db"
fi
case $URL in sqlite*) BACKEND=sqlite ;; *) BACKEND=postgresql ;; esac
if [ -z "$OUT" ]; then
  OUT=$HERE/results/$STAMP-$SHA-$BACKEND
  if [ "$QUICK" = 1 ]; then OUT=$OUT-quick; elif [ "$IGNORE_LOAD" = 1 ]; then OUT=$OUT-smoke; fi
fi
mkdir -p "$BUILD" "$OUT"

load1() { uptime | sed -E 's/.*load averages?: *([0-9.]+).*/\1/'; }
MAX_LOAD=2
wait_quiet() {
  local t0 load
  t0=$(date +%s)
  while load=$(load1); awk "BEGIN{exit !($load > $MAX_LOAD)}"; do
    if [ "$IGNORE_LOAD" = 1 ]; then echo "WARNING: 1-minute load $load is above $MAX_LOAD; --ignore-load given. These numbers are not valid."; return; fi
    if [ $(( $(date +%s) - t0 )) -gt 300 ]; then echo "1-minute load $load stays above $MAX_LOAD: stop. Quiet the machine and rerun." >&2; exit 1; fi
    sleep 5
  done
}

if [ "$QUICK" = 1 ]; then
  PY=$ROOT/.venv/bin/python
  NATIVE=$ROOT/languages/js/monorm.node
  [ -x "$PY" ] && [ -f "$NATIVE" ] && [ -d "$ROOT/languages/js/dist" ] \
    || { echo "--quick needs the dev build: .venv with maturin develop, languages/js/monorm.node and languages/js/dist" >&2; exit 1; }
  ROUNDS=1
else
  # Release timing needs the real release profile (fat LTO, one codegen unit).
  unset CARGO_PROFILE_RELEASE_LTO CARGO_PROFILE_RELEASE_CODEGEN_UNITS
  export CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$BUILD/target
  VENV=$BUILD/venv
  PY=$VENV/bin/python
  NATIVE=$BUILD/monorm.node
  RUST=$BUILD/target/release/monorm-internal-bench
  build_python() {
    [ -x "$PY" ] || uv venv -q -p "$PYTHON" "$VENV"
    uv pip install -q --python "$PY" maturin "psycopg[binary]"
    rm -rf "$BUILD/wheels" && mkdir -p "$BUILD/wheels"
    "$PY" "$ROOT/languages/python/packaging/stage.py"
    (cd "$ROOT" && "$VENV/bin/maturin" build --release -q -i "$PY" -o "$BUILD/wheels")
    uv pip install -q --python "$PY" --force-reinstall --no-deps "$BUILD"/wheels/monorm-*.whl
  }
  build_node() {
    (cd "$ROOT/languages/js" && npm install --no-audit --no-fund -s && npm run -s build)
    (cd "$ROOT" && cargo build -q --release -p monorm-node --no-default-features --features profile-combined)
    cp "$CARGO_TARGET_DIR/release/libmonorm_node.$DYLIB" "$NATIVE"
    (cd "$HERE/js" && npm install --no-audit --no-fund -s)
  }
  build_rust() {
    cargo build -q --release --manifest-path "$HERE/rust/Cargo.toml"
  }
  if [ "$SKIP_BUILD" = 0 ]; then
    build_python
    build_node
    build_rust
  fi
  [ -x "$PY" ] && [ -f "$NATIVE" ] && [ -x "$RUST" ] || { echo "no build in $BUILD; run without --skip-build" >&2; exit 1; }
  wait_quiet
fi

LOAD=$(load1)
UPTIME_BEFORE=$(uptime)
LOG=$OUT/logs
mkdir -p "$LOG"
DUMP=$BUILD/dump.json
QUICK_FLAG=()
[ "$QUICK" = 0 ] || QUICK_FLAG=(--quick)

echo "== Python on $BACKEND"
"$PY" "$HERE/python/bench.py" --url "$URL" --out "$OUT" --seed --rounds "$ROUNDS" --dump "$DUMP" ${QUICK_FLAG[@]+"${QUICK_FLAG[@]}"} 2>&1 | tee "$LOG/python.log"
echo "== TypeScript on Node on $BACKEND"
(cd "$HERE/js" && MONORM_NATIVE=$NATIVE node bench.mjs --url "$URL" --out "$OUT" --rounds "$ROUNDS" ${QUICK_FLAG[@]+"${QUICK_FLAG[@]}"}) 2>&1 | tee "$LOG/node.log"
RUST_NOTE=""
if [ "$QUICK" = 1 ]; then
  RUST_NOTE="skipped in --quick: the engine-only row needs the release build"
  echo "== Rust: $RUST_NOTE"
else
  echo "== Rust (engine only) on $BACKEND"
  "$RUST" --url "$URL" --out "$OUT" --rounds "$ROUNDS" --dump "$DUMP" --schema "$HERE/schema.prisma" 2>&1 | tee "$LOG/rust.log"
fi

LOAD_END=$(load1)
VALID=true
REASON=""
if [ "$QUICK" = 1 ]; then
  VALID=false
  REASON="--quick: one call per case and path, a smoke check"
elif [ "$IGNORE_LOAD" = 1 ]; then
  VALID=false
  REASON="--ignore-load: the load check was off"
elif awk "BEGIN{exit !($LOAD_END > $MAX_LOAD)}"; then
  VALID=false
  REASON="1-minute load $LOAD_END at the end is above $MAX_LOAD: the machine was not quiet, repeat the run"
fi
# Python writes the JSON, so a load value that does not parse fails here and never makes a broken file.
RUN_DATE=$(date -u +%Y-%m-%dT%H:%M:%SZ) GIT_SHA=$(git -C "$ROOT" rev-parse HEAD) BACKEND=$BACKEND ROUNDS=$ROUNDS QUICK=$QUICK \
  RUST_NOTE=$RUST_NOTE LOAD=$LOAD LOAD_END=$LOAD_END VALID=$VALID REASON=$REASON UPTIME_BEFORE=$UPTIME_BEFORE UPTIME_AFTER=$(uptime) \
  "$PY" - "$OUT/run.json" <<'PYEOF'
import json, os, sys
e = os.environ
meta = {
    "date": e["RUN_DATE"], "git_sha": e["GIT_SHA"], "backend": e["BACKEND"], "rounds": int(e["ROUNDS"]), "quick": e["QUICK"] == "1",
    "rust": e["RUST_NOTE"], "load1_at_start": float(e["LOAD"]), "load1_at_end": float(e["LOAD_END"]),
    "valid": e["VALID"] == "true", "invalid_reason": e["REASON"], "uptime_before": e["UPTIME_BEFORE"], "uptime_after": e["UPTIME_AFTER"],
}
open(sys.argv[1], "w").write(json.dumps(meta, indent=2) + "\n")
PYEOF

"$PY" "$HERE/report.py" "$OUT"
echo "wrote $OUT/summary.md"
