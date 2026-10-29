#!/usr/bin/env bash
# Main seam: build the one wheel and the one tarball of this checkout, install them in fresh consumers, generate, import and query.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
: "${ORM_TEST_DATABASE_URL:=postgres://postgres:postgres@localhost/orm_test}"
export ORM_TEST_DATABASE_URL
mkdir -p "$ROOT/target"
WORK=$(mktemp -d "$ROOT/target/package-seam-XXXXXX")
# CI uploads $WORK, so only a local pass removes it.
trap 'status=$?; if [ "$status" -eq 0 ] && [ -z "${CI:-}" ]; then rm -rf "$WORK"; else echo "Package seam artifacts: $WORK"; fi' EXIT
PYTHON=${PYTHON:-python3}
TSC=$ROOT/languages/js/node_modules/.bin/tsc
DIST=$WORK/dist
mkdir -p "$DIST"
# The wheel: the engine, the nine extension libraries and auth.
"$PYTHON" "$ROOT/languages/python/packaging/stage.py"
(cd "$ROOT" && maturin build --release --out "$DIST")
# The tarball: `prepack` builds `dist/` and checks the package content.
(cd "$ROOT/languages/js" && npm install --no-audit --no-fund && npm run assemble && npm pack --pack-destination "$DIST" --silent)
# The outside extension acme.outside, as a wheel and a tarball.
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p monorm-extension-package -- package "$ROOT/tests/outside-extension" --out "$WORK/outside" --tsc "$TSC"
cp "$WORK/outside"/*.whl "$DIST"
(cd "$WORK/outside/npm" && npm pack --pack-destination "$DIST" --silent)
"$PYTHON" "$ROOT/scripts/check-assembled-consumers.py" "$DIST" postgres,sqlite
# A library of a newer ABI minor fails `define()` with both versions in the message.
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p monorm-extension-package -- package "$ROOT/tests/outside-extension" --out "$WORK/outside-newer" --tsc "$TSC" --features abi-newer
uv venv --python "$PYTHON" "$WORK/venv-newer"
uv pip install --python "$WORK/venv-newer/bin/python" "$DIST"/monorm-*.whl "$WORK/outside-newer"/*.whl
sed "s/__DIALECT__/sqlite/" "$ROOT/scripts/tests/package_consumer.prisma" > "$WORK/newer.prisma"
if (cd "$WORK" && "$WORK/venv-newer/bin/python" -m monorm --schema newer.prisma generate python -o newer.py) 2> "$WORK/newer.err"; then
  echo "a library of ABI 0.3 must fail define()" >&2
  exit 1
fi
if ! grep -q "ABI 0.3" "$WORK/newer.err" || ! grep -q "ABI 0.2" "$WORK/newer.err"; then
  cat "$WORK/newer.err" >&2
  exit 1
fi
