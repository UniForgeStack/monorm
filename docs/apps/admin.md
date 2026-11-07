# The admin

The admin is in progress.
It reads model lists for a superuser, and `monorm admin` serves them over HTTP.
It has no login, write, search, filter or count yet: until the login comes, every served API route answers 401.

The admin is one Rust crate (`monorm-admin`) with a thin module for each language.
The Python module is `monorm.admin`, and the TypeScript module is `monorm/admin`.
`schema.prisma` gets no admin attributes: the config lives in code.

## Register a model

`register` takes a model and a config.
A column is a field reference, so pyright, mypy and TypeScript check it.
A column through a to-one relation is one more column in the same statement.

```python
from monorm import admin

admin.register(Post, {"list": {"columns": [Post.title, Post.author.name], "order": [Post.created_at.desc()], "page_size": 25}})
admin.check()  # raises AdminConfigError; needs no database
```

```ts
import { admin } from "monorm/admin";

admin.register(Post, { list: { columns: [Post.title, Post.author.name], order: [Post.createdAt.desc()], pageSize: 25 } });
admin.check(); // throws AdminConfigError; needs no database
```

`check()` rejects an unknown field and a list column through a to-many relation.
The page size is 1 to 100, and the default is 25.
An order column must be a plain column: not nullable, not an array, and not Json, Decimal or an enum.
The primary key is always the last order column, so a keyset page has a stable order.

## Default resources

With no `register` call, every concrete model is a read-only resource.
After the first `register` call, only the registered models are resources.
A resource is named after its model.

## Test client

`test_client` (Python) and `testClient` (TypeScript) send a request to the router in memory.
No socket opens, and no port is used.

```python
client = admin.test_client(db, user={"is_superuser": True})
page = (await client.get("/admin/api/resources/Post")).json()
```

```ts
const client = admin.testClient(db, { user: { is_superuser: true } });
const page = (await client.get("/admin/api/resources/Post")).json();
```

The response has `status`, `text`, `queries` (the statements that the request ran) and `json()`.

## Routes and answers

The path prefix is a config value, and the default is `/admin/api`.

| Route | Answer |
|---|---|
| `GET {prefix}/resources` | The resources with their columns, order, page size and permissions (`["read"]`). |
| `GET {prefix}/resources/{name}?cursor=&limit=` | One page: `columns`, `rows` and `next`. |

A row is an array that follows `columns`.
`next` is a cursor for the next page, or `null` on the last page.
BigInt and Decimal are JSON strings.
A date and a time are ISO 8601 with an offset.

| Status | When |
|---|---|
| 401 | The request has no user. |
| 403 | The user is not a superuser (`is_superuser` is not `true`). |
| 404 | The resource or the path is unknown. |
| 400 | The cursor or the limit is not valid. |

An error body is `{"ok": false, "error": {"kind": "...", "message": "..."}}`.

## Start the admin

`monorm admin init` writes three things: the `[admin]` settings, an admin module that registers every model, and the `import "app:..."` line in `schema.prisma`.
It refuses to run when the settings already have an `admin` section or when the admin module exists.

```
python -m monorm admin init [--models MODULE]
npx monorm admin init [--models MODULE]
```

The Python module is `admin.py`, with `app = "admin:admin"`, and it imports the models from `models` (run `monorm generate` first, or pass `--models`).
The TypeScript module is `admin.ts`, with `app = "./admin.ts:admin"`, and it loads the schema itself unless you pass `--models`.
The schema import is `import "app:monorm-admin" (prefix: "admin_")` in Python and `import "app:admin" (prefix: "admin_")` in TypeScript.
It adds the tables `admin_adminlogentry` and `admin_userpreference`.
The admin schema imports nothing from `app:auth`, so the admin works with any user model.

After `makemigrations` and `migrate`, start the server with no flags:

```
python -m monorm admin
npx monorm admin
```

The standalone `monorm` binary cannot start the admin, because the admin module and the `setup` hook are your code.
It stops with a message that names the two commands above.

The server binds a socket only in this command.
`Admin.test_client` and `testClient` never open a port.

### Settings

The settings come from the same file as every other `monorm` command: `monorm.toml`, `[tool.monorm]` in `pyproject.toml`, or the `"monorm"` key in `package.json`.

```toml
[admin]
app = "admin:admin"          # module:name of the Admin object (required)
setup = "myproject.boot:setup"  # optional hook, sync or async
host = "127.0.0.1"
port = 8000
timeout = 30                 # seconds, for every request
db_settings = { "app.tenant" = "acme" }
```

| Key | Default | Meaning |
|---|---|---|
| `app` | none | Where to import the `Admin` object. A TypeScript value is a file path, such as `./admin.ts:admin`. |
| `setup` | none | A function that runs once, after the app import and before the server starts. |
| `host`, `port` | `127.0.0.1`, `8000` | The address. Port `0` picks a free port. |
| `timeout` | `30` | The request time limit in seconds. |
| `db_settings` | none | PostgreSQL settings that every admin transaction sets (see below). |

`MONORM_ADMIN_HOST` and `MONORM_ADMIN_PORT` override `host` and `port`.
An unknown key under `[admin]` is an error that names the key and the settings file.

The `setup` hook connects the database, the extensions and the file storage, with the same calls as any other `monorm` program.
When the hook did not connect a database, `monorm admin` connects with `MONORM_DATABASE_URL`.
If neither exists, it stops with `no database`.

The exit code is 0 after a clean stop, 2 for a wrong setting, and 1 for any other failure.
A failure includes a database without the admin tables: the message names the missing tables and says to run `makemigrations` and `migrate`.

### Database settings and row-level security

`db_settings` is a table of text values.
On PostgreSQL, the admin runs `set_config(key, value, true)` at the start of each request transaction for every entry.
A row-level security policy can read the value with `current_setting('app.tenant')`.
The value is static for the whole process.
SQLite has no such settings: the admin ignores the key and writes one message at startup.

### Health, logs and shutdown

| Route | Answer |
|---|---|
| `GET /healthz` | 200 when the process runs. It does not touch the database. |
| `GET /readyz` | 200 when `SELECT 1` answers within 2 seconds. Otherwise 503. It also answers 503 after a stop starts. |

Both routes sit at the root, outside the path prefix, and need no user.

Each request writes one JSON line to stderr, with a request id, the method, the path, the status and the time.
The response carries the same id in the `x-request-id` header.
The server takes the id from the request header when it has 1 to 128 printable ASCII characters. Otherwise it makes a new UUID.

The host stops the server on SIGTERM and SIGINT.
The server answers 503 on `/readyz`, stops to accept connections, lets the running requests end, and then the process exits with code 0.

### Timeout

One middleware applies the `timeout` setting to every request.
A request that is over the limit gets status 504 and the error kind `Timeout`.
The admin then rolls back the transaction of that request explicitly, so a cancelled request never leaves a transaction open.

The two languages differ in what the cancel stops:

- Python: the timeout cancels the coroutine of the request.
- Node: a Promise cannot be cancelled. The work of the request runs on, and the admin discards its result.

A Node `setup` hook or a long query in Node code can therefore use time after the client has its 504.

### Several processes

The admin keeps no state in the process: no session store, no cache and no queue.
Run as many processes as you need on one database, each with its own port, behind any load balancer.

### Kubernetes

Use the two health routes as probes, and let Kubernetes send SIGTERM.

```yaml
livenessProbe:  { httpGet: { path: /healthz, port: 8000 } }
readinessProbe: { httpGet: { path: /readyz, port: 8000 } }
```

Set `host = "0.0.0.0"` in the container, because the default `127.0.0.1` is not reachable from outside the pod.
Make `terminationGracePeriodSeconds` longer than `timeout`, so a request in progress can end before the kill.
The admin has no drain delay: if your load balancer needs a pause after SIGTERM, add a `preStop` sleep.
