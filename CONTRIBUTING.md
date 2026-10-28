# Contributing

Thank you for your interest.
The project is under active development and the API can change.
Open an issue before you start a large change, so that we can agree on the design first.

## Support

Use GitHub issues for bugs and feature requests (the forms ask for what we need).
The project is maintained on a best-effort basis: no response time is promised.
Report a security problem in private: see [SECURITY.md](SECURITY.md).

## Prerequisites

- Rust (stable), [uv](https://docs.astral.sh/uv/), Python 3.11 to 3.14, Node 20.19+ or 22.12+, and [Bun](https://bun.sh).
- PostgreSQL for the test database (`postgres:postgres@localhost/orm_test`). SQLite needs no setup.
- `maturin` for the Python native library (installed in the commands below).

## Setup and checks

Run these from the repository root. Keep them green before you open a pull request.

```bash
# Start PostgreSQL first (Linux: `service postgresql start`; macOS: your own server) with the test database above.
uv venv .venv && . .venv/bin/activate && uv pip install -e '.[dev]' maturin   # the thin `orm` package (languages/python/)
scripts/install-extension-packages.sh        # Python and npm pass packages, required by tracking/auth tests
(cd languages/python/packaging/tooling && maturin develop)   # the tooling native profile
python -m pytest -q               # Python end-to-end, SQL shape and typing tests
cargo test -q && cargo clippy -q -p orm-core -p orm-engine -p orm-python -p orm-node -p orm-contracts -p orm-extension-build -p orm-extension-package
mypy --strict languages/python/orm && pyright languages/python/orm
cd languages/js && npm install && npm run build:native   # the TypeScript package (languages/js/), Node addon
npm test && npm run test:bun && npm run typecheck
cd ../..
uv pip install -e apps/auth/python
python -m pytest -q apps/auth/python/tests
python extensions/inherit/tests/public/python.py
(cd languages/js && node ../../extensions/inherit/tests/public/node.mjs)
(cd apps/auth/typescript && npm test && npm run test:bun)
scripts/check-extension-packages.sh  # needs uv, maturin, node and bun; fresh Python, Node and Bun consumers; PostgreSQL and SQLite
```

The commands install the thin `orm` package, build the native libraries and run Python, Rust and TypeScript tests with type checks.
If a command needs a different PostgreSQL than the default, set `ORM_TEST_DATABASE_URL` (and `ORM_RUST_TEST_POSTGRES_URL` for `cargo test`).

### Feature artifacts

Optional features are build-time extensions.
`scripts/feature-check.sh` builds one native artifact with all extensions and runs the tests on it.
Run it when you change an extension crate, `crates/contracts/`, `crates/extension-build/` or feature-gated host code.
It needs `uv`, `node`, `npm` and `bun`, and takes a long time. CI runs it on every pull request.

```bash
PYTHON=python3.14 ORM_TEST_DATABASE_URL=postgres://postgres:postgres@localhost/orm_test \
  scripts/feature-check.sh            # or name the artifact: scripts/feature-check.sh all
```

### Generated example models

After you change code generation, regenerate the example models. A test checks that they are current.

```bash
cd examples/blog && python -m orm generate
cargo run -q -p orm-cli -- --schema examples/blog/schema.prisma generate typescript
cargo run -q -p orm-cli -- --schema examples/blog/schema.prisma generate typescript -o languages/js/test/blog/models.ts --import ../../src/index.js
```

## Pull requests

- One concern for each pull request. Keep the change small enough to review at once.
- Add or change tests for every change in behavior. A bug fix gets a test that fails without the fix.
- In tests and fixtures, prefer bulk database actions (`insert_many()` / `insertMany()`, bulk updates and deletes) to reduce round trips. Use single operations when the test verifies them.
- Update the docs in `docs/` when you change the public API or the schema language.
- Fill in the pull request template: what and why, which checks you ran.

## Commit messages

A short title (imperative, about 70 characters), then a body of at most 10 lines that says why.
One commit holds one concept. Squash fixup commits before you push.

## Where to read

- [docs/README.md](docs/README.md): the documentation index.
- [examples/quickstart](examples/quickstart): the first query, in Python and TypeScript.
- [bench/README.md](bench/README.md): how to run the benchmarks.
