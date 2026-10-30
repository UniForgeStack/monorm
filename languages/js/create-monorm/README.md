# create-monorm

Start a [MonOrm](https://github.com/UniForgeStack/monorm) project in one command.

```bash
npm create monorm my-app
# or
bun create monorm my-app
```

The command runs `monorm init` from the [`@ufstack/monorm`](https://www.npmjs.com/package/@ufstack/monorm) package, with the TypeScript language and the package manager that you used (npm, pnpm, yarn or bun).
In a terminal it asks for the database (SQLite or PostgreSQL) and the built-in extensions.
Other arguments go to `monorm init`: `--db`, `--url`, `--ext a,b` and `--yes`.
Without a terminal, it uses the defaults and does not ask.

The project holds a `schema.prisma` file with two models, an `index.ts` script, `package.json`, `.gitignore` and `README.md`.
The script creates the tables, inserts a user with two posts, and reads the users that have a published post.

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
The command refuses a directory that is not empty.
`npm create monorm` downloads the native addon of `@ufstack/monorm`, because it runs the same code as `npx monorm init`.
For Python, run `uvx monorm init my-app`.

## What is MonOrm

MonOrm is an async ORM for TypeScript and Python with one Rust core.
It reads the schema from a `schema.prisma` file and runs on PostgreSQL and SQLite.
It is independent of Prisma and is not a drop-in replacement for Prisma Client.
Install it in an existing project: `npm install monorm@npm:@ufstack/monorm`.
For Python: `pip install monorm`.

## License

MIT
