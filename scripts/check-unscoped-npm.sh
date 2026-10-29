#!/usr/bin/env bash
# Pack `monorm` (languages/js/unscoped) and `@ufstack/monorm`, install both in a fresh project, import `monorm` and run `npx monorm --help`.
# Needs a built core (`cd languages/js && npm run build`); builds the tooling native profile when `target/node-profiles` has none.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
platform="$(node -p 'process.platform + "-" + process.arch')"
profile="$root/target/node-profiles/tooling-$platform"
[ -f "$profile/monorm.node" ] || (cd "$root" && node languages/js/packaging/build-node-profile.mjs tooling)
mkdir "$work/tgz" "$work/app"
(cd "$profile" && npm pack --silent --pack-destination "$work/tgz")
(cd "$root/languages/js/packaging/tooling" && npm pack --silent --pack-destination "$work/tgz")
(cd "$root/languages/js" && npm pack --silent --ignore-scripts --pack-destination "$work/tgz")
(cd "$root/languages/js/unscoped" && npm pack --silent --pack-destination "$work/tgz")
cd "$work/app"
echo '{"type":"module","private":true}' > package.json
npm install --silent --no-audit --no-fund --ignore-scripts "$work"/tgz/*.tgz
[ "$(find node_modules -name package.json -path '*monorm/package.json' | wc -l)" -eq 2 ] # the core and the thin package, one copy each
node --input-type=module -e '
import * as thin from "monorm";
import * as core from "@ufstack/monorm";
import * as thinExt from "monorm/ext";
import * as coreExt from "@ufstack/monorm/ext";
import * as thinOtel from "monorm/otel";
import * as coreOtel from "@ufstack/monorm/otel";
for (const [a, b] of [[thin, core], [thinExt, coreExt], [thinOtel, coreOtel]]) {
  const names = Object.keys(b);
  if (!names.length || names.some((n) => a[n] !== b[n])) throw new Error("export mismatch");
}'
npx --no-install monorm --help > help.txt
grep -qi usage help.txt
echo "unscoped monorm: imports and CLI ok"
