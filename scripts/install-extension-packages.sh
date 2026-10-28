#!/usr/bin/env bash
# Build pass packages for this host and install them into the check environments.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=$ROOT/target/extension-packages
PY=${ORM_PACKAGE_PYTHON:-$ROOT/.venv/bin/python}
TSC=$ROOT/languages/js/node_modules/.bin/tsc
# Install the compiler before the local package dependencies exist.
if [ ! -x "$TSC" ]; then
  tools=$OUT/tools
  mkdir -p "$tools"
  npm install --prefix "$tools" --no-audit --no-fund typescript@^5.9.0
  TSC=$tools/node_modules/.bin/tsc
fi
for extension in timestamps soft-delete locking inherit query-defaults generic proxy model-composition file-storage; do
  # A renamed package leaves its old wheel behind, and the install below globs every wheel.
  rm -rf "$OUT/$extension"
  cargo run -q --manifest-path "$ROOT/Cargo.toml" -p monorm-extension-package -- package "$ROOT/extensions/$extension" --out "$OUT/$extension" --tsc "$TSC"
done
uv pip install --python "$PY" "$OUT"/*/*.whl
# The package directories import monorm; resolve it through the checkout.
mkdir -p "$ROOT/node_modules/@ufstack"
ln -sfn ../../languages/js "$ROOT/node_modules/@ufstack/monorm"
(cd "$ROOT/languages/js" && npm install --no-audit --no-fund)
(cd "$ROOT/apps/auth/typescript" && npm install --no-audit --no-fund)
