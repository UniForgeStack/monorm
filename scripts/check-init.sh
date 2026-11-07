#!/usr/bin/env bash
# `monorm init` end to end on SQLite with the local build: a Python project runs its script,
# a TypeScript project runs on Node and on Bun, `create-monorm` wraps the same command,
# and `monorm admin init` and a start of `monorm admin` work in both projects.
# Needs `maturin develop` (the `monorm` package of $PYTHON), `npm run assemble` in languages/js, node, bun and npm.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PYTHON=${PYTHON:-python3}
JS=$ROOT/languages/js
mkdir -p "$ROOT/target"
WORK=$(mktemp -d "$ROOT/target/init-XXXXXX")
trap 'rm -rf "$WORK"' EXIT
unset MONORM_DATABASE_URL npm_config_user_agent

# Python: `python -m monorm init`, then the script of the project.
"$PYTHON" -m monorm init "$WORK/py-app" --lang python --tool pip --yes > "$WORK/py.out"
grep -q "python main.py" "$WORK/py.out"
(cd "$WORK/py-app" && "$PYTHON" main.py | tee "$WORK/py-run.out")
grep -q "Alice" "$WORK/py-run.out"

# monorm.toml is the settings file of the project: a moved schema is found through it, with no flag.
test -f "$WORK/py-app/monorm.toml"
mkdir "$WORK/py-app/db" && mv "$WORK/py-app/schema.prisma" "$WORK/py-app/db/"
printf 'schema = "db/schema.prisma"\n' > "$WORK/py-app/monorm.toml"
(cd "$WORK/py-app" && "$PYTHON" -m monorm check | tee "$WORK/py-check.out")
grep -q "db/schema.prisma: ok" "$WORK/py-check.out"

# Python with every built-in extension: the example must run, not only pass `check`.
"$PYTHON" -m monorm init "$WORK/py-all" --lang python --tool pip --ext all --yes > /dev/null
(cd "$WORK/py-all" && "$PYTHON" main.py | tee "$WORK/py-all-run.out")
grep -q "Alice" "$WORK/py-all-run.out"

# TypeScript: the CLI of the local package, with the dependency pointed at the local build.
node "$JS/dist/src/cli.js" init "$WORK/ts-app" --lang typescript --tool npm --yes > "$WORK/ts.out"
grep -q "npm start" "$WORK/ts.out"
node -e '
  const fs = require("fs");
  const file = process.argv[1] + "/package.json";
  const pkg = JSON.parse(fs.readFileSync(file, "utf8"));
  if (!/^npm:@ufstack\/monorm@\^/.test(pkg.dependencies.monorm)) throw new Error("unexpected dependency " + pkg.dependencies.monorm);
  pkg.dependencies.monorm = "file:" + process.argv[2];
  fs.writeFileSync(file, JSON.stringify(pkg, null, 2));
' "$WORK/ts-app" "$JS"
(cd "$WORK/ts-app" && npm install --no-audit --no-fund --silent)
(cd "$WORK/ts-app" && node index.ts | tee "$WORK/node-run.out")
grep -q "Alice" "$WORK/node-run.out"
(cd "$WORK/ts-app" && rm -f dev.db && bun index.ts | tee "$WORK/bun-run.out")
grep -q "Alice" "$WORK/bun-run.out"

# The same for TypeScript: monorm.toml, not package.json, holds the settings.
test -f "$WORK/ts-app/monorm.toml"
mkdir "$WORK/ts-app/db" && mv "$WORK/ts-app/schema.prisma" "$WORK/ts-app/db/"
printf 'schema = "db/schema.prisma"\n' > "$WORK/ts-app/monorm.toml"
(cd "$WORK/ts-app" && node "$JS/dist/src/cli.js" check | tee "$WORK/ts-check.out")
grep -q "db/schema.prisma: ok" "$WORK/ts-check.out"

# TypeScript with every built-in extension: the project installs the packed tarball, because a `file:` link
# makes the extension libraries resolve outside the project. The example must run, not only pass `check`.
(cd "$JS" && npm pack --pack-destination "$WORK" --silent > "$WORK/pack.out")
node "$JS/dist/src/cli.js" init "$WORK/ts-all" --lang typescript --tool npm --ext all --yes > /dev/null
node -e '
  const fs = require("fs");
  const file = process.argv[1] + "/package.json";
  const pkg = JSON.parse(fs.readFileSync(file, "utf8"));
  pkg.dependencies.monorm = "file:" + process.argv[2];
  fs.writeFileSync(file, JSON.stringify(pkg, null, 2));
' "$WORK/ts-all" "$WORK/$(tail -n 1 "$WORK/pack.out")"
(cd "$WORK/ts-all" && npm install --no-audit --no-fund --silent)
(cd "$WORK/ts-all" && node index.ts | tee "$WORK/node-all-run.out")
grep -q "Alice" "$WORK/node-all-run.out"
(cd "$WORK/ts-all" && rm -f dev.db && bun index.ts | tee "$WORK/bun-all-run.out")
grep -q "Alice" "$WORK/bun-all-run.out"

# `monorm admin init`, then a start of `monorm admin` on SQLite: health answers and SIGTERM stops it with exit 0.
admin_probe() { # name, then the command that starts the server (the project is the current directory)
  local name=$1 pid url
  shift
  MONORM_ADMIN_PORT=0 MONORM_DATABASE_URL="sqlite://$WORK/$name.db" "$@" 2> "$WORK/$name.err" &
  pid=$!
  for _ in $(seq 150); do
    grep -q '"listening"' "$WORK/$name.err" && break
    kill -0 "$pid" 2> /dev/null || break
    sleep 0.2
  done
  url=$(sed -n 's/.*"url":"\([^"]*\)".*/\1/p' "$WORK/$name.err" | head -n 1)
  if [ -z "$url" ]; then cat "$WORK/$name.err" >&2; kill "$pid" 2> /dev/null || true; return 1; fi
  curl -fsS "$url/healthz" > /dev/null
  curl -fsS "$url/readyz" > /dev/null
  kill -TERM "$pid"
  wait "$pid"
}
(
  cd "$WORK/py-app"
  "$PYTHON" -m monorm admin init --models db.models > "$WORK/py-admin-init.out"
  grep -q "admin:admin" monorm.toml
  "$PYTHON" -m monorm generate > /dev/null
  "$PYTHON" -m monorm makemigrations > /dev/null
  "$PYTHON" -m monorm migrate --url "sqlite://$WORK/py-admin.db" > /dev/null
  admin_probe py-admin "$PYTHON" -m monorm admin
)
(
  cd "$WORK/ts-app"
  node "$JS/dist/src/cli.js" admin init > "$WORK/ts-admin-init.out"
  grep -q "./admin.ts:admin" monorm.toml
  node "$JS/dist/src/cli.js" makemigrations > /dev/null
  node "$JS/dist/src/cli.js" migrate --url "sqlite://$WORK/ts-admin.db" > /dev/null
  admin_probe ts-admin node "$JS/dist/src/cli.js" admin
)

# create-monorm: the thin wrapper writes the same TypeScript project, with the Bun commands under bun.
mkdir -p "$WORK/wrapper/node_modules/@ufstack"
ln -s "$JS" "$WORK/wrapper/node_modules/@ufstack/monorm"
cp "$JS/create-monorm/bin.js" "$WORK/wrapper/bin.js"
echo '{"type":"module"}' > "$WORK/wrapper/package.json"
npm_config_user_agent="bun/1.2.0 npm/? node/v24.0.0" node "$WORK/wrapper/bin.js" "$WORK/wrapped" > "$WORK/wrapped.out"
grep -q "bun install" "$WORK/wrapped.out"
grep -q '"start": "bun index.ts"' "$WORK/wrapped/package.json"
echo "monorm init: ok"
