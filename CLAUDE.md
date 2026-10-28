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
. .venv/bin/activate && uv pip install -e .   # the thin `orm` package (languages/python/)
scripts/install-extension-packages.sh        # Python and npm pass packages, required by tracking/auth tests
(cd languages/python/packaging/tooling && maturin develop)   # the tooling native profile
python -m pytest -q               # Python end-to-end, SQL shape and typing tests
cargo test -q && cargo clippy -q -p orm-core -p orm-engine -p orm-python -p orm-node -p orm-contracts -p orm-extension-package
mypy --strict languages/python/orm && pyright languages/python/orm
cd languages/js && npm install && npm run build:native   # the TypeScript package (languages/js/), Node addon
npm test && npm run test:bun && npm run typecheck
cd ../..
uv pip install -e apps/auth/python
python -m pytest -q apps/auth/python/tests
python extensions/inherit/tests/public/python.py
python extensions/query-defaults/tests/public/python.py
(cd languages/js && node ../../extensions/inherit/tests/public/node.mjs)
(cd languages/js && node ../../extensions/query-defaults/tests/public/node.mjs)
(cd apps/auth/typescript && npm test && npm run test:bun)
scripts/check-extension-packages.sh  # needs uv, maturin, node and bun; fresh Python, Node and Bun consumers; PostgreSQL and SQLite
python3 -m unittest scripts/tests/test_check_extension_names.py && python3 scripts/check-extension-names.py  # host crates name no extension
cargo package -p orm-contracts      # the crate that extension authors get from crates.io
```

Regenerate the example models after changing code generation:
`cd examples/blog && python -m orm generate`, and for TypeScript
`cargo run -q -p orm-cli -- --schema examples/blog/schema.prisma generate typescript`
and the same with `-o languages/js/test/blog/models.ts --import ../../src/index.js`
(a test checks both are current).
