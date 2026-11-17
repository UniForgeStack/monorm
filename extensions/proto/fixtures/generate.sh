#!/bin/sh
# Rebuild the generated fixture classes. Needs protoc, and node with network access for the two plugins.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
tools=$(mktemp -d)
trap 'rm -rf "$tools"' EXIT
(cd "$tools" && npm init -y >/dev/null && npm i -D @bufbuild/protoc-gen-es@2 ts-proto@2 >/dev/null)
py="$root/languages/python/tests/proto_gen"
es="$root/languages/js/test/proto/gen"
tsp="$root/languages/js/test/proto/gen-ts-proto"
rm -rf "$py" "$es" "$tsp"
mkdir -p "$py" "$es" "$tsp"
cd "$here"
protoc -I . --python_out="$py" --pyi_out="$py" acme/note.proto acme/billing/invoice.proto
PATH="$tools/node_modules/.bin:$PATH" protoc -I . --es_out="$es" --es_opt=target=ts,import_extension=js acme/note.proto acme/billing/invoice.proto
PATH="$tools/node_modules/.bin:$PATH" protoc -I . --ts_proto_out="$tsp" --ts_proto_opt=importSuffix=.js,esModuleInterop=true acme/note.proto
