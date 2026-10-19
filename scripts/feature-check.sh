#!/usr/bin/env bash
# Build every feature artifact through `orm-extension-build` and run all suites on each.
#
#   ORM_TEST_DATABASE_URL=postgres://... scripts/feature-check.sh [artifact...]
#
# Artifacts: proxy-models query-defaults model-composition file-storage generic-relations
# tracking (timestamps, soft-delete, locking) all
# (default: every one). Each artifact selects only extension crates; the manifests turn on
# the host Cargo features, so no cargo command here passes --features.
# Output, logs and the pass/fail matrix go to $ORM_FEATURE_CHECK_DIR (default target/feature-check).
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${ORM_FEATURE_CHECK_DIR:-$ROOT/target/feature-check}
: "${ORM_TEST_DATABASE_URL:?set ORM_TEST_DATABASE_URL to a PostgreSQL test database}"
export ORM_TEST_DATABASE_URL
# Nested cargo runs find the target dir in $WORK/.cargo/config.toml, so cargo runs from $WORK.
# Not CARGO_TARGET_DIR: sccache hashes every CARGO_* variable, and a per-checkout path would block cache hits.
TARGET=$WORK/target
mkdir -p "$WORK/.cargo" && printf '[build]\ntarget-dir = "%s"\n' "$TARGET" >"$WORK/.cargo/config.toml"
ARTIFACTS=${*:-proxy-models query-defaults model-composition file-storage generic-relations tracking all}
LOGS=$WORK/logs
MATRIX=$WORK/matrix.txt
case "$(uname -s)" in
  Darwin) DYLIB=dylib ;;
  *) DYLIB=so ;;
esac

# The dependency entry and expected capability of each extension crate.
dependency() {
  case $1 in
    proxy-models) echo '"proxy":{"package":"orm-proxy","path":"'"$ROOT"'/extensions/proxy"}' ;;
    query-defaults) echo '"query_defaults":{"package":"orm-query-defaults","path":"'"$ROOT"'/extensions/query-defaults"}' ;;
    model-composition) echo '"composition":{"package":"orm-model-composition","path":"'"$ROOT"'/extensions/model-composition"}' ;;
    file-storage) echo '"file_storage":{"package":"orm-file-storage-extension","path":"'"$ROOT"'/extensions/file-storage"}' ;;
    generic-relations) echo '"generic":{"package":"orm-generic","path":"'"$ROOT"'/extensions/generic"}' ;;
    tracking) echo '"timestamps":{"package":"orm-timestamps","path":"'"$ROOT"'/extensions/timestamps"},"soft_delete":{"package":"orm-soft-delete","path":"'"$ROOT"'/extensions/soft-delete"},"locking":{"package":"orm-locking","path":"'"$ROOT"'/extensions/locking"}' ;;
    *) echo "unknown feature $1" >&2; return 1 ;;
  esac
}
FEATURES="proxy-models query-defaults model-composition file-storage generic-relations tracking"
# file-storage and tracking are extension artifacts with no host capability.
HOST_FEATURES=${FEATURES/file-storage /}
HOST_FEATURES=${HOST_FEATURES/ tracking/}
features_of() { if [ "$1" = all ]; then echo "$FEATURES"; else echo "$1"; fi; }
has() { case " $(features_of "$1") " in *" $2 "*) return 0 ;; *) return 1 ;; esac; }

# step ARTIFACT SUITE COMMAND...: run one suite, log it, and record PASS or FAIL.
step() {
  local artifact=$1 suite=$2; shift 2
  local log=$LOGS/$artifact-$suite.log
  if "$@" >"$log" 2>&1; then echo "$artifact $suite PASS" | tee -a "$MATRIX"
  else echo "$artifact $suite FAIL ($log)" | tee -a "$MATRIX"; fi
}

rm -rf "$LOGS" "$MATRIX"
mkdir -p "$LOGS"

# One Python environment for every artifact: orm, its dev tools and the storage packages.
VENV=$WORK/venv
if [ ! -x "$VENV/bin/python" ]; then uv venv -q --python "${PYTHON:-python3}" "$VENV" || exit 1; fi
PY=$VENV/bin/python
uv pip install -q --python "$PY" -e "$ROOT[dev]" -e "$ROOT/extensions/file-storage/integration/python" || exit 1
export PYO3_PYTHON=$PY
EXT_SUFFIX=$("$PY" -c 'import sysconfig; print(sysconfig.get_config_var("EXT_SUFFIX"))')
(cd "$ROOT/languages/js" && npm install --no-audit --no-fund -s) >"$LOGS/npm-install.log" 2>&1 || { echo "npm install failed"; exit 1; }

# Suites that do not depend on an artifact.
step storage rust-extension sh -c "cd '$WORK' && cargo test -q --manifest-path '$ROOT/extensions/file-storage/Cargo.toml'"
step storage typescript-integration sh -c "cd '$ROOT/extensions/file-storage/integration/typescript' && npm install --no-audit --no-fund -s && npm test"

build() {
  local artifact=$1 dir=$WORK/$1 deps="" feature
  for feature in $(features_of "$artifact"); do deps="$deps${deps:+,}$(dependency "$feature")"; done
  rm -rf "$dir" && mkdir -p "$dir/py/orm_native_custom" || return 1
  printf '{"host":"%s","output":"%s/build","prepare_only":true,"bindings":["python","node"],"dependencies":{%s}}\n' \
    "$ROOT" "$dir" "$deps" >"$dir/config.json"
  (cd "$WORK" && cargo run -q --manifest-path "$ROOT/Cargo.toml" -p orm-extension-build -- "$dir/config.json") || return 1
  # The documented rebuild of an emitted workspace: composition paths, no feature flags.
  (cd "$dir/build" && cargo build -q -p orm-python -p orm-node) || return 1
  cp "$ROOT/languages/python/packaging/custom/orm_native_custom/__init__.py" "$dir/py/orm_native_custom/"
  cp "$TARGET/debug/lib_native.$DYLIB" "$dir/py/orm_native_custom/_native$EXT_SUFFIX"
  cp "$TARGET/debug/liborm_node.$DYLIB" "$dir/orm.node"
}

# The loaded artifacts report exactly the selected host features.
capabilities() {
  local artifact=$1 dir=$WORK/$1 expected actual_py actual_node
  expected=$(for f in $(features_of "$artifact"); do [ "$f" = file-storage ] || [ "$f" = tracking ] || echo "$f"; done | sort | tr '\n' ' ' | sed 's/ $//')
  actual_py=$(PYTHONPATH=$dir/py ORM_PROFILE=custom "$PY" -c "import json; from orm import _native; print(' '.join(sorted(set(json.loads(_native.native_artifact())['capabilities']) & set('$HOST_FEATURES'.split()))))")
  actual_node=$(node -e "const a = JSON.parse(require(process.argv[1]).nativeArtifact()); console.log(a.capabilities.filter(c => '$HOST_FEATURES'.split(' ').includes(c)).sort().join(' '))" "$dir/orm.node")
  # The embedded build record names the same Cargo features.
  built_py=$(PYTHONPATH=$dir/py ORM_PROFILE=custom "$PY" -c "import json; from orm import _native; print(' '.join(sorted(set(json.loads(_native.profile_metadata())['build']['features']) & set('$HOST_FEATURES'.split()))))")
  built_node=$(node -e "const b = JSON.parse(require(process.argv[1]).profileMetadata()).build; console.log(b.features.filter(f => '$HOST_FEATURES'.split(' ').includes(f)).sort().join(' '))" "$dir/orm.node")
  echo "expected: $expected"; echo "python:   $actual_py (build: $built_py)"; echo "node:     $actual_node (build: $built_node)"
  [ "$actual_py" = "$expected" ] && [ "$actual_node" = "$expected" ] && [ "$built_py" = "$expected" ] && [ "$built_node" = "$expected" ]
}

for artifact in $ARTIFACTS; do
  dir=$WORK/$artifact
  export ORM_CORE_COMPOSITION=$dir/build/composition.rs ORM_ENGINE_COMPOSITION=$dir/build/engine-composition.rs
  export ORM_PYTHON_METHODS=$dir/build/python-methods.rs ORM_NODE_METHODS=$dir/build/node-methods.rs
  step "$artifact" build build "$artifact"
  grep -q "^$artifact build PASS" "$MATRIX" || continue
  step "$artifact" capabilities capabilities "$artifact"
  step "$artifact" cargo sh -c "cd '$dir/build' && cargo test -q --workspace"
  step "$artifact" pytest env PYTHONPATH="$dir/py" ORM_PROFILE=custom sh -c "cd '$ROOT' && '$PY' -m pytest -q -p no:cacheprovider"
  step "$artifact" npm env ORM_NATIVE="$dir/orm.node" sh -c "cd '$ROOT/languages/js' && npm test"
  step "$artifact" bun env ORM_NATIVE="$dir/orm.node" sh -c "cd '$ROOT/languages/js' && npm run -s test:bun"
  if has "$artifact" query-defaults; then
    step "$artifact" query-defaults-python env PYTHONPATH="$dir/py" ORM_PROFILE=custom "$PY" "$ROOT/extensions/query-defaults/tests/public/python.py"
    step "$artifact" query-defaults-node env ORM_NATIVE="$dir/orm.node" node "$ROOT/extensions/query-defaults/tests/public/node.mjs"
  else
    step "$artifact" query-defaults-disabled-python env PYTHONPATH="$dir/py" ORM_PROFILE=custom "$PY" "$ROOT/extensions/query-defaults/tests/public/disabled.py"
    step "$artifact" query-defaults-disabled-node env ORM_NATIVE="$dir/orm.node" node "$ROOT/extensions/query-defaults/tests/public/disabled.mjs"
  fi
  if has "$artifact" file-storage; then
    # These tests import `orm.adapters`, which needs a native profile.
    step "$artifact" storage-python-unit env PYTHONPATH="$dir/py" ORM_PROFILE=custom "$PY" -m pytest -q -p no:cacheprovider "$ROOT/extensions/file-storage/integration/python/tests"
    for url in "sqlite://:memory:" "$ORM_TEST_DATABASE_URL"; do
      db=${url%%:*}
      step "$artifact" "storage-python-$db" env PYTHONPATH="$dir/py" ORM_PROFILE=custom FILE_STORAGE_DATABASE_URL="$url" "$PY" "$ROOT/extensions/file-storage/integration/tests/native_python.py"
      step "$artifact" "storage-node-$db" env ORM_NATIVE="$dir/orm.node" FILE_STORAGE_DATABASE_URL="$url" sh -c "cd '$ROOT/extensions/file-storage/integration/typescript' && npm install --no-audit --no-fund -s && npm run -s build && node --test '$ROOT/extensions/file-storage/integration/tests/native_node.mjs'"
    done
  fi
done

echo
echo "Feature check matrix ($MATRIX):"
column -t "$MATRIX" 2>/dev/null || cat "$MATRIX"
! grep -q FAIL "$MATRIX"
