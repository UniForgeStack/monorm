# MonOrm for TypeScript

An async ORM for Node and Bun with one Rust core.
You write the schema once in Prisma syntax.
The Rust core plans the SQL and runs it on PostgreSQL or SQLite.
The Python package uses the same engine, schema and queries.

> **Under active development.**
> It works today, and the author runs it in production.
> Other people should not use it in production yet, or do so at their own risk.
> Evaluate it against your own app before you rely on it.

**About 1.90× faster end to end than Prisma on Node (geometric mean of 20 cases, up to 11.68× faster).**
On Bun, the three measured cases are 1.61× to 3.61× faster than Prisma.
"End to end" means the database round trip is included.
Measured on 2026-10-28 (run `d428e01`), one run on a shared cloud VM with PostgreSQL 16.15.
Against Drizzle (geometric mean 1.50× faster) it is slower or level in a few cases, for example `tx-5` (0.97×, slower).
Source, method and limits: [bench/README.md](https://github.com/UniForgeStack/monorm/blob/main/bench/README.md#speed-up-summary).
Run the comparison yourself before you decide.

The name is MonOrm.

## Installation

Install the package under the alias `monorm`, because the code and the generated models import `monorm`:

```bash
npm install monorm@npm:@ufstack/monorm      # pnpm add monorm@npm:@ufstack/monorm; bun add monorm@npm:@ufstack/monorm
```

A plain `npm install @ufstack/monorm` installs, but the imports fail.

## Usage

There is no registry install line yet. Build from source (you need Rust and Node 20.19+ or 22.12+, or Bun):

```bash
git clone https://github.com/UniForgeStack/monorm.git && cd monorm
cd languages/js && npm install && npm run build:native && npm run build
cd ../../examples/quickstart && npm install && node quickstart.ts   # Node 23.6 or later
```

The example in `examples/quickstart/quickstart.ts` (in-memory SQLite):

```ts
// Build languages/js first; then npm install and node quickstart.ts (Node 23.6+).
import { connect, loads } from "monorm";

const url = process.argv[2] ?? "sqlite://:memory:";
const provider = url.startsWith("sqlite:") ? "sqlite" : "postgresql";
const { User, Post } = loads(`datasource db {
  provider = "${provider}"
}
model User {
  id    Int @id @default(autoincrement())
  name  String
  posts Post[]
}
model Post {
  id        Int @id @default(autoincrement())
  title     String
  published Boolean
  author_id Int
  author    User @relation(fields: [author_id], references: [id])
}
`);
const db = await connect(url);
try {
  await db.createTables();
  const alice = await User.objects.insert({ name: "Alice" });
  await Post.objects.insertMany([
    { title: "Hello", published: true, author: alice },
    { title: "Draft", published: false, author: alice },
  ]);
  const users = await User.objects.filter(User.posts.published).load(User.posts);
  for (const user of users) {
    console.log(user.name, user.posts.cached.map((post) => post.title));
  }
} finally {
  await db.close();
}
```

The API is in the [TypeScript API](https://github.com/UniForgeStack/monorm/blob/main/docs/api/typescript-api.md).
A larger project with generated models and migrations is [examples/blog](https://github.com/UniForgeStack/monorm/tree/main/examples/blog).
Breaking changes are in [upgrade.md](https://github.com/UniForgeStack/monorm/blob/main/languages/js/upgrade.md).

## Features

- Prisma schema syntax ([docs](https://github.com/UniForgeStack/monorm/blob/main/docs/schema/prisma-syntax.md)).
- A Django-style model API: `Model.objects`, `filter()`, `load()` and migrations from the schema.
- Generated, typed models.
- Queries, pagination, subqueries, windows, CTEs, writes, transactions and locks ([TypeScript API](https://github.com/UniForgeStack/monorm/blob/main/docs/api/typescript-api.md)).
- Build-time extensions: proxy models, query defaults, model composition, file storage, generic relations, timestamps, soft delete, locking and inheritance ([extension index](https://github.com/UniForgeStack/monorm/blob/main/docs/README.md#feature-extensions)).

The full feature table is in the [root README](https://github.com/UniForgeStack/monorm/blob/main/README.md#features).

## Runtimes and databases

| | |
|---|---|
| Runtimes | Node 20.19+ or 22.12+, and Bun |
| Databases | PostgreSQL and SQLite |

The API is async only. There is no MySQL driver.

## Development

Run these commands in `languages/js`.
You need Rust and Node 20.19+ or 22.12+; Bun is for the Bun tests.
This package converts types, maps errors and drives the async runtime.
It holds no SQL text (`scripts/check-thin-bindings.py` checks this).

```bash
npm install
npm run assemble   # the Node addon and the built-in extension libraries
npm run build      # the TypeScript source into dist/
```

| Path | Content |
|---|---|
| `src/` | The TypeScript package |
| `native/` | The Rust crate `monorm-node`: it calls the engine |
| `packaging/` | The npm package scripts |
| `test/` | The TypeScript tests |

## Testing

```bash
npm test && npm run test:bun && npm run typecheck
(cd create-monorm && npm test)
cargo test -q -p monorm-node && cargo clippy -q -p monorm-node
```

The tests need `postgres:postgres@localhost/orm_test`, or set `ORM_TEST_DATABASE_URL`.
Each TypeScript test has a Python twin: see `scripts/check-test-twins.py`.

## Links

- [TypeScript API](https://github.com/UniForgeStack/monorm/blob/main/docs/api/typescript-api.md)
- [Schema](https://github.com/UniForgeStack/monorm/blob/main/docs/schema/schema.md)
- [Extensions](https://github.com/UniForgeStack/monorm/blob/main/docs/README.md#feature-extensions)
- [Root README](https://github.com/UniForgeStack/monorm/blob/main/README.md)
- [Changelog](https://github.com/UniForgeStack/monorm/blob/main/CHANGELOG.md)

## License

MIT.
