# The admin

The admin is in progress.
Today it reads model lists for a superuser, with no server and no login.
It has no write, search, filter or count yet.

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
