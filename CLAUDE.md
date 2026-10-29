# Working on this repo

Outside contributors: read [CONTRIBUTING.md](CONTRIBUTING.md). It has the same checks (keep the two lists in step) and the PR rules.

## Git

- Commit directly on `main` and push to `origin main`. No feature branches or pull
  requests unless asked for one.
- Before committing, run the checks below and keep them green.

## Tests

- Always prefer bulk database actions in tests and fixtures to reduce database
  round trips: batch inserts with `insert_many()` / `insertMany()`, and use bulk
  updates or deletes where applicable. Use individual operations when their
  behavior is what the test verifies or later operations depend on earlier results.

## Checks

```bash
service postgresql start          # the test database (postgres:postgres@localhost/orm_test)
. .venv/bin/activate && uv pip install maturin pytest 'pytest-asyncio>=0.24' mypy pyright   # the wheel builder and the test tools
python languages/python/packaging/stage.py   # the extension libraries of this platform, for the one wheel
maturin develop                   # the one `monorm` package: engine, built-in extensions and auth
python -m pytest -q               # Python end-to-end, SQL shape and typing tests
cargo test -q && cargo clippy -q -p monorm-core -p monorm-engine -p monorm-python -p monorm-node -p monorm -p monorm-extension-package
mypy --strict languages/python/monorm && pyright languages/python/monorm
cd languages/js && npm install && npm run assemble   # the TypeScript package (languages/js/): Node addon and built-in extension libraries
npm test && npm run test:bun && npm run typecheck
cd ../..
python -m pytest -q apps/auth/python/tests
python extensions/inherit/tests/public/python.py
python extensions/query-defaults/tests/public/python.py
(cd languages/js && node ../../extensions/inherit/tests/public/node.mjs)
(cd languages/js && node ../../extensions/query-defaults/tests/public/node.mjs)
scripts/check-extension-packages.sh  # needs uv, maturin, node and bun; builds the one wheel and the one tarball and installs them in fresh Python, Node and Bun consumers; PostgreSQL and SQLite
python3 -m unittest scripts/tests/test_check_extension_names.py && python3 scripts/check-extension-names.py  # host crates name no extension
cargo package -p monorm      # the crate that extension authors get from crates.io
python3 scripts/rename/rename.py --check   # no old project name in the tree
python3 -m unittest scripts/tests/test_package_metadata.py   # publish metadata of every package
python3 -m unittest scripts/tests/test_check_glibc.py scripts/tests/test_release_versions.py   # glibc floor check and release scripts
python3 scripts/release/versions.py check --allow-tbd   # every version site agrees with pyproject.toml
```

Regenerate the example models after changing code generation:
`cd examples/blog && python -m monorm generate`, and for TypeScript
`cargo run -q -p monorm-cli -- --schema examples/blog/schema.prisma generate typescript`
and the same with `-o languages/js/test/blog/models.ts --import ../../src/index.js`
(a test checks both are current).
