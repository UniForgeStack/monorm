# MonOrm for Python

An async ORM for Python with one Rust core.
The package is `monorm` on PyPI.
The Rust core plans the SQL and runs it on PostgreSQL or SQLite.
This directory holds the Python layer: the `monorm` package, the native crate and the packaging scripts.
The Python layer converts types, maps errors and drives the async runtime.
It holds no SQL text (`scripts/check-thin-bindings.py` checks this).

## Installation

```bash
pip install monorm
```

Python 3.11 to 3.14 is supported.

## Usage

Write the schema in Prisma syntax, load it, connect and query:

```python
import asyncio
import monorm

SCHEMA = """
datasource db {
  provider = "sqlite"
}
model User {
  id   Int @id @default(autoincrement())
  name String
}
"""


async def main():
    User = monorm.loads(SCHEMA)["User"]
    db = await monorm.connect("sqlite://:memory:")
    try:
        await db.create_tables()
        await User.objects.insert(name="Alice")
        print([user.name for user in await User.objects.all()])
    finally:
        await db.close()


asyncio.run(main())
```

A full program that runs on in-memory SQLite is [examples/quickstart/quickstart.py](../../examples/quickstart/quickstart.py).
A larger project with generated models and migrations is [examples/blog](../../examples/blog).
The API is in [docs/api/python-api.md](../../docs/api/python-api.md).
Breaking changes are in [upgrade.md](upgrade.md).

## Development

Run these commands from the repository root.
You need Rust, [uv](https://docs.astral.sh/uv/) and PostgreSQL for the test database.

```bash
uv venv .venv && . .venv/bin/activate && uv pip install maturin pytest 'pytest-asyncio>=0.24' mypy pyright
python languages/python/packaging/stage.py   # the extension libraries of this platform
maturin develop                              # the native library and the package, in the venv
```

| Path | Content |
|---|---|
| `monorm/` | The Python package |
| `native/` | The Rust crate `monorm-python`: it calls the engine |
| `packaging/` | The wheel scripts |
| `tests/` | The Python tests |

## Testing

```bash
python -m pytest -q
mypy --strict languages/python/monorm && pyright languages/python/monorm
cargo test -q -p monorm-python && cargo clippy -q -p monorm-python
python3 scripts/check-thin-bindings.py
```

The tests need `postgres:postgres@localhost/orm_test`, or set `ORM_TEST_DATABASE_URL`.
Each Python test has a TypeScript twin: see `scripts/check-test-twins.py`.
