# create-monorm

Start a [MonOrm](https://github.com/UniForgeStack/monorm) project in one command.

```bash
npm create monorm my-app
# or
bun create monorm my-app
```

The command copies a small project into `my-app`.
It holds a `schema.prisma` file with two models and an `index.ts` script.
The script creates the tables on SQLite, inserts a user with two posts, and reads the users that have a published post.

```bash
cd my-app
npm install
npm start
```

Expected output:

```
Alice [ 'Hello', 'Draft' ]
```

`npm start` runs `node index.ts`, which needs Node 22.18 or newer (Node strips the types).
With `bun create`, the project uses `bun install` and `bun start`, and `bun start` runs `bun index.ts`.
Each run adds one more user to `dev.db`.

## What is MonOrm

MonOrm is an async ORM for TypeScript and Python with one Rust core.
It reads the schema from a `schema.prisma` file and runs on PostgreSQL and SQLite.
It is independent of Prisma and is not a drop-in replacement for Prisma Client.
Install it in an existing project: `npm install monorm@npm:@ufstack/monorm`.
For Python: `pip install monorm`.

## License

MIT
