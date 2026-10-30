# From Prisma Client Python

Prisma Client Python has had no maintainer since March 2025 ([prisma-client-py#1073](https://github.com/RobertCraigie/prisma-client-py/issues/1073)).
MonOrm reads the same `schema.prisma` language, so a project can keep its schema and change the client.
MonOrm is independent of Prisma and is not a drop-in replacement: the query API is different.
This guide shows the schema changes and the query API side by side.
The MonOrm snippets ran on SQLite (measured, 2026-10-30, the worktree build of 0.1.1).

## The schema

Keep the file.
A `generator client { provider = "prisma-client-py" }` block and `url = env("DATABASE_URL")` load without an error.
MonOrm ignores the generator block, and it ignores `url`: the URL comes from `monorm.connect(url)` (or `MONORM_DATABASE_URL` for the CLI).

| Prisma Client Python schema | MonOrm |
|---|---|
| `@default(cuid())` | Not supported. Use `@default(uuid())` or `@client_default(uuid7())` on a `String` id. |
| `@default(uuid())` | Same. |
| `DateTime @updatedAt` | Not supported. Use `@@timestamps.updated_at("updated_at")` (the `timestamps` extension, [schema-extensions.md](../extensions/schema-extensions.md)). |
| `Bytes` | Not supported. |
| `enum` on SQLite | Add `@@storage(text)` or `@@storage(int)` to the enum. PostgreSQL uses a native enum with no change. |
| `Decimal`, `String[]` on SQLite | Not supported on SQLite. They work on PostgreSQL. |
| `autoincrement()`, `@@id`, `@@unique`, `@@index`, `@@map`, `@map`, `@relation(...)`, `onDelete`, `Json`, `@db.*` | The same. |

`cuid()` fails when the schema loads: `User.id: cuid() is not supported; use @client_default(uuid()) or @client_default(uuid7())` (measured).
Existing rows keep their `cuid` values: a `String` column holds them, and only new rows get a UUID.

```prisma
generator client {
  provider = "prisma-client-py"
}

datasource db {
  provider = "sqlite"
  url      = env("DATABASE_URL")
}

model User {
  id    String @id @default(uuid())
  name  String
  posts Post[]
}

model Post {
  id        String  @id @default(uuid())
  title     String
  views     Int     @default(0)
  published Boolean @default(false)
  author_id String
  author    User    @relation(fields: [author_id], references: [id])
}
```

Prisma Client Python generates the client with `prisma generate`.
MonOrm loads the schema when the program starts, with no generation step:

```python
import monorm

models = monorm.load("schema.prisma")
User, Post = models["User"], models["Post"]
db = await monorm.connect("sqlite://:memory:")
await db.create_tables()
```

For typed models, run `python -m monorm generate` to write `models.py` and `models.pyi` ([schema.md](../schema/schema.md)).
Use `python -m monorm makemigrations` and `migrate` in place of `prisma migrate`, or `create_tables()` in a test.
`python -m monorm pull` writes a schema file from a live database.

## Queries

Each Prisma call below is the one in the Prisma Client Python documentation (recalled, not run).
`db` is a connected `Prisma()` client in the first column and a `monorm` client in the second.

### Connect

```python
# Prisma Client Python
db = Prisma()
await db.connect()
...
await db.disconnect()
```

```python
# MonOrm
db = await monorm.connect("sqlite://:memory:")
...
await db.close()
```

### Create

```python
# Prisma Client Python
alice = await db.user.create(data={"name": "Alice"})
await db.post.create_many(data=[
    {"title": "Hello", "published": True, "views": 12, "author_id": alice.id},
    {"title": "Draft", "published": False, "author_id": alice.id},
])
```

```python
# MonOrm
alice = await User.objects.insert(name="Alice")
await Post.objects.insert_many([
    {"title": "Hello", "published": True, "views": 12, "author": alice},
    {"title": "Draft", "published": False, "author": alice},
])
```

`author=alice` and `author_id=alice.id` both work.

### Find many with a filter

```python
# Prisma Client Python
posts = await db.post.find_many(
    where={"published": True, "views": {"gt": 10}},
    order={"views": "desc"},
)
```

```python
# MonOrm
posts = await Post.objects.filter(Post.published, Post.views > 10).order_by(-Post.views)
```

Prisma's `where` is a dict with operator keys.
MonOrm filters are Python expressions on the model columns: `==`, `<`, `>=`, `.in_()`, `.contains()`, `.is_null()`.
`filter(a, b)` combines the conditions with AND.
Each query set is lazy, and `await` runs it.
`find_unique` becomes `await Post.objects.get(Post.id == post_id)`, which gives `None` when no row matches.

A filter on a relation uses the same expression style.
`where={"posts": {"some": {"published": True}}}` becomes `User.objects.filter(User.posts.published)`.

### Include: `load()`

```python
# Prisma Client Python
users = await db.user.find_many(
    where={"posts": {"some": {"published": True}}},
    include={"posts": True},
)
for user in users:
    print(user.name, [post.title for post in user.posts])
```

```python
# MonOrm
users = await User.objects.filter(User.posts.published).load(User.posts)
for user in users:
    print(user.name, [post.title for post in user.posts.cached])
```

`load()` reads the related rows in one more query, not one query for each user.
The filter picks the users; `load()` brings all their posts (measured: the unpublished post also came back).
To load only some posts, pass a filtered query set: `.load(User.posts.objects.filter(Post.published))` (measured: only the published post came back).
An instance can also load later: `await post.load(Post.author)` ([loading-instances.md](loading-instances.md)).

### Update

```python
# Prisma Client Python
await db.post.update(where={"id": post.id}, data={"title": "Hello again"})
await db.post.update_many(where={"title": "Draft"}, data={"published": True})
```

```python
# MonOrm
await post.update(title="Hello again")
await Post.objects.filter(Post.title == "Draft").update(published=True)   # returns the row count
```

A value can be an expression: `.update(views=Post.views + 1)`, like Prisma's `{"increment": 1}`.

### Delete

```python
# Prisma Client Python
await db.post.delete(where={"id": post.id})
await db.post.delete_many(where={"title": "Draft"})
```

```python
# MonOrm
await post.delete()
await Post.objects.filter(Post.title == "Draft").delete()
```

### Transaction

```python
# Prisma Client Python
async with db.tx() as tx:
    await tx.post.create(data={"title": "Kept", "author_id": alice.id})
```

```python
# MonOrm
async with db.transaction():
    await Post.objects.insert(title="Kept", author=alice)
```

The transaction is a `ContextVar`: queries inside the block use it with no `tx` object.
An exception rolls back the block, and a nested block is a savepoint (measured: a rolled-back insert left no row).

## What the two clients do not share

- Method names are in `snake_case` in both, but MonOrm has no `find_first`, `find_unique` or `upsert` with a `where` dict. Use `first()`, `get()` and `on_conflict(...)`.
- Prisma Client Python also has a sync interface. The MonOrm Python API is async.
- MonOrm has no `prisma` import and no `prisma generate` step. Remove both from the project.
- MonOrm returns model instances, not Pydantic models. `to_dict()` gives a dict.

See [python-api.md](../api/python-api.md) for the full API and [prisma-syntax.md](../schema/prisma-syntax.md) for the schema language.
