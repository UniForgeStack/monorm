#!/usr/bin/env bash
# Build release artifacts, check the load, run the equality checks and every contender, write results/<date>-<sha>/.
#
#   bench/compare/run.sh [--rounds N] [--iters N] [--skip-build] [--ignore-load] [--no-all] [--fast-build]
#
# Order: build, wait for a quiet machine, fresh database, Python (check, then timing), TypeScript on Node
# (check, then timing), Bun, report. Each runner does the equality check before it times anything, waits for
# a quiet machine before every round, and rotates the contender order every round.
# The Rust slot runs only when bench/compare/rust exists, and no runner for it exists yet, so it is skipped.
# Builds go to bench/compare/build/ (git-ignored): a venv, a Node addon and the `all` extension artifact.
# The default `.venv` and languages/js/orm.node are never touched.
#
#   --ignore-load   run although the 1-minute load is above 2 (no wait, no per-round wait). Pipeline smoke only: the numbers are not valid.
#   --fast-build    keep the CARGO_PROFILE_RELEASE_* overrides of the environment (no fat LTO). Smoke only.
#   --no-all        skip the `all` extension artifact (orm-all rows)
#   --skip-build    reuse the artifacts in build/ from an earlier run
#
# Environment: BENCH_ADMIN_URL (default postgres://postgres:postgres@localhost:55432/postgres),
# BENCH_DB_NAME (default bench; the run drops and recreates it, then drops it at the end),
# PYTHON (default /opt/homebrew/bin/python3.14).
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
BUILD=$HERE/build
ROUNDS=20
ITERS=20
SKIP_BUILD=0
IGNORE_LOAD=0
BUILD_ALL=1
FAST=0
while [ $# -gt 0 ]; do
  case $1 in
    --rounds) ROUNDS=$2; shift ;;
    --iters) ITERS=$2; shift ;;
    --skip-build) SKIP_BUILD=1 ;;
    --ignore-load) IGNORE_LOAD=1 ;;
    --no-all) BUILD_ALL=0 ;;
    --fast-build) FAST=1 ;;
    *) echo "unknown flag $1" >&2; exit 2 ;;
  esac
  shift
done

ADMIN_URL=${BENCH_ADMIN_URL:-postgres://postgres:postgres@localhost:55432/postgres}
PYTHON=${PYTHON:-/opt/homebrew/bin/python3.14}
SHA=$(git -C "$ROOT" rev-parse --short HEAD)
STAMP=$(date +%Y-%m-%d)
OUT=$HERE/results/$STAMP-$SHA
# A run that overrides the load check or the release profile is a smoke: its directory is git-ignored.
{ [ "$IGNORE_LOAD" = 0 ] && [ "$FAST" = 0 ]; } || OUT=$OUT-smoke
case "$(uname -s)" in Darwin) DYLIB=dylib ;; *) DYLIB=so ;; esac

# Release timing needs the real release profile (fat LTO, one codegen unit).
if [ "$FAST" = 0 ]; then unset CARGO_PROFILE_RELEASE_LTO CARGO_PROFILE_RELEASE_CODEGEN_UNITS; fi
export CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$BUILD/target PYO3_PYTHON=$PYTHON

load1() { uptime | sed -E 's/.*load averages?: *([0-9.]+).*/\1/'; }
MAX_LOAD=2
# The builds heat the machine, so the load is checked after them: wait up to 5 minutes for a quiet machine.
wait_quiet() {
  local t0 load
  t0=$(date +%s)
  while load=$(load1); awk "BEGIN{exit !($load > $MAX_LOAD)}"; do
    if [ "$IGNORE_LOAD" = 1 ]; then echo "WARNING: 1-minute load $load is above $MAX_LOAD; --ignore-load given. These numbers are not valid."; return; fi
    if [ $(( $(date +%s) - t0 )) -gt 300 ]; then echo "1-minute load $load stays above $MAX_LOAD: stop. Quiet the machine and rerun." >&2; exit 1; fi
    sleep 5
  done
}
# The runners wait before every round; --ignore-load switches that wait off.
RUNNER_LOAD=$MAX_LOAD
[ "$IGNORE_LOAD" = 0 ] || RUNNER_LOAD=1000

mkdir -p "$BUILD" "$OUT/python" "$OUT/js"
TIMES=$OUT/build-times.txt
[ "$SKIP_BUILD" = 1 ] || : >"$TIMES"
timed() { # timed LABEL COMMAND...: run a build step and record its wall time (measured)
  local label=$1 t0; shift
  t0=$(date +%s)
  "$@"
  echo "$label $(( $(date +%s) - t0 ))s" | tee -a "$TIMES"
}

VENV=$BUILD/venv
PY=$VENV/bin/python
ALL=$BUILD/all

build_python() {
  [ -x "$PY" ] || uv venv -q -p "$PYTHON" "$VENV"
  uv pip install -q --python "$PY" maturin==1.15.0 mypy==2.4.0 pyright==1.1.414 \
    django==6.1.2 "psycopg[binary]==3.3.6" psycopg-pool==3.3.3 asgiref==3.12.1 \
    "sqlalchemy[asyncio]==2.1.6" asyncpg==0.32.0
  rm -rf "$BUILD/wheels" && mkdir -p "$BUILD/wheels"
  # The default profile of the shipped wheel: postgres, release.
  (cd "$ROOT/languages/python/packaging/postgres" && "$VENV/bin/maturin" build --release -q -i "$PY" -o "$BUILD/wheels")
  uv pip install -q --python "$PY" --force-reinstall --no-deps "$BUILD"/wheels/orm_native_postgres-*.whl
  uv pip install -q --python "$PY" --force-reinstall --no-deps "$ROOT"
}

build_node() {
  (cd "$ROOT/languages/js" && npm install --no-audit --no-fund -s && npm run -s build)
  (cd "$ROOT" && cargo build -q --release -p orm-node --no-default-features --features profile-postgres)
  cp "$CARGO_TARGET_DIR/release/liborm_node.$DYLIB" "$BUILD/orm.node"
  (cd "$HERE/js" && npm install --no-audit --no-fund -s && node make-prisma-schema.mjs && npx prisma generate >/dev/null)
}

# The `all` artifact, as scripts/feature-check.sh builds it, in release mode.
build_all() {
  local deps='"proxy":{"package":"orm-proxy","path":"'"$ROOT"'/extensions/proxy"}'
  deps+=',"query_defaults":{"package":"orm-query-defaults","path":"'"$ROOT"'/extensions/query-defaults"}'
  deps+=',"composition":{"package":"orm-model-composition","path":"'"$ROOT"'/extensions/model-composition"}'
  deps+=',"file_storage":{"package":"orm-file-storage-extension","path":"'"$ROOT"'/extensions/file-storage"}'
  deps+=',"generic":{"package":"orm-generic","path":"'"$ROOT"'/extensions/generic"}'
  rm -rf "$ALL" && mkdir -p "$ALL/py/orm_native_custom"
  printf '{"host":"%s","output":"%s/build","prepare_only":true,"bindings":["python","node"],"dependencies":{%s}}\n' \
    "$ROOT" "$ALL" "$deps" >"$ALL/config.json"
  # The extension build type-checks its generated Python with mypy from the PATH.
  (export PATH=$VENV/bin:$PATH; cd "$ROOT" && cargo run -q --release -p orm-extension-build -- "$ALL/config.json")
  (
    export ORM_CORE_COMPOSITION=$ALL/build/composition.rs ORM_ENGINE_COMPOSITION=$ALL/build/engine-composition.rs
    export ORM_PYTHON_METHODS=$ALL/build/python-methods.rs ORM_NODE_METHODS=$ALL/build/node-methods.rs
    cd "$ALL/build" && cargo build -q --release -p orm-python -p orm-node
  )
  local ext
  ext=$("$PY" -c 'import sysconfig; print(sysconfig.get_config_var("EXT_SUFFIX"))')
  cp "$ROOT/languages/python/packaging/custom/orm_native_custom/__init__.py" "$ALL/py/orm_native_custom/"
  cp "$CARGO_TARGET_DIR/release/lib_native.$DYLIB" "$ALL/py/orm_native_custom/_native$ext"
  cp "$CARGO_TARGET_DIR/release/liborm_node.$DYLIB" "$ALL/orm.node"
}

if [ "$SKIP_BUILD" = 0 ]; then
  timed build-python-wheel build_python
  timed build-node-addon build_node
  [ "$BUILD_ALL" = 0 ] || timed build-all-artifact build_all
fi
[ -x "$PY" ] && [ -f "$BUILD/orm.node" ] || { echo "no build in $BUILD; run without --skip-build" >&2; exit 1; }
ARTIFACT=()
if [ "$BUILD_ALL" = 1 ] && [ -f "$ALL/orm.node" ]; then ARTIFACT=(--orm-artifact "$ALL"); fi

wait_quiet
UPTIME_BEFORE=$(uptime)
LOAD=$(load1)

# A fresh database for this run, dropped at the end.
DB=${BENCH_DB_NAME:-bench}
DB_URL=${ADMIN_URL%/*}/$DB
psql "$ADMIN_URL" -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" -c "CREATE DATABASE $DB"
trap 'psql "$ADMIN_URL" -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" >/dev/null' EXIT
psql "$DB_URL" -q -v ON_ERROR_STOP=1 -f "$HERE/schema.sql" -f "$HERE/seed.sql" >/dev/null
export BENCH_DATABASE_URL=$DB_URL

LOG=$OUT/logs
mkdir -p "$LOG"
echo "== Python (check, then $ROUNDS rounds x $ITERS iterations)"
(cd "$HERE/python" && "$PY" run.py run --rounds "$ROUNDS" --iters "$ITERS" --max-load "$RUNNER_LOAD" --out "$OUT/python" ${ARTIFACT[@]+"${ARTIFACT[@]}"}) 2>&1 | tee "$LOG/python.log"

echo "== TypeScript on Node"
(cd "$HERE/js" && ORM_NATIVE=$BUILD/orm.node node run.mjs run --rounds "$ROUNDS" --iters "$ITERS" --max-load "$RUNNER_LOAD" --out "$OUT/js" ${ARTIFACT[@]+"${ARTIFACT[@]}"}) 2>&1 | tee "$LOG/node.log"

echo "== TypeScript on Bun"
(cd "$HERE/js" && ORM_NATIVE=$BUILD/orm.node bun run.mjs run --rounds "$ROUNDS" --iters "$ITERS" --max-load "$RUNNER_LOAD" --out "$OUT/js") 2>&1 | tee "$LOG/bun.log"

# The Rust slot: skipped, no runner exists for it.
if [ -d "$HERE/rust" ]; then echo "== Rust: bench/compare/rust exists, but run.sh has no runner for it yet; skipped"
else echo "== Rust: skipped (no bench/compare/rust)"; fi

LOAD_END=$(load1)
VALID=true
REASON=""
if [ "$IGNORE_LOAD" = 0 ] && awk "BEGIN{exit !($LOAD_END > $MAX_LOAD)}"; then
  VALID=false
  REASON="1-minute load $LOAD_END at the end is above $MAX_LOAD: the machine was not quiet, repeat the run"
fi
{
  echo "{"
  echo "  \"date\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\","
  echo "  \"git_sha\": \"$(git -C "$ROOT" rev-parse HEAD)\","
  echo "  \"rounds\": $ROUNDS, \"iters\": $ITERS,"
  echo "  \"load_check_ignored\": $([ "$IGNORE_LOAD" = 1 ] && echo true || echo false),"
  echo "  \"fast_build\": $([ "$FAST" = 1 ] && echo true || echo false),"
  echo "  \"load1_at_start\": $LOAD,"
  echo "  \"load1_at_end\": $LOAD_END,"
  echo "  \"valid\": $VALID,"
  echo "  \"invalid_reason\": \"$REASON\","
  echo "  \"uptime_before\": \"$UPTIME_BEFORE\","
  echo "  \"uptime_after\": \"$(uptime)\""
  echo "}"
} >"$OUT/run.json"

"$PY" "$HERE/report.py" "$OUT"
echo "wrote $OUT/summary.md"
