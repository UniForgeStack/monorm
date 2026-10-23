# orm

An async ORM for Python and TypeScript with one Rust core.
You write the schema once in Prisma syntax (`schema.prisma`).
The Rust core compiles it, plans the SQL and runs it on PostgreSQL or SQLite.
Both languages get the same queries, the same migrations and generated, typed models.

```python
users = await User.objects.filter(User.posts.created_at < yesterday).prefetch_related(User.posts)
```

```ts
const users = await User.objects.filter(User.posts.createdAt.lt(yesterday)).prefetchRelated(User.posts);
```

Optional features (proxy models, schema query defaults, model composition, file fields, generic relations) are build-time extensions: Rust crates that you select when you build the native library.

## Start

```bash
uv venv .venv && . .venv/bin/activate
uv pip install -e '.[dev]' maturin
(cd languages/python/packaging/tooling && maturin develop)      # the native library (tooling profile)
python examples/blog/demo.py sqlite://:memory:

cd languages/js && npm install && npm run build:native && npm run build
```

Released packages need one native profile: `pip install 'orm[postgres]'`, `orm[sqlite]`, `orm[combined]`, `orm[tooling]` or `orm[full]` (tooling plus every first-party extension).

## Where to read next

- [docs/README.md](docs/README.md): the documentation index (core, extensions, performance).
- [examples/blog](examples/blog): a schema, its generated models and migrations, and runnable demos.
- [CLAUDE.md](CLAUDE.md): the checks to run before a commit.
