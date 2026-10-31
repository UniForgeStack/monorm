# Contributing

Thank you for your interest.
The project is under active development and the API can change.
Open an issue before you start a large change, so that we can agree on the design first.
To add a language, read [docs/contracts/language-bindings.md](docs/contracts/language-bindings.md) first.

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
uv venv .venv && . .venv/bin/activate && uv pip install maturin pytest 'pytest-asyncio>=0.24' mypy pyright   # the wheel builder and the test tools
python languages/python/packaging/stage.py   # the extension libraries of this platform, for the one wheel
maturin develop                   # the one `monorm` package: engine, built-in extensions and auth
python -m pytest -q               # Python end-to-end, SQL shape and typing tests
cargo test -q && cargo clippy -q -p monorm-core -p monorm-engine -p monorm-python -p monorm-node -p monorm -p monorm-extension-package
mypy --strict languages/python/monorm && pyright languages/python/monorm
cd languages/js && npm install && npm run assemble   # the TypeScript package (languages/js/): Node addon and built-in extension libraries
npm test && npm run test:bun && npm run typecheck
(cd create-monorm && npm test)   # the create-monorm package: bin.js into a temporary directory
cd ../..
python -m pytest -q apps/auth/python/tests
python extensions/inherit/tests/public/python.py
python extensions/query-defaults/tests/public/python.py
(cd languages/js && node ../../extensions/inherit/tests/public/node.mjs)
(cd languages/js && node ../../extensions/query-defaults/tests/public/node.mjs)
scripts/check-extension-packages.sh  # needs uv, maturin, node and bun; builds the one wheel and the one tarball and installs them in fresh Python, Node and Bun consumers; PostgreSQL and SQLite
scripts/check-init.sh  # needs the local `maturin develop` and `npm run assemble` builds, node and bun; `monorm init` on SQLite: a Python project runs, a TypeScript project runs on Node and Bun, `create-monorm` wraps it
python3 -m unittest scripts/tests/test_check_extension_names.py && python3 scripts/check-extension-names.py  # host crates name no extension
cargo package -p monorm      # the crate that extension authors get from crates.io
python3 scripts/rename/rename.py --check   # no old project name in the tree
python3 -m unittest scripts/tests/test_package_metadata.py   # publish metadata of every package
python3 -m unittest scripts/tests/test_check_glibc.py scripts/tests/test_release_versions.py   # glibc floor check and release scripts
MONORM_BENCH_SMOKE=1 python3 -m unittest scripts/tests/test_internal_bench.py   # bench/internal/run.sh --quick on SQLite: every case and path has a number, the equality check ran
python3 -m unittest scripts/tests/test_ci_affected.py   # the map from changed files to the CI check groups
python3 -m unittest scripts/tests/test_check_test_twins.py && python3 scripts/check-test-twins.py   # every Python test case has a TypeScript twin and the other way round, or an entry with a reason in languages/test-twins.toml
python3 -m unittest scripts/tests/test_check_thin_bindings.py && python3 scripts/check-thin-bindings.py   # no SQL statement text in the language bindings: the Rust core builds all SQL
python3 scripts/ci/check_links.py   # every relative link in a Markdown file points to a file in the tree
python3 scripts/release/versions.py check --allow-tbd   # every version site agrees with pyproject.toml
```

The commands install the thin `monorm` package, build the native libraries and run Python, Rust and TypeScript tests with type checks.
If a command needs a different PostgreSQL than the default, set `ORM_TEST_DATABASE_URL` (and `ORM_RUST_TEST_POSTGRES_URL` for `cargo test`).

### Generated example models

After you change code generation, regenerate the example models. A test checks that they are current.

```bash
cd examples/blog && python -m monorm generate
cargo run -q -p monorm-cli -- --schema examples/blog/schema.prisma generate typescript
cargo run -q -p monorm-cli -- --schema examples/blog/schema.prisma generate typescript -o languages/js/test/blog/models.ts --import ../../src/index.js
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
- [bench/RUNNING.md](bench/RUNNING.md): how to run the benchmarks; [bench/README.md](bench/README.md): the results.
