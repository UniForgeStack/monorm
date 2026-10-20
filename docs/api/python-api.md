# Python API

The query style is SQLAlchemy's: typed expressions over model attributes. The model
layer is Django's: `Model.objects` managers, `load()` (Django's `only` / `select_related` / `prefetch_related`),
`DoesNotExist` per model. Everything is async. Queries compile to the ORM IR in Python
and cross into Rust once per operation. Rust plans the SQL with sea-query and runs it
with its own drivers (tokio-postgres for PostgreSQL, rusqlite for SQLite; see [Drivers and dialects](#drivers-and-dialects)).

```python
yesterday = datetime.now(timezone.utc) - timedelta(days=1)
users = await User.objects.filter(User.posts.created_at < yesterday)
```

`User.posts.created_at` autocompletes and type-checks as `ColumnRef[datetime]`. Through
the `.pyi` stub, `User.posts` is the path class of `Post`. The filter joins
automatically: each relation hop becomes a correlated `EXISTS`.

Samples use the blog models (`examples/blog/schema.prisma`). A sample that starts
with `# sketch` uses other models and is not run. `examples/blog/demo.py` runs the
main API on SQLite and PostgreSQL. Feature extensions (proxies, query defaults,
composition, file fields, generic relations) have their own docs: see [README.md](../README.md).

## Layout

| Path | What |
|---|---|
| `languages/python/orm/` | Python package: `expr.py` (expressions, `func`, `outer` / `exists`, windows → IR), `fields.py` (descriptors), `model.py`, `query.py` (QuerySet, `load()`), `select.py` (`select()`, `Row`), `cte.py` (CTEs), `write.py` (insert / update / delete statements), `pagination.py` (`Page`), `db.py` (connections, transactions, locks), `protection.py` (`allow_writes`), `debug.py` / `testing.py` (N+1 finder, pytest plugin), `hooks.py` (package hooks), `composition.py` (`attach`), `_references.py` (reference loaders), `migrations.py` / `__main__.py` (migrations, CLI: [`schema.md`](../schema/schema.md)), `_profiles.py` (native profile selection) |
| `crates/core/` | Rust crate `orm-core`, no binding code: schema language, IR (`ir.rs`), extensions, migrations, code generation, the `orm` CLI (see [`schema.md`](../schema/schema.md)) |
| `crates/engine/` | Rust crate `orm-engine`, shared by both bindings: `plan.rs` (IR → sea-query statements), `db/` (drivers: `mod.rs` traits, `postgres.rs`, `sqlite.rs`), `exec.rs` (running plans, prefetch, `update_many`), `migrate.rs` (applying migrations), `params.rs` (the values a binding passes in) |
| `crates/cli/` | Rust crate `orm-cli`: the `orm` command line, run as the `orm` binary, `python -m orm` and `npx orm` |
| `languages/python/native/` | Rust crate `orm-python` (PyO3) on top of `orm-engine`: `build.rs` (rows → instances and `Row`s), `convert.rs` (Python ↔ values), `errors.rs`, `lib.rs`. It is packaged as `orm_native_<profile>._native`; `orm._native` forwards to the selected profile |
| `languages/python/packaging/` | native profiles: `postgres`, `sqlite`, `combined`, `tooling` (both drivers, CLI and code generation), `custom` (an extension build): [`optional-packaging.md`](../contracts/optional-packaging.md) |
| `languages/js/`, `languages/js/native/` | The TypeScript package on the same engine: [`typescript-api.md`](typescript-api.md) |
| `examples/blog/` | `schema.prisma`, the `models.py` / `models.pyi` generated from it, its migrations, `demo.py` |
| `languages/python/tests/` | SQL shape tests (no DB), Postgres end-to-end tests, mypy + pyright stub checks |

Python 3.11 or newer (`select()` rows are typed with `TypeVarTuple`).

```bash
uv venv .venv && . .venv/bin/activate
uv pip install maturin pytest pytest-asyncio mypy pyright
uv pip install -e .        # the thin orm package
(cd languages/python/packaging/tooling && maturin develop)   # native tooling profile; --release for benchmarks
python -m pytest           # needs Postgres; ORM_TEST_DATABASE_URL overrides the default URL
python examples/blog/demo.py sqlite://:memory:
```

`orm` needs exactly one native profile: install `orm[postgres]`, `orm[sqlite]`,
`orm[combined]` or `orm[tooling]`. Set `ORM_PROFILE` when more than one is installed;
`ORM_PROFILE=custom` selects an extension build ([`build-time-extensions.md`](../contracts/build-time-extensions.md)).

## Models: the generated module

`python -m orm generate` turns `schema.prisma` into two files (see [`schema.md`](../schema/schema.md)):

* **`models.py` (runtime).** The compiled schema IR plus `orm.define()`, which builds
  the model classes (field and relation descriptors) from it. `orm.load("schema.prisma")`
  does the same at runtime without a generated file.
* **`models.pyi` (types).** Per model:
  * the model class (`id: f.BigInt[int]`, `posts: f.HasMany[Post, _PostPath]`, a typed
    `update()`);
  * a **path class** `_PostPath` listing the model's columns as `ColumnRef[T]` and its
    relations as other path classes. This is what makes `User.posts.<TAB>` complete;
  * `PostInsert` / `PostUpdate` / `PostUpdateRow` **TypedDicts**: the row shapes `insert`,
    `update` and `update_many` accept (required vs optional fields come from the schema);
    a composed model also gets `<Model>Attach` ([`model-composition.md`](../extensions/model-composition.md));
  * a **query set class** `PostQuerySet` with `insert()`, `insert_many()`, `update()` and
    `update_many()` typed by those TypedDicts, since `objects`, `filter()` and the other
    builders return `Self`;
  * a `load_<relation>()` method for each to-one relation ([`async-reference-loading.md`](async-reference-loading.md)).

`orm.define(schema, *, registry=None, module=None, required_capabilities=())` builds the
classes from a compiled IR; `orm.load(path)` and `orm.loads(source)` compile a schema
file or text first. Each returns the model and enum classes by name. Classes join
`orm.registry` unless you pass `registry=orm.Registry()`; `connect(..., registry=...)`
uses that registry.

Models can also be declared as classes, without a schema file:

```python
from orm.fields import Integer, String

class Counter(orm.Model, registry=registry, table="counters"):
    id = Integer(primary_key=True)
    label = String(default=lambda: "new", nullable=True)          # Python callable default
    token = String(client_default={"call": "uuid"}, nullable=True)  # the IR of @client_default(uuid())

registry.prepare()
```

`Field(*, primary_key=False, auto_increment=False, nullable=False, unique=False,
index=False, column=None, default=MISSING, default_now=False, server_default=False,
client_default=None)` is the base of every field class in `orm.fields`. `default` is a
literal (a database default) or a callable (run by the ORM for each insert).
`client_default` takes the IR form of `@client_default`: a literal or
`{"call": "uuid" | "uuid7" | "now"}`. `has_insert_default` tells whether an omitted
value gets filled. Queries on an unprepared class registry fail; `connect()` also
prepares it.

The descriptors behave differently on the class and on an instance:

| | on the class | on an instance |
|---|---|---|
| column `User.email` | `ColumnRef[str]`, for queries | `str`, read-only (plain `__dict__` read, no descriptor call) |
| to-one `Post.author` | `_UserPath` | `User`, if loaded, else `NotLoaded` |
| to-many `User.posts` | `_PostPath` | `RelatedSet[Post]`: a query set over the user's posts |
| one-to-one `User.profile` | `_ProfilePath` | `Profile` or `None`, if loaded, else `NotLoaded` |

An instance also has `pk` (its primary key value), `to_dict()` (its loaded fields) and
the write methods in [Writes](#writes).
| many-to-many `Post.tags` | `_TagPath` | `ManyRelatedSet[Tag]`: the post's tags, with `add()` / `remove()` |

## Custom query-set methods

Named filters (Django's custom managers) go in a subclass of the model's query set, in your own module:

```python
# blog/queries.py
from typing import Self
from blog.models import Post, PostQuerySet

class PostQueries(PostQuerySet):
    def published(self) -> Self:
        return self.filter(Post.published)

    def popular(self, views: int = 100) -> Self:
        return self.filter(Post.views >= views)
```

Generate the models with the class, as `module:Class`:

```bash
python -m orm generate --query-set Post=blog.queries:PostQueries
```

or in `pyproject.toml`:

```toml
[tool.orm.query_sets]
Post = "blog.queries:PostQueries"
```

The native tooling generator accepts the same mapping explicitly:

```python
from orm import _native

module, stub = _native.generate_python(
    "schema.prisma", query_sets={"Post": "blog.queries:PostQueries"}
)
```

It returns the `models.py` and `models.pyi` source strings. Omitting `query_sets`,
passing `None`, or passing `{}` keeps the default query-set classes. This native
call uses only the supplied mapping; it does not read the CLI configuration.

Then `Post.objects` is a `PostQueries`, and the methods chain with every builder method in both orders:
`await Post.objects.published().filter(Post.author_id == 1).popular()`.
`models.pyi` types `Post.objects` as the class, so mypy and pyright check the calls.

* `models.py` calls `orm.use_query_set(Post, "blog.queries:PostQueries")`.
  The module is imported on the first use of `Post.objects`, so it can import `blog.models` without an import cycle.
  `use_query_set(Model, cls)` also takes the class itself, for models from `orm.load()`.
* Relation sets have the methods too: `await user.posts.published()`, `post.tags.<method>()`.
  The stub does not type them on a relation set yet; `Post.objects.published().filter(Post.author_id == user.id)` is typed.
* `User.posts.objects.published()` uses them for related rows in `load()`.
* The class must subclass `QuerySet` and must not declare `__slots__` (it mixes with the relation-set classes); `use_query_set` raises `TypeError` otherwise.
  Keep state in the query, not in attributes: builder methods copy the instance `__dict__`, but nothing else.

## Queries

Builders return a new immutable `QuerySet`. Awaiting it runs the query. Awaiting the
same query set again gives the same rows without querying again (a new list each time;
concurrent awaits share one run): the result cache, as in Django. Builders and `.all()`
return new query sets, so `await qs.all()` re-reads; `await User.objects` always
queries, since `User.objects` lives as long as the class. `select()` caches the same way.

```python
await User.objects.filter(cond, cond2)        # list[User]; conditions AND-ed
User.objects.exclude(cond)                    # NOT (...)
User.objects.order_by(User.name, User.id.desc())
User.objects.order_by(-User.created_at, User.id)   # -column is column.desc()
User.objects.all()[10:20]                     # LIMIT 10 OFFSET 10
async for u in User.objects.filter(...): ...

await qs.first() / qs.last()                  # lazy OneRowQuery; by ordering, else pk; None if empty
await qs[n] / qs[-1]                         # row n of this slice; [-1] is last()
qs.first().prepare() / qs.first().inspect()  # prepare a one-row read or inspect its plan
await qs.get(cond)                            # User | None; rejects multiple matches
await qs.require().get(cond)                  # User; rejects zero or multiple matches
await qs.require().first()                    # User; rejects zero matches
await qs.require().all()                      # list[User]; rejects an empty list
await qs.count() / qs.exists()
await qs.in_bulk([1, 2, 3])                   # {1: <User 1>, 3: <User 3>}: by pk, missing ids left out
await qs.in_bulk(emails, field=User.email)    # by a unique field; no ids: every row
qs.sql()                                      # SQL with values inlined, for debugging
qs.limit(n) / qs.offset(n)                    # the same as a slice; takes orm.param()
qs.load(User.name, User.email)                # partial instances: other fields raise NotLoaded
qs.without_defaults()                         # skip the schema query defaults (query-defaults extension)
qs.current_filter()                           # [{source, name, expr, active}] of the default filters
qs.without_related()                          # drop default and explicit relation loading
qs.model                                      # the model class
```

`OneRowQuery` runs afresh on each await; TypeScript reuses the result of its first await.
Only `-1` or non-negative integer indices are supported; another negative index raises `ValueError`, and a `param()` index raises `QueryError`.
`last()` (also `[-1]`) raises `ValueError` at the call on a query set with a limit or offset; a `param()` limit before `first()`, `[n]`, `get()` or prepared `first()` / `get()` raises `QueryError`.
Use a relation's `[n].label(name)` in `load()` to store one row per parent (or `None`); `get().label(name)` raises `TypeError`.

A model with a composite primary key (`@@id([shop, id])`) has a tuple `pk` in key order, `(1, 7)`.
`in_bulk()` takes and gives these tuples.
A relation through a composite foreign key reads the whole key: `rel.via_fields` and `rel.to_fields` are tuples, and `rel.via` raises.

`.require()` clones the query and rejects absent row results with
`Model.DoesNotExist`. It applies to `.get()`, `.first()`, `.last()`, `.all()`,
awaiting the query and async iteration. Builders retain the modifier, and prepared
queries support it before or after `.prepare()`. `.get()` always rejects multiple
matches, including without `.require()`. Counts and existence checks keep their
normal zero/false results.

`.asc(nulls="first")` and `.desc(nulls="last")` place NULLs; without `nulls`, the database decides.
`-column` is a descending `Ordering`, for `order_by()` and a relation query set.
It is never SQL negation: write `0 - Post.views` for that.
`ordering.reversed()` gives the opposite order, NULL placement included.
`-` works on a column only, and an ordering in `filter()` or `select()` is a `TypeError`.

Expressions: `== != < <= > >=` (with `== None` meaning `IS NULL`), `.in_()`,
`.not_in()`, `.is_null()` / `.is_not_null()`, `.between()`, `.like()` / `.ilike()` / `.contains()` /
`.icontains()` / `.startswith()` / `.endswith()` (only on string columns; wildcards in
the argument are escaped), `+ - * /`. Combine with `& | ~` or `and_() / or_() / not_()`.
A boolean column is a condition by itself: `filter(Post.published)`. Using Python's
`and` / `or` / `not` on expressions raises `TypeError` instead of silently doing the
wrong thing.

### Relation filters (Django semantics, no duplicate rows)

| Query | Meaning | SQL |
|---|---|---|
| `filter(User.posts.views > 10, User.posts.published)` | some **single** post is both | one `EXISTS (… views > 10 AND published)` |
| `filter(User.posts.views > 10).filter(User.posts.published)` | some post has views > 10 and some (maybe other) post is published | two `EXISTS` |
| `exclude(User.posts.published == False)` | no unpublished post (users without posts included) | `NOT EXISTS` |
| `filter((User.email == x) \| (User.posts.views > 10))` | users without posts still match on email | `email = x OR EXISTS (…)` |
| `filter(User.posts.comments.body.contains("hi"))` | nested hops | nested `EXISTS` |
| `filter(Post.author.email == x)` | to-one hops | `EXISTS` on the PK (planned as a semi-join) |
| `filter(Post.author == alice)` | the post's key is alice's key | `author_id = $1`, no join |

This matches what Django returns for multi-valued relations: one `filter()` call means
the same related row, separate calls are independent, and negation means "none". It
differs in one way: Django uses a `JOIN` and returns a parent once **per matching
child** unless you add `.distinct()`. `EXISTS` returns each parent once and needs no
`DISTINCT`. The same filter code also works in `UPDATE` and `DELETE`, where Postgres
doesn't allow `JOIN`.

`NOT` doesn't group into a subquery: `filter(User.posts.a & ~User.posts.b)` means "a
post with a, and no post with b", as in Django.

A `BelongsTo` relation compares with an instance of its target:
`Post.author == alice` is `Post.author_id == alice.id` (the column the relation
references), and `Post.author == None` is `IS NULL`. `Comment.post.author == bob`
reaches `posts` but not `users`. Other relation kinds, an instance of another model
and an instance whose key is `None` raise.

### Loading related objects

Async code can't lazy-load on attribute access, so access is explicit (like SQLAlchemy's
`lazy="raise"`):

```python
cs = await Comment.objects.load(Comment.post.author, Comment.author)  # to-one: LEFT JOINs
cs[0].post.author.name

us = await User.objects.load(User.posts)   # to-many: +1 query, WHERE author_id IN (...)
us[0].posts.cached                  # list[Post], no await; post.author is set back to the user
await us[0].posts                   # uses the loaded rows if present, else queries
await us[0].posts.filter(Post.published).count()
await us[0].posts.insert(title=..., body=...)   # author_id filled in
post.author                         # NotLoaded unless loaded

await User.objects.load(User.posts.comments)   # +2 queries: posts, then their comments
await Post.objects.load(Post.title, Post.author.name)   # partial posts and partial authors
await User.objects.load(
    User.posts.objects.filter(Post.published).order_by(Post.views.desc())[:3],
    User.comments.objects.load(Comment.post).label("recent"),
)
await Comment.objects.load(Comment.post.objects.as_prefetch())   # to-one by IN (...) query
```

The main query and its prefetch queries run in the same Rust call, one query per
relation level (keys deduplicated, `NULL` keys skipped). Each key is a bound parameter,
so more keys than a statement takes (the dialect's `max_params`: 65 535 on Postgres, 32 766 on SQLite)
split into several queries; the split is between parents, so ordering and slices per
parent are unaffected. (Django doesn't split: on Postgres it inlines the values into the
SQL text client-side, so the limit doesn't apply to it.)

`load()` takes columns, relation paths, relation query sets and labeled relation aggregates; calls add up.

* A labeled aggregate, `load(func.count(Post.comments).label("n"))`, stores the value in the attribute `n` of each row, so `post.n` works.
  It runs in the same statement as the rows, on the model's query set, a relation query set (`User.posts.objects.load(...)`) and instance `load()` (`await post.load(...)`, which reads only the aggregate).
  Only an aggregate over a relation takes a label here; another expression raises `TypeError` (use `select()`), as does an aggregate without a label.
  A label that names a field, relation or member of the model, or that another label or relation query set of the same `load()` also uses, raises `ValueError`.
  `select()` ignores it. The attribute is not typed statically.

* A column of the model makes its instances partial: they hold the named fields, and, hidden, the primary key and the relation keys. Reading another field raises `NotLoaded`.
  Without a column of the model itself, its instances keep their normal selection: `load(Post.author)` gives whole posts.
  A related model is partial in the same way when a column of it is named (`Post.author.name`) and no bare path ends at it (`load(Post.author, Post.author.name)` gives the normal selection of the authors, which is all public fields or the schema's default selection, united with `name`).
  A bare path gives the normal selection united with the columns named beside it, and a set's own columns join the named ones.
  `load()` without items gives whole instances again (see [`selection-and-defaults.md`](../schema/selection-and-defaults.md)).
* A path loads every relation along it: `User.posts.comments` fills `user.posts` and each post's `comments`.
  Paths with a common prefix share its query.
* To-one relations are joined. A path with a to-many hop is loaded with an `IN (...)` query from its first hop, and every item under that hop uses the query.
* `User.posts.objects` is the relation query set: `Post.objects` (with its query-set class and query defaults) bound to `User.posts`.
  Use it inside `load()` (including instance loading and prefetching). Running it on its own, preparing it, writing through it, or using it in a projection, CTE or subquery raises `TypeError`; use `Post.objects` for standalone queries.
  A column or path through the relation combined with an unlabeled query set that filters, orders, slices or bypasses defaults raises `ValueError`: put the columns in the set's own `.load()`, or label the set to request a separate result. A plain set or one that only adds `.load()` can share the path.
  A bare path or plain set gives the normal selection united with the columns named beside it. A column beside a set with its own columns is added to that set's columns.
  It gives the related rows their own filters, ordering, slice and `.load(...)` (nested under it).
  As in Django, the rows fill the relation itself, so `user.posts.cached` and `await user.posts` then give the filtered rows; `user.posts.filter(...)` and other new queries still see every post.
  With `.label("recent")` they go to a plain list attribute (`user.recent`), and the relation stays unloaded; `[0].label("best")` stores the first row, or `None`.
  A path never goes under a label: `User.posts.comments` fills `user.posts`; a labeled query set loads its own items with `.load()`.
  A label that is a field, relation or member of the model, one label for two relations, or two different unlabeled query sets for one relation raise `ValueError`.
* `.as_prefetch()` loads a to-one relation with an `IN (...)` query instead of a join; the rows are the same.
  A to-one relation query set that filters, orders, slices or labels uses the query without it.
* `.label()` and `.as_prefetch()` exist only on a relation query set (else `TypeError`). The type checkers don't know a labeled attribute.
* A **slice applies per parent**: `[:3]` above is each user's three most viewed posts,
  planned as `ROW_NUMBER() OVER (PARTITION BY author_id ORDER BY views DESC, id)` in a
  subquery and `WHERE _rn <= 3` around it (Django 4.2 does the same).

**One instance.** `await post.load(Post.body, Post.author, Post.comments)` accepts the
same arguments as queryset `load()`, fills the existing instance, and returns it.
It preserves loaded fields and skips loaded values. `reload=True` refreshes the
requested values. `await post.load(reload=True)` with no items reads all scalar fields, so a partial object becomes full.
`await post.load()` fills the scalar fields that are missing. Nested paths and
filtered relation querysets work too. Collection values remain in `.cached`.
`post.load()`, `post.update()` and `post.delete()` run through `post.qs()`: they apply no query default to the row itself, but a relation or aggregate they load keeps the defaults of its target.

**Instances you already have.** `await orm.of(instances).load(*items)` loads relations onto them, as `load()` does for the rows of a query:

```python
bundle = await Bundle.objects.require().get(Bundle.id == 1)
await orm.of([bundle]).load(Bundle.items.product, Bundle.versions.objects.order_by(-Version.id)[:1].label("latest"))
```

A bound load of relations only runs no query for the objects themselves: the keys come from the instances, and every relation loads with an `IN (...)` query.
A filter or a lock reads the keys first.
See "Bound query sets" for the rest.
Each of those queries gives a query event.

### Bound query sets

`orm.of(objs)`, `Model.objects.of(objs)` and `post.qs()` give a bound query set: a query set filtered to the primary keys of objects you already hold.
Awaiting it reads those rows, fills the objects in place, and gives the matched objects in input order.
An object whose row is missing stays unchanged.
The objects are of one model and one database; `using(db)` names the database to read.

```python
await orm.of(posts).load(Post.author, Post.comments)    # relations only: no query for the posts
await Post.objects.of(posts).filter(Post.views > 10)    # a read of the rows, with a filter
async with db.transaction():
    await post.qs().lock()                              # reload and lock
    await post.qs().using("primary").load(Post.views, reload=True)
await post.qs().update(title="New")                     # UPDATE ... RETURNING, fills `post`
await post.qs().delete()
```

* A bound set keeps the rule "at most one row per bound object": `filter()`, `exclude()`, `load()`, `lock()`, `using()`, `without_defaults()`, `without_related()`, `update()`, `delete()`, `count()`, `exists()`, `sql()` and `inspect()` are allowed, and so are the extension members `hard_delete()` and `undelete()`.
  Ordering, slices, `first()`, `get()` and other shape-changing methods do not exist on it.
* A bound set applies no query default of the model to its own rows, for every read and write: `of(objs)` and `post.qs()` see a soft-deleted post, and `update()` and `delete()` write it.
  `filter()` still narrows.
  A relation query set inside a bound `load()` keeps the defaults of its target model, also in an aggregate.
  `without_defaults()` stays allowed: on the bound set it changes only the targets.
  The same holds for `lock()`: `post.qs().lock()` locks a soft-deleted row.
  This is the same rule as for the shortcuts `post.load()`, `post.update()` and `post.delete()`.
* A plain `await post.qs()` with no `load()` reads the normal selection of the model, so a partial object gets those fields.
* A read of more than 500 keys runs in chunks of at most 500 keys, one statement for each chunk.
  The chunks are not one snapshot unless you hold a transaction: use `db.transaction()` when you need one.
  `sql()` and `inspect()` describe the first chunk.
* A bound write (`update()`, `delete()`) is one statement: above the parameter cap of the database (32,766 on SQLite, 65,535 on PostgreSQL) it raises `QueryError` before it runs, so split a very large set. An update that assigns a key field of 2 or more distinct objects raises `TypeError` before it runs: the rows cannot be matched back to the objects. One object may change its key.
* `await post.load(reload=True)` does not reload a label such as `func.count(Post.comments).label("n")`: the instance does not know the label's expression.
* The version enters writes only: reads and `lock()` refresh a stale object.
  Bound writes (`update()`, `delete()`) have no version check yet.

`refresh()` is removed from instances. Migrate as follows:

| Before | After |
| --- | --- |
| `await post.refresh()` | `await post.load(reload=True)` |
| `await post.refresh(Post.views)` | `await post.load(Post.views, reload=True)` |
| `await post.refresh(lock=True)` | `await post.qs().lock()` |
| `await job.refresh(lock=True, skip_locked=True)` gives `False` | `await job.qs().lock(skip_locked=True)` gives `[]` |
| `await orm.prefetch(posts, Post.author)` | `await orm.of(posts).load(Post.author)` |

`refresh()` raised `DoesNotExist` for a row that is not found; `qs().lock()` and the other bound reads give `[]`.

`orm.prefetch()` is removed.

### One-to-one and many-to-many

```python
# One-to-one (`profile Profile?` on User, Profile.user_id unique)
us = await User.objects.load(User.profile)                  # LEFT JOIN; user.profile is a Profile or None
us = await User.objects.load(User.profile.objects.as_prefetch())  # +1 query; profile.user is set back too
await User.objects.filter(User.profile.role == Role.admin)  # EXISTS, like any relation

# Many-to-many (`tags Tag[] @relation(through: PostTag)`)
await post.tags                                     # list[Tag]
await post.tags.add(news, rust)                     # inserts PostTag rows (existing links are kept)
await post.tags.add(news, through_defaults={"position": 1})  # other fields of the new PostTag rows
await post.tags.remove(news)                        # deletes them (a join model with `@soft_delete` soft-deletes them); returns how many
await post.tags.set([news, py])                     # exactly these
await post.tags.clear()
tag = await post.tags.insert(name="go")             # insert a Tag and link it, in one transaction
await Post.objects.filter(Post.tags.name == "rust") # one EXISTS over tags JOIN post_tags
await Post.objects.select(Post.title, func.count(Post.tags))
await Post.objects.load(Post.tags)                  # +1 query: tags JOIN post_tags WHERE post_id IN (...)
await Tag.objects.load(Tag.posts.author)
```

* `user.profile` raises `NotLoaded` until `load()` loads
  it, like a to-one relation; it is never a query in disguise.
* A many-to-many hop is planned as one hop through the join table, so Django's
  semantics hold: conditions in one `filter()` call must hold for the same tag, separate
  calls are independent, `exclude()` means "no such tag". Aggregates
  (`func.count(Post.tags)`) are correlated subqueries over the two tables.
* `post.tags` is a `ManyRelatedSet`: a query set over the post's tags (filter, order,
  count, ...) that reads prefetched rows when unchanged, like `user.posts`. `add()`,
  `remove()`, `set()` take instances or keys. Changing the links drops the prefetched
  rows. `add()` and `set()` take `through_defaults={...}`: values of the join model's
  other fields in the new join rows. Existing links keep their values, the link's key
  fields can't be set this way, and an unknown field raises. `set()` runs in one
  transaction: when it fails, the old links stay. The join model stays an ordinary model for
  anything else (bulk inserts: `await PostTag.objects.insert_many(...)`).
* Prefetching selects the join row's key next to each tag, so a tag linked to two posts
  comes back once per post. `Post.tags.objects...[:3]` slices per post.

### Decimal, enum and array columns

```python
p = await Profile.objects.insert(user=alice, balance=Decimal("10.25"), role=Role.admin,
                                 links=["https://a.example"])
p.balance                                           # Decimal('10.25'), never a float
await p.update(balance=Profile.balance + Decimal("0.10"))
await Profile.objects.select(func.sum(Profile.balance)).scalar()   # Decimal

p.role is Role.admin                                # members of the generated enum
await Profile.objects.filter(Profile.role.in_([Role.admin, "editor"]))   # values work too

await Profile.objects.filter(Profile.links.has("https://a.example"))    # links @> ARRAY[...]
await Profile.objects.filter(Profile.links.has_any(urls))                # links && ...
Profile.links.has_all(urls) / Profile.links.contained_by(urls)          # @> / <@
await Profile.objects.select(func.cardinality(Profile.links))
await Profile.objects.select(Profile.links[1])                          # links[1]: the first element
await Profile.objects.select(func.unnest(Profile.links))                # one row per element
```

* Decimal parameters accept `Decimal`, `int`, `float` (through its `str`) and decimal
  strings; `NaN` and infinities are refused. Values outside the column's precision are
  the database's error.
* Enum classes are `StrEnum` (native and text storage) or `IntEnum` (int storage), so
  members compare equal to their stored values. A value the enum doesn't know (added in
  the database by hand) reads back as the plain value.
* Arrays are Python lists both ways (tuples are accepted); elements may be `None`.
* `Profile.links[i]` is SQL's element access: the index is 1-based, as in the SQL
  text, and an index out of range gives `None`. `func.unnest(...)` returns one row per
  element, so it is valid only as a `select()` column. SQLite has no array columns.

### JSON columns

```python
await Doc.objects.filter(Doc.meta["author"]["name"] == "Ann")       # meta -> 'author' -> 'name' = '"Ann"'
await Doc.objects.filter(Doc.meta["n"] > 3)                         # jsonb ordering: numbers compare as numbers
await Doc.objects.filter(Doc.meta["tags"][0].as_text() == "x")      # ->> : text, no JSON quotes
await Doc.objects.filter(Doc.meta["name"].as_text().icontains("an"))
await Doc.objects.filter(Doc.meta.json_contains({"kind": "post"}))  # meta @> '{"kind": "post"}'
Doc.meta.json_contained_by(value) / Doc.meta.has_key("tags")        # <@ / ?
await Doc.objects.filter(...).update(meta=Doc.meta.json_merge({"seen": True}))   # meta || '{...}'
```

* A path step is a string key or a 0-based integer index (a `String[]` column indexes
  from 1, JSON from 0). The value is `jsonb`, so a
  comparison binds the other side as JSON: `== "x"` is the JSON string `"x"`, `== 5` the
  number. `as_text()` reads the value as text (`->>`) and ends the path: no step can follow.
  `== None` is `IS NULL` (a JSON `null` value is not SQL `NULL`). The comparison is `jsonb`
  ordering across types, so `> 3` also matches `true`, `{}` and `[1]`.
* The SQL contains a string key as an escaped `E'...'` literal, like an integer index. A key with a NUL character raises `QueryError`.
  So `group_by(Doc.meta["kind"].as_text())` and the same path in `select`, `having` and `order_by` have equal SQL text.
* On an array column, `col[1]` stays SQL's 1-based element access; a string key on a
  column that is not `Json` is a `TypeError`.
* `json_contains`, `json_contained_by` and `has_key` work on the column and on a path,
  and a GIN index on the column (`@@index([meta], type: Gin)`) serves `json_contains`,
  `json_contained_by` and `has_key` on the column only, not on a path. They take a
  `param()` in a prepared query.
* `json_merge(value)` is `||`: objects merge one level deep (the keys of `value` win);
  arrays join, and a scalar merged with an object or array makes an array. A column that is
  `NULL` stays `NULL` after a merge.
* PostgreSQL only: on SQLite these are a `QueryError`.

### Full-text search

```python
vector = func.to_tsvector("english", Post.body)
await Post.objects.filter(vector.matches("running dogs"))        # @@ plainto_tsquery('english', ...)
query = func.websearch_to_tsquery("english", '"quick fox" -lazy')
await Post.objects.filter(vector.matches(query)).order_by(func.ts_rank(vector, query).desc())
func.to_tsquery("english", "cat & !dog") / func.plainto_tsquery("english", text)
```

* The first argument `"english"` names the text search configuration; without it the
  server's `default_text_search_config` applies. It is written into the SQL as
  `'english'::regconfig` (it must be a name), so an expression index on the same expression
  is used in every plan, also a prepared statement's generic plan. A document bound as a
  value (`param()`) is not an expression, so no index covers it.
* `vector.matches("text")` with a plain string uses `plainto_tsquery` with the vector's
  configuration; a `param()` query is wrapped the same way. A vector that is not
  `to_tsvector(config, ...)` (for example a `coalesce(...)`) uses the server default
  configuration.
* Index the same expression with a GIN index in the schema:
  `@@index([sql("to_tsvector('english', body)")], type: Gin)`.
* `ts_rank` takes no plain string: pass a tsquery. It is a `float`. A tsvector or tsquery itself can't be a `select()` column.
* PostgreSQL only: on SQLite these are a `QueryError`.

### Partial rows, OR of query sets, column paths

* `load(...)` with columns gives partial instances (see "Loading related objects" and [`selection-and-defaults.md`](../schema/selection-and-defaults.md)).
  A column through relations trims the related instance: `Comment.objects.load(Comment.body, Comment.post.title)` joins `comment.post`, with only `title` public; `User.objects.load(User.posts.title)` loads partial posts by `IN (...)` query.
  Unlike Django's `only("post__title")`, a column of a related model leaves the model itself whole; name `Comment.id` for a key-only root.
* `qs1 | qs2` is one query set with the filter `(filters of qs1) OR (filters of qs2)`; order, loading and `using` come from `qs1`. A relation set on the left gives a plain query set. A custom class takes its state from the left side.
  `qs2` must set nothing but filters, neither may be sliced, and each side has at most one `filter()`/`exclude()` call, else `QueryError`:
  conditions of one call through a to-many relation must hold for one related row, so two calls can't be joined into one.
* `User.posts.objects.order_by(-Post.views)[0].label("best")` in `load()` stores the first related row, or `None`, in `user.best` instead of a list.
  Positions apply per parent; `[-1]` reverses the order. A to-many relation and a label are required.
  An unlabeled one-row query in `load()` raises `TypeError`; awaiting a relation query outside `load()` also raises `TypeError`.
* `orm.column(Bundle, "items.product.title")` is the column a dotted path names, the same as `Bundle.items.product.title`.
  Each name before the last is a relation, the last a field; an unknown name raises `LookupError`.
  It is for adapters that map request names to columns (search and ordering filters). It is a function, not `Model.column`, so it can't collide with a field named `column`.

### Big tables: batches

```python
async for post in Post.objects.filter(Post.published).iterate(batch_size=1000):
    ...
async for batch in Post.objects.batches(500):      # list[Post] per batch
    await search_index.add(batch)
```

Each batch is `WHERE <filters> AND id > <last id> ORDER BY id LIMIT n` (keyset paging, no
`OFFSET`), so memory stays flat and every batch is an index range scan. `load()` and
`lock()` apply per batch; `order_by` and slicing are rejected.

### Cursor pagination

```python
page = await Post.objects.filter(Post.published).order_by(-Post.created_at).paginate(first=20)
page.items, page.has_next, page.next_cursor
page = await qs.paginate(first=20, after=page.next_cursor)       # the next page
page = await qs.paginate(last=20, before=page.previous_cursor)   # the previous page
```

`paginate()` reads one page by keyset, like `batches()`, so rows added between pages do not repeat others.
A deep page stays fast when an index matches the order and its first column is NOT NULL: the page then starts with a plain bound on that column.
With a nullable first column there is no such bound, and Postgres reads the index from its start to the cursor.
The order is `order_by()`, else the schema default order, else the primary key.
The primary key is added as the last order column when no column of the order is unique.
`load()` and query defaults apply to each page.

* `next_cursor` is the cursor of the last item, and `previous_cursor` that of the first; both are `None` on an empty page.
* `has_next` / `has_previous` come from one extra row in the direction of reading. In the other direction, they are `True` when the call gave a cursor.
* Order columns are columns of the model itself: an expression or a related column is a `QueryError`. JSON, array and enum columns are a `QueryError` too.
* A nullable order column needs `nulls=`: `Comment.author_id.desc(nulls="last")`.
* A cursor is opaque base64 of the order values and a fingerprint of the order. A cursor from another order or model is a `QueryError`.
* A cursor is not signed. A client can change it to start at any position of the same order, so do not use it for access control.
* A malformed cursor, or one with a value its column can't hold, is `QueryError("invalid cursor")`; a cursor that is not a string is a `TypeError`.
* A cursor holds positions, not filters: it stays valid after a filter change and starts at the same position.
* There is no total count; call `count()` for it.

### Prepared queries

A query the app runs over and over with different values (a lookup per request) can be
built once and then only bound per call. `orm.param("name")` marks a value to supply
on each call:

```python
# module level: the query set, its IR and the IR's JSON are built once
post_by_id = Post.objects.load(Post.author).filter(Post.id == orm.param("id")).prepare()
latest = (Post.objects.filter(Post.author_id == orm.param("user"))
          .order_by(Post.created_at.desc())
          .limit(orm.param("n")).offset(orm.param("skip")).prepare())

post = await post_by_id.get(id=post_id)         # Post | None; also .first() .count() .exists()
post = await post_by_id.require().get(id=post_id)  # Post; absence raises
posts = await latest(user=me.id, n=20, skip=40) # the rows
latest.params                                   # frozenset({'user', 'n', 'skip'})
latest.sql(user=1, n=20, skip=0)                # the SQL with these values, for debugging
```

A call copies the stored parameter list, fills in the values and hands the stored IR
JSON to the engine. It skips building the query set, the expression tree, the IR and
its JSON: −22% on `get` by primary key and −13% to −31% on a 50-row read with two filters
(`bench/RESULTS.md`, "Prepared queries"). Rust still plans each call, and Postgres
reuses the statement, which is prepared once per connection.

* A `param()` stands for one value: in comparisons and arithmetic, `like` / `contains`
  / `startswith` / `endswith` (the pattern is escaped when the value is bound),
  `has()`, `limit()` and `offset()`, and in filters of relation query sets in `load()`.
* Values can't be `None`: whether a comparison is `= $1` or `IS NULL` is fixed when the
  query is built, so write `is_null()` into the query instead.
* `in_(param(...))` is rejected, because the number of values is part of the query. A
  query set limited by a `param()` can't be sliced; use `limit()` / `offset()`.
* A `param()` outside a prepared query raises `QueryError`. Calls with a missing or
  unknown name raise `TypeError`.
* Prepared queries read. Inserts and updates already send their rows to Rust without an
  IR, so there is nothing to prepare.

`LIMIT` and `OFFSET` are parameters in the IR for every query, not only prepared ones:
`qs[10:20]` and `qs[30:40]` are the same IR document and the same SQL text.

## Columns and aggregates: `select()`

`select()` is the one way to read anything other than whole instances. It replaces
Django's `values()`, `values_list()`, `aggregate()` and `annotate()`:

```python
rows = await (
    Post.objects.filter(Post.published)
    .select(Post.author_id, func.count().label("posts"), func.sum(Post.views).label("views"))
    .group_by(Post.author_id)
    .having(func.count() > 2)
    .order_by(func.count().desc())
)
rows[0].posts; rows[0][1]; author_id, posts, views = rows[0]    # Row: a tuple with names

await Post.objects.select(func.max(Post.views)).scalar()         # int | None
await Post.objects.filter(...).select(Post.id).scalars()          # list[int]
await qs.select(...).first()                                    # Row | None
await qs.select(...).get()                                      # Row | None; rejects multiple
await qs.select(...).require().get(Post.id == post_id)           # exactly one matching Row
await Post.objects.select(Post.title, Post.author.name.label("author"))   # to-one: LEFT JOIN
await Post.objects.select(Post.author_id).distinct()
await Post.objects.select(func.count(Post.author_id, distinct=True)).scalar()   # also sum / avg
await Post.objects.select(Post.title).distinct(Post.author_id).order_by(Post.author_id, Post.views.desc())

# The model plus computed values (Django's annotate):
for user, n_posts, views in await User.objects.select(User, func.count(User.posts), func.sum(User.posts.views)):
    ...
# Aggregates over relations also filter:
await User.objects.filter(func.count(User.posts) > 2)
# Subqueries:
await Post.objects.filter(Post.author_id.in_(User.objects.filter(...).select(User.id)))
```

Like model queries, selected-row queries use `.get(*conditions)` to return zero or
one result, rejecting multiple matches with `orm.MultipleObjectsReturned`.
`.require()` rejects queries returning no rows with `orm.DoesNotExist`; it also
applies to `.first()`, `.all()`, awaiting the selection, `.scalar()` and `.scalars()`.
A row containing NULL values is still a result: a required scalar may return `None`
when its row exists but its column is NULL.

| Django | here |
|---|---|
| `values("id", "title")` / `values_list(...)` | `select(Post.id, Post.title)` |
| `values_list("id", flat=True)` | `select(Post.id).scalars()` |
| `aggregate(total=Sum("views"))` | `select(func.sum(Post.views)).scalar()` |
| `values("author").annotate(n=Count("id"))` | `select(Post.author_id, func.count()).group_by(Post.author_id)` |
| `annotate(n=Count("posts"))` → `user.n` | `select(User, func.count(User.posts))` → `(user, n)` rows |

* **Rows** are `Row`s: tuple subclasses (index, unpack, compare with tuples) whose items
  also have names: the field name for columns, `.label()` for expressions, the function
  name for functions (`count`, `sum`), the lower-case model name for a model. Two columns
  with the same name must be labelled. Statically a row is `Row[int, int, int | None]`
  (from overloads of `select()`), so unpacking and indexing are typed; names are `Any`.
  Selecting fewer columns is about twice as fast as building instances (1000 rows: 1.2 ms
  vs 2.3 ms here).
* **Aggregates** are `func.count` / `sum` / `avg` / `min` / `max`; scalar functions are
  `lower`, `upper`, `length`, `abs`, `coalesce`, `now`, and for strings `concat`, `trim`,
  `ltrim`, `rtrim`, `replace`, `substr` (1-based start, optional length) and `strpos`
  (1-based, 0 if absent; `INSTR` on SQLite). Over the model's own columns an
  aggregate summarizes the rows (of each `group_by()` group). Over a relation path it is
  computed **per row in a correlated subquery**: `func.count(User.posts)` is `(SELECT
  COUNT(*) FROM posts WHERE posts.author_id = users.id)`. So two aggregates over different
  relations never multiply each other, unlike Django's JOIN-based `annotate(Count(...),
  Count(...))`, and they work in `filter()` too.
* **Filtered aggregates**: every aggregate takes `filter=cond`, which is
  `FILTER (WHERE cond)`: the aggregate reads only the rows where `cond` holds. Several
  conditional counts fit in one grouped query:

  ```python
  await Post.objects.select(
      Post.author_id,
      func.count(filter=Post.published),
      func.sum(Post.views, filter=Post.created_at > last_week),
  ).group_by(Post.author_id)
  await User.objects.select(User, func.count(User.posts, filter=User.posts.published))
  ```

  Over a relation, the filter follows Django's rule: a condition on the aggregate's own
  relation applies inside the correlated subquery (`FILTER`); a condition that reads only
  to-one hops (`User.profile.role == x`) is a join, so it is an `EXISTS` at the outer level
  and keeps all posts of a matching user; a condition that reads no column of the relation
  filters the user, not the posts. With `.over(...)` the filter comes before `OVER`.
  SQLite supports `FILTER` since 3.30 (the bundled SQLite is newer).
* **`func.array_agg(expr, order_by=, distinct=, filter=)`** collects the values into a
  `list` (`ARRAY_AGG`); `NULL` values are kept as `None`. Over no rows it gives `None`,
  not `[]` (like `sum`). `order_by=Post.views.desc()` (or a list) fixes the element order;
  with `distinct=True` it must use the same expression. It works over a relation
  (`func.array_agg(User.posts.title, order_by=User.posts.title)`, `None` for a user without
  posts). The columns of `order_by` must be on the relation path of the argument: a key
  that is constant there, a window function, or an aggregate on that path raises `QueryError`.
  It does not take an array column, and it can't be a window function.
  PostgreSQL only: SQLite raises `QueryError`. The other aggregates take no `order_by`
  (their result does not depend on row order).
* **Same-relation aggregates share one subquery.** In a `select()` (without `group_by()`,
  `having()`, `distinct()`, `lock()`, a limit or an offset, and with no window function,
  local aggregate or CTE), two or more aggregates over the same
  relation path become one
  `LEFT JOIN LATERAL (SELECT COUNT(*), SUM(...) ...)` instead of one scalar subquery each.
  The result is the same: one row per parent (no fan-out), `0` for an empty count, `None` for
  an empty sum, and each aggregate keeps its own `filter` and `distinct`. Aggregates in
  `filter()`, `order_by()` and `having()` stay scalar subqueries. SQLite has no `LATERAL`
  and keeps the scalar subqueries. An aggregate whose `filter` reads only the parent's own
  columns also stays a scalar subquery. Queries with a limit or offset also keep scalar subqueries,
  so an unindexed parent sort can run before computing the selected aggregates.
* **Aggregates over to-one paths only join.** An aggregate whose arguments, `order_by` keys and
  `filter` follow to-one hops only (`func.sum(Post.author.id)`,
  `func.array_agg(Post.title, order_by=Post.author.name)`) reads a `LEFT JOIN` of the query
  instead of a subquery, so it gives one value per group, not one per row; a `NULL` foreign key
  keeps its row. This holds at the query root of a read, not in an `update()`. The `filter` stays an `EXISTS`.
  A nested scope (a subquery) and the values of `update()` keep the scalar subquery.
  Like the same aggregate over a local column, `select(Post.title, func.sum(Post.author.id))`
  without `group_by` and `order_by(func.max(Post.author.name))` give Postgres error 42803;
  write a per-row value without the aggregate.
* **String concatenation** has two forms with different `NULL` rules:
  `func.concat(a, " ", b)` is `CONCAT(...)` and reads a `NULL` part as an empty string;
  `a.concat(b)` is `a || b` and is `NULL` when either side is `NULL`.
* Integer `SUM`s come back as `int` (cast to `bigint`), `AVG` as `float` (as a
  `Decimal` over decimal columns).
* Columns through to-one relations are `LEFT JOIN`ed; a to-many column outside an
  aggregate is rejected (it would repeat rows).
* `lock()` works with plain column selects, not with aggregates, window functions,
  `group_by` or `distinct`. `load()` of relations doesn't combine with
  `select()`.
* A condition is a boolean column once labelled: `select(User.name, (User.id > 3).label("big"))`.
* **`CASE`** is `func.case((cond, value), ..., default=v)`: the value of the first true
  condition, else `default` (`None` without one). It is a value like any other, in
  `select()`, `filter()`, `order_by()`, `update()` and `on_conflict(update_values=...)`:

  ```python
  heat = func.case((Post.views >= 50, "hot"), (Post.views >= 20, "warm"), default="cold")
  await Post.objects.filter(heat == "hot")
  # Django's Sum(Case(When(published=True, then=1), default=0)):
  await Post.objects.select(Post.author_id, func.sum(func.case((Post.published, 1), default=0))).group_by(Post.author_id)
  await Post.objects.update(views=func.case((Post.views > 100, 100), default=Post.views))
  ```

  Plain values bind with the type the context expects (the updated field, or the other side
  of a comparison when the `CASE` is on the right of it), else with the type of the first branch that is not a plain value.
  With only plain values, a float or `Decimal` among integers makes the result a float or
  `Decimal`.

## Subqueries: `exists()`, scalar values, `outer()`

Relation paths cover subqueries along declared relations. For anything else a query
set or `select()` goes inside another query, and `outer(col)` (Django's `OuterRef`)
names a column of the enclosing query:

```python
from orm import exists, outer

# EXISTS as a condition (negate with ~), or a boolean column
await User.objects.filter(exists(Post.objects.filter(Post.author_id == outer(User.id), Post.views > 100)))
await User.objects.filter(~exists(Comment.objects.filter(Comment.author_id == outer(User.id))))

# A one-column, at-most-one-row query as a value: (SELECT ...)
latest = (Post.objects.filter(Post.author_id == outer(User.id))
          .order_by(Post.created_at.desc()).select(Post.title)[:1].as_scalar())
await User.objects.select(User, latest.label("latest"))              # Row[User, str]
avg = Post.objects.filter(Post.author_id == outer(Post.author_id)).select(func.avg(Post.views)).as_scalar()
await Post.objects.filter(Post.views > avg)                           # above their author's average
await Post.objects.update(views=Comment.objects.filter(Comment.post_id == outer(Post.id)).select(func.count()).as_scalar())
```

* `outer(Model.col)` refers to the nearest enclosing query over `Model` (a subquery
  of a subquery can reach two levels up). Using `User.id` directly inside a `Post`
  subquery raises a `ValueError` that suggests `outer(User.id)`.
* `outer(Post.author.name)` reads a related row of the enclosing query's row through
  to-one relations, in a correlated scalar subquery. A to-many hop has several rows, so
  it is a `QueryError`.
* A scalar subquery returning more than one row is a database error, as in SQL:
  slice it (`[:1]`) or aggregate. No row gives `NULL` (`None`).
* `in_()` subqueries take `outer()` too. A subquery over the same table as its outer
  query gets an alias (`FROM posts AS s1`), so both stay addressable.
* `qs.exists()` (awaited, a `bool`) stays the way to ask about one query set.

## Window functions

```python
rank = func.row_number().over(partition_by=Post.author_id, order_by=Post.views.desc())
await Post.objects.select(Post.title, rank.label("rank"))
await Post.objects.select(Post.title, func.sum(Post.views).over(order_by=Post.created_at, rows=(None, 0)))  # running total
await Post.objects.select(Post.title, func.lag(Post.views, 1, 0).over(order_by=Post.created_at))
```

* `.over(partition_by=..., order_by=..., rows=(start, end) | range=(start, end))` on any
  `func` call. Frame bounds: `None` unbounded, `0` the current row, `-n` n preceding,
  `n` n following (SQLAlchemy's convention).
* **Shared windows**: `w = window(partition_by=..., order_by=..., rows=...)` defines a
  window once; `.over(w)` uses it, and the query gets `WINDOW w1 AS (...)` with
  `OVER w1` at each use. `.over(w, order_by=...)` / `.over(w, rows=...)` extend it
  (`OVER (w1 ROWS ...)`) when it has no ordering / frame of its own; partitioning comes
  from the window only (Postgres' rules). Each query, subquery or CTE declares the
  windows it uses.

  ```python
  w = window(partition_by=Post.author_id, order_by=Post.created_at)
  await Post.objects.select(Post.title, func.sum(Post.views).over(w), func.avg(Post.views).over(w),
                            func.sum(Post.views).over(w, rows=(-1, 0)))
  ```

  For now the planner takes **one shared window per query, in a query without
  `order_by()`, slicing or `lock()`**, and raises `QueryError` otherwise: sea-query
  (still on its master) keeps a single `WINDOW` per statement and writes it after `ORDER
  BY` / `LIMIT`, where Postgres rejects it. Inline `.over(partition_by=...)` has no such
  limit. The Python API already takes any number of windows, so the limit goes away with
  the SQL builder fix, without API changes.
* Window-only functions: `row_number`, `rank`, `dense_rank`, `percent_rank`,
  `cume_dist`, `ntile(n)`, `lag` / `lead(expr, offset=1, default=None)`,
  `first_value`, `last_value`, `nth_value(expr, n)`. Without `.over()` they raise
  `QueryError`. The aggregates (`count`, `sum`, ...) work over windows too.
* Window functions go in `select()` and `order_by()`. In `filter()`, `having()`,
  `group_by()` or an update they raise `QueryError`, since SQL evaluates them after
  `WHERE`. To filter on one, compute it in a CTE and filter that (below).

## CTEs: `WITH`

`qs.cte(name)` and `qs.select(...).cte(name)` name a query; whatever reads it declares
it, so it ends up once in the statement's `WITH` clause (subqueries included):

```python
ranked = Post.objects.select(Post, rank.label("rank")).cte("ranked")
await Post.objects.from_(ranked).filter(ranked.c.rank <= 3)          # top 3 per author, as Post instances

totals = (Post.objects.select(Post.author_id, func.sum(Post.views).label("views"))
          .group_by(Post.author_id).cte("totals"))
await totals.select(totals.c.author_id, totals.c.views).filter(totals.c.views > 100)   # rows
await User.objects.filter(User.id.in_(totals.select(totals.c.author_id)))

chain = User.objects.filter(User.id == 1).cte(                          # WITH RECURSIVE
    "chain", recursive=lambda c: User.objects.filter(User.id == c.c.id + 1))
await User.objects.from_(chain)
```

* **Joins**: `qs.join(cte, on, outer=False)` is `[LEFT] JOIN <cte> ON <on>`, so a CTE's
  columns come along with each row (django-cte's `cte.join(...)`):

  ```python
  await (User.objects.join(totals, totals.c.author_id == User.id, outer=True)
         .select(User, totals.c.views).order_by(totals.c.views.desc()))
  ```

  A row matching several CTE rows comes back once per match, as in SQL. Writes refuse
  a query set with joins.
* **Subqueries in `FROM`**: `Model.objects.from_(cte)` reads the model's rows from a
  CTE with the model's columns (`Post.objects....cte(...)` or a `select(Post, ...)`),
  so filters, relation paths, `load()`, `count()` and
  `select()` work as usual; its extra columns are `cte.c.<name>`. Writes refuse a
  `from_()` query set.
* `cte.select(...)` queries a CTE with no model: filter, group, order and slice it like
  any `select()`, await it for rows, or use it in `in_()`, `exists()` and
  `as_scalar()`. Columns are named as in `select()` rows (labels, field names).
* `recursive=lambda cte: query` adds the recursive part, `UNION ALL` or
  (`distinct=True`) `UNION`; it must have as many columns as the first part. It reads
  the CTE through `join()` (`User.objects.join(c, User.id == c.c.id + 1)`), or just by
  using `c.c.<col>`, which adds the CTE to `FROM` (`FROM users, chain WHERE ...`).
  `materialized=True / False` adds `[NOT] MATERIALIZED`.
* CTEs work in `update()` / `delete()` filters too. Each prefetch query declares the
  CTEs it reads. Two different CTEs with one name in a statement raise `ValueError`.

Compared with [django-cte](https://github.com/dimagi/django-cte) (4.0), the model is the
same (a CTE wraps a query, `cte.c.x` ↔ `cte.col.x`, `join`, recursion through a
function receiving the CTE, `materialized`), with three differences: a CTE is declared
by whatever reads it instead of `with_cte(cte, select=...)`, so it can't be forgotten
or declared twice; the recursive part is given on its own instead of as `base.union(...)`
inside the function; and `from_(cte)` gives model instances where `cte.queryset()`
gives Django rows typed by the CTE's query.

## Writes

There is no `save()`, and no hidden unit of work. Instances are **read-only snapshots**
of rows: `user.name = "x"` raises (and is a type error), and `User(...)` can't be
constructed. Every write is a statement you await, named after its SQL:

```python
# INSERT ... RETURNING: the instance comes back with id, defaults, timestamps
alice = await User.objects.insert(email="a@x.io", name="Alice")
await alice.posts.insert(title="...", body="...")   # FK filled in

# Many rows: one statement per batch; the row count, or the rows with RETURNING
n = await Post.objects.insert_many(rows)
posts = await Post.objects.insert_many([
    {"author": alice, "title": "Hi", "body": "..."},
    {"author_id": alice.id, "title": "Yo", "body": "...", "views": 3},
]).returning()                                      # list[Post], in input order

# Upsert: INSERT ... ON CONFLICT (email) DO UPDATE / DO NOTHING; the row count, or
# the row(s) with .returning()
alice = await User.objects.insert(email="a@x.io", name="Al").on_conflict(User.email, update=True).returning()
maybe = await User.objects.insert(email="a@x.io", name="Al").on_conflict(User.email, update=False).returning()  # None if it existed
n = await User.objects.insert_many(rows).on_conflict(User.email, update=True, update_fields=[User.name])
# ... with expressions: the existing row is `Post.<col>`, the proposed one `excluded(Post.<col>)`
await Post.objects.insert_many(rows).on_conflict(
    Post.slug, update=True, update_values={"views": Post.views + excluded(Post.views)}
)

# Read, or insert when missing: (row, created). The lookup is one unique constraint.
user, created = await User.objects.get_or_insert(email="a@x.io", defaults={"name": "Al"})

# UPDATE / DELETE over a query: set-based, returns the row count
await Post.objects.filter(Post.author.name == "Alice").update(views=Post.views + 1)
await Post.objects.filter(Post.views < 10).delete()
# ... or the affected rows, with RETURNING (only added when you ask for it)
posts = await Post.objects.filter(...).update(views=Post.views + 1).returning()   # list[Post]
gone = await Post.objects.filter(...).delete().returning()

# Each row to its own values, by primary key: one statement per batch
n = await Post.objects.update_many([{"id": 1, "title": "a"}, {"id": 2, "title": "b"}])
posts = await Post.objects.update_many(rows, batch_size=1000).returning()

# One row, by primary key
await post.update(title="New", views=Post.views + 1)   # UPDATE ... RETURNING; refreshes `post`
await post.delete()
await post.load(reload=True)                     # all scalar fields
await post.load(Post.views, reload=True)         # some fields
```

`load(reload=True)` reads the primary, also on a database with replicas.
`post.delete()` always reads `RETURNING`.
`refresh()` is removed and its name stays reserved.
`on_commit` callbacks and `load_x()` still read replicas: use `.using("primary")` there.

* `insert(...)` gives the instance. `insert_many(...)` and every insert with
  `on_conflict(...)` give the number of rows inserted or updated (no `RETURNING` in
  the SQL), as `update()` does; `.returning()` gives the rows. One row with
  `update=True` gives the instance; with `update=False` (or a `bool` that the type
  checker can't see) the instance or `None`. Many rows give a list in input order,
  without the rows that `update=False` skipped.
* `insert` validates eagerly. Unknown fields, expressions as values and missing
  required fields raise before any SQL runs. Fields left out get their
  `@client_default`, else the column's database default (`DEFAULT` in the VALUES
  list), and `RETURNING` reads them back.
* `insert_many(rows)` splits the rows so that no statement has more parameters than
  the database accepts (65,535 on Postgres, 32,766 on SQLite).
  `insert_many(rows, batch_size=n)` sets a lower number of rows for each statement.
  All the statements run in one transaction (or in the current one). Rows without any
  value go one per statement (`DEFAULT VALUES`). An upsert sees one batch at a time:
  two rows with the same conflict key raise in one statement, but in two batches the
  second one updates the row of the first.
* `insert_many(rows, copy=True)` loads the rows with Postgres
  `COPY ... FROM STDIN (FORMAT binary)`, for large imports. `await` gives the row
  count, not instances. It is one statement: a duplicate key stops the whole load and
  no row is written. Client defaults fill values first; a field must be set in every
  row or in none, because COPY has no per-row `DEFAULT`. `on_conflict()`,
  `batch_size`, SQLite, composed models, models with native write behavior and fields
  that write through an SQL template (other than enums: arrays with a `@db.*` type and
  extension types do) raise. Values fit the column's type (`Int` on a `smallint`
  column); a value out of its range raises. COPY fails when row-level security
  applies to the role (a non-owner without `BYPASSRLS`, or the owner with
  `FORCE ROW LEVEL SECURITY`); use `insert_many(rows)` without `copy=True` there.
  `bench/copy_insert.py` compares it with `insert_many(rows)`: 200,000 posts in
  about 1.2 s against 1.8 s, so about x1.5 (measured 2026-10-09 on a loaded machine,
  Postgres in Docker, best of 3; the batched insert also built the instances).
* `on_conflict(*columns, where=None, update, update_fields=None, update_values=None)`:
  `columns` are the columns of one unique constraint (several for a composite one;
  unpack a list with `*cols`). `update` is required and keyword-only.
  `update=False` keeps the existing row (`DO NOTHING`). `update=True` updates it:
  `update_fields=[Post.title]` copies the proposed values, and
  `update_values={"views": Post.views + excluded(Post.views)}` sets plain values or
  expressions (keys are field names). Both together update the union; a field in
  both raises `TypeError`. The fields you don't name keep their values. With neither
  option, every field passed to the insert except the conflict columns is
  overwritten, so `created_at` isn't reset to `now()`. An empty `update_fields` or
  `update_values`, and either one with `update=False`, raise `TypeError`.
* `on_conflict(*columns, where=cond, update=...)` picks a partial unique index:
  `.on_conflict(Task.shop, Task.task_type, where=Task.deleted_at.is_null(), update=False)` gives
  `ON CONFLICT (shop, task_type) WHERE "tasks"."deleted_at" IS NULL`. The database
  uses the condition to find the index, so the condition goes into the SQL text with
  its values written out, not as parameters. A boolean column (`where=Task.active`)
  gives the bare column, the form SQLite needs. Without `where`, the schema decides:
  when exactly one `@@index([...], unique: true, where: ...)` has these plain
  columns (no expression, no sort), its `where` text goes into the conflict target.
  So `insert()`, `insert_many()` and `get_or_insert()` use that partial index. An
  explicit `where=` wins. With no such index, or with several, there is no predicate.
* `get_or_insert(defaults=..., **lookup)` reads the row that matches `lookup`.
  When there is none, it inserts `lookup` and `defaults` with
  `ON CONFLICT (lookup) DO NOTHING`, and reads again when a concurrent insert wins.
  So concurrent calls give one row, and exactly one call gets `created=True`.
  The reads go to the primary, not to a replica. They see the query set's filters
  but not its limit and offset; when the filters hide the row that has the key,
  `QueryError` says so. On a related set (`post.post_tags.get_or_insert(tag=t)`)
  the link field is part of the lookup. Inside a `REPEATABLE READ` transaction, a
  key that another session commits after the snapshot gives a serialization error.
  The lookup fields must be the fields of one unique constraint or index; Postgres
  raises otherwise. With a partial unique index, the lookup must be unique among the
  rows the query set sees: the reads do not carry the index predicate, so a row
  outside the index can come back (`created=False`), or `MultipleObjectsReturned`
  is raised. A `None` lookup value raises, because `NULL` never conflicts.
* `update()` and `delete()` validate when called and return a statement: awaiting it
  gives the row count (no `RETURNING` in the SQL), `.returning()` gives the rows.
* `update_many(rows)` is Django's `bulk_update` without mutated instances: each row is a
  dict with the primary key and the fields to set, the same fields in every row, plain
  values only. Postgres gets one `UPDATE posts SET title = v.column2 FROM (VALUES ...) AS
  v WHERE posts.id = v.column1` per batch; databases without `UPDATE ... FROM` will get
  Django's `SET title = CASE WHEN id = ... THEN ... END WHERE id IN (...)`. Batches hold as
  many rows as fit in the dialect's `max_params` unless `batch_size` says fewer, and run in one
  transaction. The query set's filters still apply (`alice.posts.update_many(rows)`
  leaves other authors' posts alone); ids that match nothing are skipped.
* A write statement that is never awaited emits a `RuntimeWarning`, the same safety
  net an un-awaited coroutine has.
* `instance.update()` changes exactly the fields named, and the instance then shows
  what the database stored, including expression results and concurrent changes to
  other columns. Instances read through `.using(db)` write back to `db`.

Where the ideas come from:

| | SQLAlchemy 2.0 | Rails (ActiveRecord) | here |
|---|---|---|---|
| insert one | `session.add(User(...)); await session.commit()` | `User.create(email: ...)` | `await User.objects.insert(email=...)` |
| insert many | `await s.execute(insert(User).returning(User), rows)` | `User.insert_all(rows)` | `await User.objects.insert_many(rows)` |
| upsert | `pg_insert(User).on_conflict_do_update(...)` | `User.upsert_all(rows, unique_by: :email)` | `.insert_many(rows).on_conflict(User.email, update=True)` |
| update a set | `update(User).where(...).values(...)` | `User.where(...).update_all(...)` | `await User.objects.filter(...).update(...)` |
| update rows to different values | `session.execute(update(User), rows)` | `User.update(ids, rows)` (one by one) | `await User.objects.update_many(rows)` |
| update a row | mutate + flush (unit of work) | `user.update(name: "B")` | `await user.update(name="B")` |
| delete | `delete(User).where(...)` / `session.delete(u)` | `delete_all` / `user.destroy` | `await qs.delete()` / `await user.delete()` |

Statement names and `RETURNING` follow SQLAlchemy Core. Row-level `update(**values)`,
`insert_all` / `upsert_all` with `unique_by`, and choosing the update columns follow
Rails. What's left out on purpose: SQLAlchemy's session and dirty tracking, Rails'
`save` / `attr=` + `save`, and callbacks. A write happens only where the code says
`await ...insert/update/delete`.

The column-role extensions change these writes (see `schema-extensions.md`).
`@timestamps.updated_at` and `@locking.version` fields are set on each update.
On a `@soft_delete.deleted_at` model, `delete()` soft-deletes, and `hard_delete()`, `undelete()`, `all_with_deleted()` and `deleted_only()` exist (from the soft-delete extension's generated code).
Plain reads hide deleted rows (the `"deleted"` default); `all_with_deleted()` and `deleted_only()` bypass only that default. A to-one reference to the model is `T | None`. See [Behavior changes](../extensions/schema-extensions.md#behavior-changes).
A stale versioned instance write raises `orm.VersionConflict`.

### Connections

```python
db = await orm.connect("postgres://user:pw@host/db", max_connections=10)   # also sqlite:///path, sqlite://:memory:
other = await orm.connect(url, default=False, registry=registry)          # not the default database
orm.get_database()                     # the default database; NotConnected before connect()
await db.execute("VACUUM")             # raw SQL, returns the row count; no ORM checks apply
await db.create_tables(); await db.drop_tables()
await db.close()
User.objects.using(other)              # run a query set on another database
```

### Transactions

```python
async with db.transaction():          # commit on success, rollback on exception
    async with db.transaction(): ...  # nested: savepoint
    async with db.transaction(savepoint=False): ...  # nested: joins the outer transaction, no savepoint
```

The current transaction lives in a `ContextVar`, so queries inside the block use it
without passing it around. Tasks started inside the block inherit it.

```python
async with db.transaction():
    order = await Order.objects.insert(...)
    await db.on_commit(lambda: send_receipt.delay(order.id))   # after COMMIT only
```

Nested transactions must run in stack order on their shared connection. Overlapping
siblings (for example, `asyncio.gather`), or statements on an enclosing
transaction while a child savepoint is open, raise `QueryError`. Run siblings one after
the other or give each task its own outer transaction. Ordinary nesting remains allowed.

`await db.on_commit(fn)` calls `fn()` after the outermost transaction on `db` commits, and awaits the result when it is awaitable.
A rollback drops the callback. A rolled-back savepoint drops only the callbacks registered inside it.
Outside a transaction, `fn()` runs at once.
Callbacks run in registration order, outside the transaction.
An error in a callback goes to the caller of `transaction()`, and the later callbacks do not run; the transaction is already committed.
`on_commit` is a coroutine: without `await`, the callback is not registered (Python warns "coroutine was never awaited").
Transactions and callbacks are kept for each database: a callback on `a` inside a transaction on `b` inside a transaction on `a` waits for `a`'s commit.
A task that the transaction started and that calls `on_commit` after the transaction ended raises `TransactionRequired`.

#### Failed statements

A statement that fails marks its transaction (or savepoint) failed, also when your code catches the error.
This is the same on Postgres and SQLite.
Later statements raise `TransactionAborted`, a `DatabaseError` chained (`__cause__`) to the first error.
A block that exits cleanly after a failure rolls back, drops the `on_commit` callbacks, and raises `TransactionAborted`.
Catch errors only around a savepoint:

```python
async with db.transaction():
    await Order.objects.insert(...)
    try:
        async with db.transaction():          # savepoint: its rollback clears the failed mark
            await Order.objects.insert(...)   # may raise IntegrityError
    except orm.IntegrityError:
        pass
    await Audit.objects.insert(...)           # the outer transaction still works
```

`db.transaction(savepoint=False)` joins the enclosing transaction with no savepoint.
An error that leaves such a block marks the enclosing transaction failed.
On an outermost block the parameter has no effect.
A multi-statement call of the ORM itself (a composed insert, a related `set()` or `insert()`) opens no savepoint in an open transaction: its failure marks the transaction.
Wrap the call in a nested transaction to recover.
Outside a transaction it runs in a transaction of its own.

### Database errors

An `orm.DatabaseError` (and its subclasses `IntegrityError`, `LockNotAvailable`, `TransactionAborted`) has three attributes:

* `sqlstate`: the five-character SQLSTATE, for example `"23505"` (unique), `"23503"` (foreign key), `"23502"` (not null), `"23514"` (check).
  SQLite constraint failures get the same Postgres codes; other SQLite errors give `None`.
* `constraint`: the name of the violated constraint (Postgres only), for example `"users_email_key"`.
* `detail`: the database's DETAIL line (Postgres only), for example `"Key (email)=(a@example.com) already exists."`.

Each is `None` when the database did not give it.

```python
try:
    await User.objects.insert(email=email, name=name)
except orm.IntegrityError as e:
    if e.constraint != "users_email_key":
        raise
    ...  # the email is taken
```

The ORM does not retry a transaction.
A transaction that fails with a serialization failure (`40001`) or a deadlock (`40P01`) can run again:

```python
async def transfer(db, a, b, amount, attempts=3):
    for attempt in range(attempts):
        try:
            async with db.transaction():
                ...  # the whole transaction, reads included
            return
        except orm.DatabaseError as e:
            if e.sqlstate not in ("40001", "40P01") or attempt == attempts - 1:
                raise
            await asyncio.sleep(0.05 * 2**attempt)
```

Retry the whole transaction, never one statement in it: Postgres aborts the transaction on these errors.
`40001` occurs at the `REPEATABLE READ` and `SERIALIZABLE` isolation levels; on a primary the default `READ COMMITTED` gives deadlocks only.
A hot-standby replica can give `40001` at `READ COMMITTED` too (I recall this from the PostgreSQL hot-standby documentation; not run here).

### Protected writes

`@@protected_write` is an application-level check in the ORM. It does not protect the database.
Raw SQL (`db.execute`), migrations, other ORM processes without this schema, and other database clients can still write.
For database-level protection, use `@@trigger` or database grants.

```python
with orm.allow_writes(Post):               # a sync `with`: it does no I/O
    await post.update(title="new")

await Post.objects.filter(...).update(title="x")   # raises orm.WriteProtected
```

Every ORM write to a `@@protected_write` model fails outside `orm.allow_writes(...)` with `orm.WriteProtected`:
`insert`, `insert_many`, upserts, `update`, `update_many`, `delete`, instance writes and composed writes.
The check is on the table that the SQL writes, so `post.tags.add()` needs `allow_writes(PostTag)` when `PostTag` is protected.
The scope is a `ContextVar`, like the transaction: tasks started inside it get it.
A nested scope adds its models to the outer ones. `allow_writes` starts no transaction.
A query set is lazy, so the scope applies where the write is awaited, not where it is built:
`u = qs.update(...)` inside the scope and `await u` outside it raises `WriteProtected`.
Do not `yield` inside `allow_writes` in an async generator: when the consumer stops early, the scope stays open in the consumer task.
`orm.hooks.prepare_insert` and `prepare_update` also check protection, so a file field uploads nothing for a rejected write.
See `docs/schema/schema.md`, "Protected writes".

### Locks

```python
async with db.transaction():
    post = await Post.objects.lock().require().get(Post.id == 1)                      # FOR UPDATE
    await Post.objects.filter(...).lock(exclusive=False)                     # FOR SHARE
    todo = await Post.objects.filter(~Post.published).lock(skip_locked=True).limit(10)
    await Post.objects.lock(nowait=True).get(...)       # raises orm.LockNotAvailable if locked
    await post.qs().lock()                              # reload and lock this row
    if not await job.qs().lock(skip_locked=True):       # []: another worker has it
        ...
    await db.lock("import:42")                          # advisory lock on a name, not a row
    got = await db.lock(42, exclusive=False, nowait=True)   # False instead of waiting

async with db.lock("shop:7:sync", session=True, timeout=5):  # no transaction needed
    await call_shopify(...)
```

* `lock(exclusive=True, *, nowait=False, skip_locked=False)`: `exclusive` is
  `FOR UPDATE`, otherwise `FOR SHARE`. Only the model's own rows are locked
  (`FOR ... OF <table>`), never rows joined by `load()`.
* `post.qs().lock(exclusive=True, *, nowait=False, skip_locked=False)` reloads the row with the options of the query set's `lock()` and gives a list with the instance.
  It locks the whole row. A row that is missing, or locked elsewhere with `skip_locked`, gives `[]` and leaves the instance unchanged.
  `post.qs()` applies no query default to the row of the post, so it also locks a soft-deleted row.
* Row locks and `db.lock(key)` without `session=True` are held until the transaction
  ends, so they raise `orm.TransactionRequired` outside `db.transaction()`. `count()` / `exists()` /
  `update()` / `delete()` on a locked query set raise `QueryError` (writes lock the rows
  they change anyway).
* SQLite has no row locks and no advisory locks: `lock()` and `db.lock()` raise `QueryError` there.
* `db.lock(key)` is a transaction-scoped Postgres advisory lock. Postgres keys are
  64-bit integers; a `str` key is hashed to one in Python (first 8 bytes of BLAKE2b,
  signed big-endian).
* `async with db.lock(key, session=True, timeout=5):` is a session advisory lock: it
  holds the lock for the block, with no transaction, so the block can make slow calls
  (HTTP) without an open transaction. The lock has a connection of its own, opened from the
  same URL with the pool's TLS and connect settings and closed on release. It is not
  taken from the pool, so a lock that waits never holds a pool slot. Each lock costs one
  connect (estimated 1–5 ms) and counts against the server's `max_connections`. Session
  locks are unsafe behind PgBouncer in transaction mode. It waits at most `timeout` seconds (forever when `None` or `inf`, not at all with
  `nowait=True`; at most 2147483.647, the Postgres limit) and raises
  `orm.LockNotAvailable` when another session still holds the lock. `timeout` without
  `session=True` raises `TypeError`. The lock is released when the block ends, also on
  an error. When the unlock fails, the connection is closed, so the server releases the
  lock, and the block's own result or error stays. A task cancelled while it waits
  cancels the wait on the server.
  Reads in the block go to a replica when the database has replicas: read in a
  transaction or with `.using("primary")` to see the last write of the previous holder.
* Optimistic locking (version columns) is not in the core.
  Select the `orm-locking` extension and mark the field `@locking.version`; see `schema-extensions.md`.

### Read replicas

```python
db = await orm.connect(primary_url, replicas=[replica1_url, replica2_url])
users = await User.objects.filter(...)                  # a replica, in turn
fresh = await User.objects.using("primary").get(...)    # the primary
```

* Reads (`select`, `count`, `exists`, prepared queries) outside a transaction go to the next replica, in turn.
* Writes, raw `db.execute`, migrations, and every statement inside `db.transaction()` go to the primary.
* `db.primary` is a view of the database without its replicas; it shares the transactions of `db`.
  `.using("primary")` is `.using(<the query set's database>.primary)`, resolved when it is called.
* A replica can lag behind the primary. To read your own write, read in the same transaction or use `.using("primary")`.
* No health checks or failover: an error on a replica goes to the caller.
* `max_connections` applies to each pool. `db.close()` closes all of them.

### Tenants and row-level security

```python
with db.tenant(shop.id):                 # a sync `with`: it does no I/O
    orders = await Order.objects.all()    # RLS policies see current_setting('app.tenant')
```

* Every transaction on `db` in the block first runs `SELECT set_config('app.tenant', '<id>', true)`, the same as `SET LOCAL app.tenant = ...`, with the value bound as a parameter.
* A statement outside a transaction runs in a transaction of its own: `BEGIN`, `set_config`, the statement, `COMMIT`. That is four round trips instead of one (estimated); put many statements in one `db.transaction()`.
* A transaction that is already open keeps its setting. Savepoints use the setting of their transaction.
  `db.tenant(x)` raises `QueryError` when this task has an open transaction on `db` that began under a different tenant (a transaction with no tenant counts as different); re-entering the same tenant is allowed. Open the transaction inside the block, or give the helper a transaction of its own in another task.
* The setting ends with each transaction, so pooled connections keep no tenant.
  A connection that had a tenant reads `''` (not `NULL`) for the setting after that, so a policy should read `NULLIF(current_setting('app.tenant', true), '')`; `tenant("")` raises `ValueError`.
* Raw `db.execute(...)` in the block also runs in a transaction of its own, so a statement that cannot run in a transaction (`CREATE INDEX CONCURRENTLY`, `VACUUM`) fails there; run it outside the block.
* A composed model (model composition) stores its rows in a table for each level: each of these tables needs its own policy, for example a child policy `USING (EXISTS (SELECT 1 FROM parent p WHERE p.id = child.id))`.
* Replicas get the same setting. Session locks (`session=True`) do not.
* The policy must read the setting, for example `USING (shop_id::text = current_setting('app.tenant', true))`. The ORM does not create policies. A superuser and the table owner bypass RLS unless the table has `FORCE ROW LEVEL SECURITY`.
* SQLite raises `QueryError`.
* `orm.scope(shop=...)` is the application-side filter (see `docs/schema/selection-and-defaults.md`, "Scope values").

### Finding N+1 queries: `orm.debug`

The ORM never loads a relation by itself, so an N+1 comes from explicit code: an instance `load(...)` call or a query in a loop.
`orm.debug.n_plus_one` finds it:

```python
with orm.debug.n_plus_one(threshold=5, fail=True):
    for c in customers:
        await c.load(Customer.person)
# orm.debug.NPlusOne: 20 queries with one shape `SELECT ... FROM "person" WHERE "person"."id" = $1 ...`
#   at app/views.py:42; use load(Customer.person)
```

* The scope counts writes by call, reads by shape, each counted once per call.
  A read's shape is its SQL with placeholders, without values.
  A write (`insert`, `insert_many`, `update_many`, attach, COPY) counts under a key of the call: `insert:{model}:{fields}:{conflict}`, `update_many:{model}:{fields}:{filters}`.
  So the batches of one `update_many`, or the chunks of one `insert_many`, are one query, and a loop of `insert_many` calls with different row counts is an N+1.
  The same shape counts once per engine call, so the prefetch chunks of one call are one query.
  The report prints the first real SQL of the key.
  The statements that a query hook sends do not count.
  Tasks started in the scope count too, and so do the queries of an inner scope.
  An inner scope cannot raise the threshold of an outer scope; the outer scope also counts the inner queries.
  The pages of one ORM loop (`batches()`, `iterate()`, the chunks of `in_bulk()`) count as one query when they have one shape.
* The call site is the line that awaits the query (`await qs`, `await p.customers.all()`).
  A query that `asyncio.gather()` or `create_task()` runs reports the line that started the event loop.
* When the block ends, a shape that ran more than `threshold` times (default 5) raises `orm.debug.NPlusOne` with `fail=True`, or gives an `orm.debug.NPlusOneWarning`.
  The exception and the `with ... as report` value carry the report: each shape, its SQL, its count, the call site of its first query and the fix.
* The report is an `orm.debug.Report`: `.shapes` and `.repeated` hold `Shape(key, sql, count, site, fix)` values, and `.message()` gives the text above.
* The fix is `load(...)` for a repeated instance `load(...)` and for a repeated unchanged to-many or many-to-many query (`await post.comments`, `await post.tags`).
* The call site and the SQL text are captured only inside the scope.
  Outside it, each query pays one `ContextVar` read (about 15 ns, measured).
* In a test suite, add `pytest_plugins = ["orm.testing"]` to `conftest.py`.
  The `n_plus_one` fixture counts the whole test and fails it at teardown; set `n_plus_one.threshold` to change the threshold.

* To mock `db.lock` in tests, use `AsyncMock` for `await db.lock(k)`, and an async context manager for `session=True`: `lock` is not a coroutine function (`create_autospec(Database)` makes a mock that cannot be awaited).

### Raw SQL and query plans

```python
rows = await db.fetch("SELECT id, email FROM users WHERE created_at > $1 AND name = $2", since, "Ann")
# [{"id": 7, "email": "ann@example.com"}]
n = await db.execute("VACUUM ANALYZE posts")        # one or more statements, no parameters; rows affected

print(await Post.objects.filter(Post.author_id == 7).explain())              # EXPLAIN
print(await Post.objects.filter(Post.author_id == 7).explain(analyze=True))  # runs it: real times and rows
```

* `db.fetch(sql, *params)` runs one query and gives a list of dicts by column name (a repeated column name keeps the last value).
  Placeholders are `$1, $2, ...` on Postgres and `?` on SQLite.
* A parameter's type comes from its Python value: `int` is `bigint`, `str` is `text`, `dict` and `list` are JSON, and `datetime`, `date`, `Decimal`, `UUID` and `bool` have their own types.
  Cast in the SQL where a column needs another type: `WHERE id = $1::uuid` for a `str`.
  A `list` goes as `jsonb`, so `= ANY($1)` raises `42809`; use `WHERE x IN (SELECT jsonb_array_elements_text($1))` and compare as text.
  A naive `datetime` raises `TypeError` (use an aware one), and `bytes` raises `TypeError`.
* Cells come back by the column type that the database reports.
  Postgres gives `bigint`, `integer`, `smallint`, `double precision`, `real`, `boolean`, text types and enums, `timestamptz`, `date`, `uuid`, `json`, `jsonb`, `numeric` and arrays of them.
  Any other type (`timestamp`, `interval`, `bytea`, ...) raises `DatabaseError`; cast it in the SQL (`::text`, `::timestamptz`).
  SQLite gives each value by its storage class (`int`, `float`, `str`, `None`); a blob raises.
* `fetch` runs in the current transaction, and query hooks see it.
* After a schema change (`ALTER TABLE ...`), the first call of a cached Postgres statement that now reads another result shape raises `0A000` (`cached plan must not change result type`).
  The next call prepares the statement again and works.
* `qs.explain(analyze=False)` gives the plan of the query set's SELECT as text, not of its prefetch queries.
  On Postgres, `analyze=True` runs the query with `EXPLAIN (ANALYZE, BUFFERS)`; a query set with `lock()` raises `QueryError` there, because the run would take the row locks.
  On SQLite, it gives `EXPLAIN QUERY PLAN`, each step indented under its parent. SQLite has no `analyze`, so `analyze=True` raises `QueryError`.

### Query hooks and OpenTelemetry

`db.on_query(hook)` calls `hook(event)` after each statement that the database runs, and gives a function that removes the hook:

```python
def log_slow(e: orm.QueryEvent) -> None:
    if e.duration > 0.1:
        logger.warning("%.0f ms, %d rows: %s", e.duration * 1000, e.rows, e.sql)

remove = db.on_query(log_slow)
```

* `orm.QueryEvent` has `sql` (the SQL as sent: ORM-built SQL and `db.fetch(sql, *params)` have placeholders; `db.execute(sql)` goes as written, with any literal value in it), `start` (Unix seconds), `duration` (seconds), `rows`, `error` and `raw` (`True` for `db.execute(sql)`, which takes no parameters).
  `rows` is the rows returned, or the rows affected by a statement that returns no rows.
  `error` is the first line of the database's message when the statement failed; the ORM call still raises.
  The line has no `DETAIL` and so no key value, but it can still hold an input value (`invalid input syntax for type integer: "abc"`).
  `db.lock(..., nowait=True)` gives `rows` 1 when it got the lock and 0 when it did not.
* Each call to the database gives one event per statement: the prefetch queries that `load()` runs, each `update_many` batch, each `db.execute`, also `db.execute("A; B")`.
  A call keeps its events until it ends, so a call with many statements holds them all in memory (the review reported 200k events at about +113 MB; not measured here).
  Reads sent to a replica and the lock and unlock of `db.lock(..., session=True)` give events too.
  `COMMIT`, `ROLLBACK` and the `set_config` statements of `db.tenant()` give no event.
* The hooks belong to the database: `db.primary` shares them.
* The engine times the statement in Rust, from the send to the last row, with the wait for a connection and, under `db.tenant()`, its transaction, without the conversion to Python objects.
* The hook runs after the ORM call ends, in the task that made the call.
  So context variables, for example the current span or a request id, are those of the caller.
  A hook can be a coroutine function: the ORM awaits the result.
* Every hook gets every event of the call, also when a hook raises.
  When the call succeeded, the first hook exception propagates to the caller after all hooks ran; the other hook exceptions are notes on it (`add_note`).
  The statement has already run then: an autocommit `insert` is committed when its hook raises.
  When the call failed, the exception of the call propagates, and the hook exceptions become notes on it.
* The statements that a hook sends give no event and no N+1 count, so a hook that writes an audit row does not call itself.
* When the caller cancels a call (`asyncio.timeout`, a closed request), each statement still in flight gives an event with `error` set to `"cancelled"`, `rows` 0 and the time up to the cancel.
  The event does not tell whether the database finished the statement.
* Each call to `on_query` is one registration, also for the same function.
  Its remove function takes out that registration, and a second call to it does nothing.
* With no hook and no N+1 scope, a query pays one list check and one `ContextVar` read.
  With a hook, each call creates a trace and each statement records one event.
  A prepared `get`, median of 9 rounds of 2000 calls (measured, loaded machine): SQLite 163 µs with an empty hook and 152 µs without; Postgres 201 µs and 192 µs. The hook adds about 10 µs.

`orm.otel.instrument(db)` gives each statement a client span, and gives a function that stops the spans.
It needs `opentelemetry-api` (`pip install 'orm[otel]'`); configure the SDK and the exporter as usual.
`instrument(db, tracer=t)` uses the tracer `t` instead of `get_tracer("orm")`.

* The span starts and ends at the statement's times. Its parent is the current span of the caller.
* The span name is the SQL operation (`SELECT`, `INSERT`, ...).
  The attributes are `db.system.name` (`postgresql` or `sqlite`), `db.operation.name`, `db.query.text` (as in the event, cut at 4096 characters then `...`; left out for `db.execute(sql)` unless `instrument(db, raw_sql=True)`, because its SQL can hold literal values) and `db.response.returned_rows`.
* A failed statement gets the `ERROR` status and the database's message.

### Hooks for packages: `orm.hooks`

A package that changes writes and reads from outside the ORM (for example
`orm-file-storage`) uses these public hooks, not private names:

* `prepare_insert(qs, values)` checks one insert as `insert()` does (required fields,
  conversion, native planning) without SQL or I/O. `await prepared.execute(values)`
  inserts it, by default with the checked values.
* `prepare_update(qs, values)` checks `qs.update(**values)` the same way.
  `prepared.unique` is true when the engine proves that the filters pin one row by a
  non-null primary key or unique field. `await prepared.execute(values, returning=False)`
  runs it.
* `decode_field(Model, name, decode)` reads the loaded value of a field as
  `decode(value)`. The instance keeps the stored value for writes and filters.

### Side effects before or after a write

The ORM has no write signals and no `before_update` / `after_update` hooks.
Put each side effect where it belongs:

* A business side effect (send a mail, queue a task, clear a cache): a service function that does the write and then the side effect.
  Use `db.transaction()` when both must commit together.
* A rule for every write path, also raw SQL and other clients: a database constraint or trigger.
* Audit and metrics: `db.on_query(hook)` sees each statement after it ran. It can not change the statement.

A custom query-set class (`--query-set`) can override `update()`, and `post.update()` uses the override because it goes through `Model.objects`.
This is not a full hook: `update()` gives a lazy `Update` that runs only when awaited, so an "after" step must wrap that object.
`update_many()`, upserts (`on_conflict`) and raw SQL do not call the override.

## Model metadata and test factories

The ORM has no factory library. It gives the two things a factory library needs:

* `orm.describe(Model)`: plain data about the model.
  `primary_key` and relation `from`/`to` are always lists of field names in key order, even for a single field (for example, `["id"]`).
  `fields` gives per field the name, column, schema type, `python_type` (an enum field gives its enum class), `nullable`, `array` (with `element_type`), `max_length`, `primary_key`, `unique`, `default` (`"database"`, `"client"` or `None`) and `insert` (whether `insert()` takes it).
  `relations` gives the kind (`belongs_to`, `has_one`, `has_many`, `many_to_many`), the target class, the `from`/`to` fields, the `through` model and `nullable`.
  `unique` lists the unique keys, the primary key first.
* The insert path: `await Model.objects.insert(**values)`, which also takes a related instance for a to-one key (`author=user`).

Instances come only from the database, so a factory builds the insert values, not an instance.
With factory_boy, `_build` gives the values and `_create` gives the insert, to await:

```python
class PostFactory(factory.Factory):
    class Meta:
        model = Post

    title = factory.Sequence(lambda n: f"post {n}")
    body = "..."
    author = factory.SubFactory(UserFactory)  # a User instance, awaited by the caller first

    @classmethod
    def _build(cls, model_class, *args, **kwargs):
        return kwargs

    @classmethod
    def _create(cls, model_class, *args, **kwargs):
        return model_class.objects.insert(**kwargs)  # a coroutine: `await PostFactory.create()`
```

factory_boy has no async support, so a `SubFactory` with `create` gives a coroutine; build related rows first, or use a small async factory over `describe()` (see `tests/test_query_api.py`).

### Errors

`DatabaseError`, `QueryError` (a `ValueError`), `SchemaError` (a `ValueError`) and
`WriteProtected` come from the native engine. The other errors derive from `orm.ORMError`.

| Error | Raised when |
|---|---|
| `orm.DatabaseError` | the database rejects a statement; `IntegrityError` (constraints), `LockNotAvailable` (`nowait`) and `TransactionAborted` (failed transaction) derive from it |
| `orm.QueryError` | the ORM rejects a query before SQL (an unsupported feature, a bad cursor) |
| `orm.SchemaError` | a schema fails to compile or define |
| `Model.DoesNotExist`, `Model.MultipleObjectsReturned` | `get()` finds no row or more than one (`orm.DoesNotExist` and `orm.MultipleObjectsReturned` are the bases) |
| `orm.NotLoaded` | a relation or field was not loaded |
| `orm.NotConnected` | no default database |
| `orm.TransactionRequired` | `lock()` or `db.lock()` outside a transaction |
| `orm.WriteProtected` | a write to a `@@protected_write` model outside `allow_writes` |
| `orm.debug.NPlusOne` | the N+1 finder with `fail=True` |

## The FFI boundary

```
QuerySet ──(IR json + params list)──▶ Engine.run ──▶ planner (sea-query) ──▶ driver ──▶ Postgres
         ◀──(list[tuple] + prefetched rows, one pass)───────────────────────────────┘
```

* The schema IR is sent once, at `connect()`. Query IR is JSON with literals (and
  `LIMIT` / `OFFSET`) in a separate positional `params` list, converted to SQL values by the column type they're
  compared with (`languages/python/native/src/convert.rs`).
* The IR talks about models, fields and relation paths only. Joins, `EXISTS` and
  aliases stay in `plan.rs`; SQL text and driver types stay in `db/`.
* Rust builds the result objects (`languages/python/native/src/build.rs`). `Schema` holds each model's
  class (passed by `Registry.native()`), and an instance is made as `cls.__new__(cls)`
  would make it, its fields written straight into its `__dict__`: no Python code runs per
  row. joined objects, prefetched lists, the reverse to-one (`post.author`)
  and `_db` (for `using()`) are attached in the same pass; `select()` rows are built as
  `Row` objects; `insert` / `.returning()` rows are instances too.

Rough numbers (measured 2026-10-02 on a 4-vCPU Linux VM, release build, localhost TCP, 1000-row reads): building a query set and its
IR costs ~15 µs of Python. A 1-row read is ~0.1 ms over a bare `SELECT 1` on the same
connection. Reading 1000 posts takes ~1.9 ms, and
1000 posts + author via `load(Post.author)` ~3.1 ms. Building instances in
Rust took these from ~2.3 / ~4.3 ms
(`bench/engine_bench.py`, numbers in
[`bench/RESULTS.md`](../../bench/RESULTS.md#instances-built-in-rust)).

## Decisions

1. **Expressions only** for filters, no `field__lookup=` strings. `update()`
   and `insert()` take keyword arguments, typed per model in the stub.
2. **Awaitable query sets**: `await qs`, `await qs.first()`, `async for`.
3. **`EXISTS` for every relation hop in filters**, with Django's grouping rules (see
   above). JOINs only for `load()` of to-one relations and `order_by`, and only along to-one
   relations. Ordering by a to-many column is rejected.
4. **Explicit both sides of a relation** in the generated code (`BelongsTo` +
   `HasMany`) rather than Django's implicit `related_name`. The schema names both.
5. **Explicit writes, read-only instances** (no `save()`). Defaults live in
   the database (DDL `DEFAULT`) and come back via `RETURNING`. A schema
   `@client_default` fills an omitted insert value natively. Callable defaults
   declared in Python fill an omitted value at the same step, before native
   transforms and validators; an explicit value, also `None`, wins over both.
6. **Instance equality** is by model class and primary key (Django).
7. **One default database** set by `connect()`, `.using(db)` to override, like Django's
   `using()`.
8. `db.create_tables()` / `drop_tables()` stay as a development helper (idempotent
   `IF NOT EXISTS` DDL). Evolving databases use migrations: see [`schema.md`](../schema/schema.md).
   An existing database is adopted with `await orm.migrations.pull(db)` (its schema file),
   `Migrator.baseline()` and `Migrator.drift()`
   (see [`schema.md`](../schema/schema.md#pull-baseline-and-drift-a-live-database-under-migrations)).
   A migration may hold a `data.py` with `async def run(db)`, run by `Migrator.upgrade()`
   and `python -m orm migrate` in the migration's transaction
   (see [`schema.md`](../schema/schema.md#data-migrations)).
   `Migrator.downgrade(..., force=True)` reverts a baselined migration, which it refuses otherwise.
9. **Async only**, no sync API.
10. **No general plan cache.** [`performance.md`](../performance.md) keeps compiled-plan
    caching out until profiling shows a benefit; `prepare()` and the driver's statement
    cache exist.

## Drivers and dialects

```
planner ──sea-query statement──▶ db::build(dialect) ──(SQL, [Value])──▶ Driver ──▶ database
   └── checks orm_contracts::dialect::Capabilities
```

* **SQL building is per dialect.** The dialects are PostgreSQL and SQLite.
  `orm_contracts::dialect` (re-exported as `orm_core::dialect`) lists what each dialect
  can do (`RETURNING`, `ON CONFLICT`, `ILIKE`, row-lock flavours, `DISTINCT ON`,
  `UPDATE ... FROM (VALUES ...)`, savepoints, `max_params`). The planner
  checks it, so a query needing a missing feature raises `QueryError` naming it (or is
  emulated: `icontains` becomes `LOWER(x) LIKE ...` without `ILIKE`) instead of sending
  SQL the database rejects. Schema definition checks it too.
  `orm.connect(url, _disable=("ilike", "update_from_values"))` switches capabilities off,
  so the tests run those fallback paths on Postgres.
* **Execution is per driver**, behind the traits in `crates/engine/src/db/mod.rs`: `Driver`
  (a pool), `Executor` (run SQL with values, on the pool or in a transaction),
  `Transaction` (commit, rollback, savepoints via `begin()`), `RowSet` (decode rows by
  the schema's column types, straight into Python tuples). `db::connect` picks the
  driver from the URL scheme. Adding a database means a dialect, its capabilities and
  one driver; the planner and everything above stay as they are.
* **Postgres** uses tokio-postgres with a deadpool pool: binary protocol, parameter
  types stated in each `Parse` (taken from the value, which the planner typed by
  column), statements prepared once per connection and cached, `TCP_NODELAY` on.
  `sslmode` works as in libpq: `disable`; `prefer` (default) / `require` encrypt
  without checking the certificate; `verify-ca` / `verify-full` check it against the
  webpki roots (both also check the host name). A transaction that is dropped without
  commit or rollback closes its connection rather than returning it to the pool.
* **SQLite** uses rusqlite, with each connection on its own thread. URLs are
  `sqlite://:memory:` and `sqlite:///absolute/path.db`. It has no arrays, no native
  enums, no row locks and no advisory locks; see [`schema.md`](../schema/schema.md#sqlite).

SeaORM was the engine until this point; only its sea-query and pool were in use (its
entities, relations and loaders need Rust types compiled per schema, and its
`find_also_related` / `load_many` are what `load()` already
do). Replacing it with tokio-postgres made most operations 5–33% faster with TLS off (with
TLS on, the TLS cost hides the difference)
(`bench/engine_bench.py`, numbers in [`bench/RESULTS.md`](../../bench/RESULTS.md#engine-seaorm-vs-tokio-postgres)).

## Query inspection counters

`inspect()` returns immutable `QueryInspection` results. Each
`StatementInspection` exposes flat `has_batching`, `joins_count`,
`correlated_count`, `uncorrelated_count`, and `cte_count` fields. CTE definitions
count separately from subqueries; a recursive CTE counts once, and subqueries
inside its body still contribute to the subquery counts.

## Not done yet

* A MySQL driver.
* `SEARCH` / `CYCLE` clauses for recursive CTEs, filtering on window functions without
  a CTE.
* Several shared windows per query, and shared windows with `ORDER BY` / `LIMIT`: needs
  a fix in sea-query (or our own SELECT writer); see Window functions.
* A join through a many-to-many relation (it would repeat rows; `load()` uses an
  `IN (...)` query).
