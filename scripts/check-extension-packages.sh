#!/usr/bin/env bash
# Main seam: fresh consumers, installed packages, generate, import and query.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
: "${ORM_TEST_DATABASE_URL:=postgres://postgres:postgres@localhost/orm_test}"
mkdir -p "$ROOT/target"
WORK=$(mktemp -d "$ROOT/target/package-seam-XXXXXX")
# CI uploads $WORK, so only a local pass removes it.
trap 'status=$?; if [ "$status" -eq 0 ] && [ -z "${CI:-}" ]; then rm -rf "$WORK"; else echo "Package seam artifacts: $WORK"; fi' EXIT
PYTHON=${PYTHON:-python3}
export PYTHON
ORM_PACKAGE_PYTHON="$ROOT/.venv/bin/python" "$ROOT/scripts/install-extension-packages.sh"
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p orm-extension-package -- package "$ROOT/tests/outside-extension" --out "$WORK/outside" --tsc "$ROOT/languages/js/node_modules/.bin/tsc"
(cd "$ROOT/languages/js" && npm run build:native -- --profile tooling && npm run build)
(cd "$ROOT/languages/python/packaging/tooling" && maturin build --out "$WORK/wheels")
uv build --wheel --out-dir "$WORK/wheels" "$ROOT"
uv venv --python "$PYTHON" "$WORK/venv"
uv pip install --python "$WORK/venv/bin/python" "$WORK/wheels"/*.whl "$ROOT/target/extension-packages"/*/*.whl "$WORK/outside"/*.whl
mkdir -p "$WORK/js" "$WORK/tarballs" "$WORK/native"
printf "module.exports = require('./orm.node');\n" > "$WORK/native/index.cjs"
cp "$ROOT/languages/js/orm.node" "$WORK/native/"
printf '{"name":"@orm/native-tooling","version":"0.1.0","main":"index.cjs"}\n' > "$WORK/native/package.json"
for dir in "$ROOT/languages/js" "$WORK/native" "$ROOT/target/extension-packages"/*/npm "$WORK/outside/npm"; do
  (cd "$dir" && npm pack --pack-destination "$WORK/tarballs" --silent)
done
printf '{"private":true,"type":"module"}\n' > "$WORK/js/package.json"
(cd "$WORK/js" && npm install --ignore-scripts --no-audit --no-fund "$WORK/tarballs"/*.tgz)
for dialect in postgres sqlite; do
  consumer=$WORK/js/$dialect
  mkdir -p "$consumer"
  cat > "$consumer/schema.prisma" <<SCHEMA
datasource db { provider = "$dialect" }
model Base {
  id Int @id
  @@inherit.abstract
  @@timestamps.updated_at("changed", mode: "application")
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@locking.version("version")
}
model Note {
  text String
  start Int
  end Int
  shop_id Int
  updated_by String?
  first String @default("")
  last String @default("")
  slug String?
  stamp String?
  editor String?
  region String?
  owner String?
  secret String
  @@inherit.extends(Base)
  @@outside.tag("installed")
  @@query.filter("positive", "id > 0")
  @@outside.guard
  @@map("pkg1d_seam_notes")
}
SCHEMA
  (cd "$WORK/js" && "$WORK/venv/bin/python" -m orm --schema "$consumer/schema.prisma" generate python -o "$consumer/models.py")
  (cd "$WORK/js" && node node_modules/orm/dist/src/cli.js --schema "$consumer/schema.prisma" generate typescript -o "$consumer/models.ts")
  "$ROOT/languages/js/node_modules/.bin/tsc" "$consumer/models.ts" --outDir "$consumer" --module ESNext --target ES2022 --moduleResolution Bundler --skipLibCheck --noCheck
  cp "$ROOT/scripts/tests/package_consumer.py" "$consumer/use.py"
  cp "$ROOT/scripts/tests/package_consumer.mjs" "$consumer/use.mjs"
  url=$ORM_TEST_DATABASE_URL
  if [ "$dialect" = sqlite ]; then url=sqlite://:memory:; fi
  (cd "$consumer" && ORM_DATABASE_URL="$url" "$WORK/venv/bin/python" use.py)
  (cd "$consumer" && ORM_DATABASE_URL="$url" node use.mjs)
  (cd "$consumer" && ORM_DATABASE_URL="$url" bun use.mjs)
done
# Without its package, `@@query.*` fails and neutral read defaults still work.
uv venv --python "$PYTHON" "$WORK/bare-venv"
uv pip install --python "$WORK/bare-venv/bin/python" "$WORK/wheels"/*.whl
"$WORK/bare-venv/bin/python" "$ROOT/extensions/query-defaults/tests/public/disabled.py"
mkdir -p "$WORK/bare-js"
printf '{"private":true,"type":"module"}\n' > "$WORK/bare-js/package.json"
bare_tarballs=()
for tarball in "$WORK/tarballs"/*.tgz; do
  case "$tarball" in *query-defaults*) ;; *) bare_tarballs+=("$tarball") ;; esac
done
(cd "$WORK/bare-js" && npm install --ignore-scripts --no-audit --no-fund "${bare_tarballs[@]}" && node "$ROOT/extensions/query-defaults/tests/public/disabled.mjs")
# A library of a newer ABI minor fails `define()` with both versions in the message.
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p orm-extension-package -- package "$ROOT/tests/outside-extension" --out "$WORK/outside-newer" --tsc "$ROOT/languages/js/node_modules/.bin/tsc" --features abi-newer
uv venv --python "$PYTHON" "$WORK/venv-newer"
uv pip install --python "$WORK/venv-newer/bin/python" "$WORK/wheels"/*.whl "$ROOT/target/extension-packages"/*/*.whl "$WORK/outside-newer"/*.whl
if (cd "$WORK/js/sqlite" && "$WORK/venv-newer/bin/python" -c "import models") 2> "$WORK/newer.err"; then
  echo "a library of ABI 0.3 must fail define()" >&2
  exit 1
fi
if ! grep -q "ABI 0.3" "$WORK/newer.err" || ! grep -q "ABI 0.2" "$WORK/newer.err"; then
  cat "$WORK/newer.err" >&2
  exit 1
fi
