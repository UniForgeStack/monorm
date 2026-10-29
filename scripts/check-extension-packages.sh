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
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p monorm-extension-package -- package "$ROOT/tests/outside-extension" --out "$WORK/outside" --tsc "$ROOT/languages/js/node_modules/.bin/tsc"
(cd "$ROOT/languages/js" && npm run build:native -- --profile tooling && npm run build)
(cd "$ROOT/languages/python/packaging/tooling" && maturin build --out "$WORK/wheels")
uv build --wheel --out-dir "$WORK/wheels" "$ROOT"
uv venv --python "$PYTHON" "$WORK/venv"
uv pip install --python "$WORK/venv/bin/python" "$WORK/wheels"/*.whl "$ROOT/target/extension-packages"/*/*.whl "$WORK/outside"/*.whl
mkdir -p "$WORK/js" "$WORK/tarballs" "$WORK/native"
printf "module.exports = require('./monorm.node');\n" > "$WORK/native/index.cjs"
cp "$ROOT/languages/js/monorm.node" "$WORK/native/"
version=$(node -p 'require(process.argv[1]).version' "$ROOT/languages/js/package.json")
printf '{"name":"@ufstack/monorm-native-tooling","version":"%s","main":"index.cjs"}\n' "$version" > "$WORK/native/package.json"
for dir in "$ROOT/languages/js" "$WORK/native" "$ROOT/target/extension-packages"/*/npm "$WORK/outside/npm"; do
  (cd "$dir" && npm pack --pack-destination "$WORK/tarballs" --silent)
done
printf '{"private":true,"type":"module"}\n' > "$WORK/js/package.json"
(cd "$WORK/js" && npm install --ignore-scripts --no-audit --no-fund "$WORK/tarballs"/*.tgz)
# The file-storage package: unit tests once, then the native tests on each database below.
"$WORK/venv/bin/python" -m unittest discover -s "$ROOT/extensions/file-storage/integration/python/tests"
"$ROOT/languages/js/node_modules/.bin/tsc" "$ROOT"/extensions/file-storage/integration/typescript/test/*.ts --rootDir "$ROOT/extensions/file-storage" --outDir "$WORK/js/file-storage" --module ESNext --target ES2022 --moduleResolution Bundler --skipLibCheck --noCheck
cp -R "$ROOT/extensions/file-storage/fixtures" "$WORK/js/file-storage/fixtures"
(cd "$WORK/js" && node --test file-storage/integration/typescript/test/*.test.js)
for dialect in postgres sqlite; do
  consumer=$WORK/js/$dialect
  mkdir -p "$consumer"
  sed "s/__DIALECT__/$dialect/" "$ROOT/scripts/tests/package_consumer.prisma" > "$consumer/schema.prisma"
  (cd "$WORK/js" && "$WORK/venv/bin/python" -m monorm --schema "$consumer/schema.prisma" generate python -o "$consumer/models.py")
  (cd "$WORK/js" && node node_modules/@ufstack/monorm/dist/src/cli.js --schema "$consumer/schema.prisma" generate typescript -o "$consumer/models.ts")
  "$ROOT/languages/js/node_modules/.bin/tsc" "$consumer/models.ts" --outDir "$consumer" --module ESNext --target ES2022 --moduleResolution Bundler --skipLibCheck --noCheck
  cp "$ROOT/scripts/tests/package_consumer.py" "$consumer/use.py"
  cp "$ROOT/scripts/tests/package_consumer.mjs" "$consumer/use.mjs"
  url=$ORM_TEST_DATABASE_URL
  if [ "$dialect" = sqlite ]; then url=sqlite://:memory:; fi
  (cd "$consumer" && MONORM_DATABASE_URL="$url" "$WORK/venv/bin/python" use.py)
  (cd "$consumer" && MONORM_DATABASE_URL="$url" node use.mjs)
  (cd "$consumer" && MONORM_DATABASE_URL="$url" bun use.mjs)
  # The generic package: identities are frozen before generation.
  generic=$consumer/generic
  mkdir -p "$generic"
  { printf 'datasource db { provider = "%s" }\n' "$dialect"; cat "$ROOT/examples/blog/demo/generic-relations.prisma"; } > "$generic/schema.prisma"
  (cd "$WORK/js" && "$WORK/venv/bin/python" -m monorm --schema "$generic/schema.prisma" identities)
  (cd "$WORK/js" && "$WORK/venv/bin/python" -m monorm --schema "$generic/schema.prisma" generate python -o "$generic/models.py")
  (cd "$WORK/js" && node node_modules/@ufstack/monorm/dist/src/cli.js --schema "$generic/schema.prisma" generate typescript -o "$generic/models.ts")
  "$ROOT/languages/js/node_modules/.bin/tsc" "$generic/models.ts" --outDir "$generic" --module ESNext --target ES2022 --moduleResolution Bundler --skipLibCheck --noCheck
  cp "$ROOT/scripts/tests/generic_consumer.py" "$generic/use.py"
  cp "$ROOT/scripts/tests/generic_consumer.mjs" "$generic/use.mjs"
  (cd "$generic" && MONORM_DATABASE_URL="$url" "$WORK/venv/bin/python" use.py)
  (cd "$generic" && MONORM_DATABASE_URL="$url" node use.mjs)
  (cd "$generic" && MONORM_DATABASE_URL="$url" bun use.mjs)
  cp "$ROOT/extensions/file-storage/integration/tests/native_node.mjs" "$consumer/file-storage.test.mjs"
  (cd "$consumer" && FILE_STORAGE_DATABASE_URL="$url" "$WORK/venv/bin/python" "$ROOT/extensions/file-storage/integration/tests/native_python.py")
  (cd "$consumer" && FILE_STORAGE_DATABASE_URL="$url" node --test file-storage.test.mjs)
  (cd "$consumer" && FILE_STORAGE_DATABASE_URL="$url" bun test file-storage.test.mjs)
  # The proxy package: a proxy read through the installed package.
  proxy=$consumer/proxy
  mkdir -p "$proxy"
  { printf 'datasource db { provider = "%s" }\n' "$dialect"; grep -v '^//' "$ROOT/examples/blog/demo/proxy-models.prisma"; } > "$proxy/schema.prisma"
  (cd "$WORK/js" && "$WORK/venv/bin/python" -m monorm --schema "$proxy/schema.prisma" generate python -o "$proxy/models.py")
  (cd "$WORK/js" && node node_modules/@ufstack/monorm/dist/src/cli.js --schema "$proxy/schema.prisma" generate typescript -o "$proxy/models.ts")
  "$ROOT/languages/js/node_modules/.bin/tsc" "$proxy/models.ts" --outDir "$proxy" --module ESNext --target ES2022 --moduleResolution Bundler --skipLibCheck --noCheck
  cp "$ROOT/scripts/tests/proxy_consumer.py" "$proxy/use.py"
  cp "$ROOT/scripts/tests/proxy_consumer.mjs" "$proxy/use.mjs"
  (cd "$proxy" && MONORM_DATABASE_URL="$url" "$WORK/venv/bin/python" use.py)
  (cd "$proxy" && MONORM_DATABASE_URL="$url" node use.mjs)
  (cd "$proxy" && MONORM_DATABASE_URL="$url" bun use.mjs)
  # The composition package: a composed insert through the installed package.
  composition=$consumer/composition
  mkdir -p "$composition"
  { printf 'datasource db { provider = "%s" }\n' "$dialect"; grep -v '^//' "$ROOT/examples/blog/demo/model-composition.prisma"; } > "$composition/schema.prisma"
  (cd "$WORK/js" && "$WORK/venv/bin/python" -m monorm --schema "$composition/schema.prisma" generate python -o "$composition/models.py")
  (cd "$WORK/js" && node node_modules/@ufstack/monorm/dist/src/cli.js --schema "$composition/schema.prisma" generate typescript -o "$composition/models.ts")
  "$ROOT/languages/js/node_modules/.bin/tsc" "$composition/models.ts" --outDir "$composition" --module ESNext --target ES2022 --moduleResolution Bundler --skipLibCheck --noCheck
  cp "$ROOT/scripts/tests/composition_consumer.py" "$composition/use.py"
  cp "$ROOT/scripts/tests/composition_consumer.mjs" "$composition/use.mjs"
  (cd "$composition" && MONORM_DATABASE_URL="$url" "$WORK/venv/bin/python" use.py)
  (cd "$composition" && MONORM_DATABASE_URL="$url" node use.mjs)
  (cd "$composition" && MONORM_DATABASE_URL="$url" bun use.mjs)
done
# Without their packages, `@@query.*` and `@storage.file` fail and neutral read defaults still work.
uv venv --python "$PYTHON" "$WORK/bare-venv"
uv pip install --python "$WORK/bare-venv/bin/python" "$WORK/wheels"/*.whl
"$WORK/bare-venv/bin/python" "$ROOT/extensions/query-defaults/tests/public/disabled.py"
"$WORK/bare-venv/bin/python" "$ROOT/extensions/file-storage/tests/public/disabled.py"
mkdir -p "$WORK/bare-js"
printf '{"private":true,"type":"module"}\n' > "$WORK/bare-js/package.json"
bare_tarballs=()
for tarball in "$WORK/tarballs"/*.tgz; do
  case "$tarball" in *query-defaults*|*file-storage*) ;; *) bare_tarballs+=("$tarball") ;; esac
done
(cd "$WORK/bare-js" && npm install --ignore-scripts --no-audit --no-fund "${bare_tarballs[@]}" && node "$ROOT/extensions/query-defaults/tests/public/disabled.mjs" && node "$ROOT/extensions/file-storage/tests/public/disabled.mjs")
# A library of a newer ABI minor fails `define()` with both versions in the message.
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p monorm-extension-package -- package "$ROOT/tests/outside-extension" --out "$WORK/outside-newer" --tsc "$ROOT/languages/js/node_modules/.bin/tsc" --features abi-newer
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
