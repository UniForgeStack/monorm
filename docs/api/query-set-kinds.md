# Query-set kinds

The Python and TypeScript APIs have four kinds of query set and four related query objects.
Each kind leaves out the methods that do not fit its result.

## Kinds

| Kind | Get it from | Runs alone | Use |
|---|---|:---:|---|
| `QuerySet` (or a custom subclass) | `Post.objects` | yes | Every normal read and write. |
| Relation query set | `User.posts.objects` (class path) | no: `TypeError` outside `load()` | A `load()` item: filter, order, slice, `.label()`, `.as_prefetch()`. |
| `RelatedSet` / `ManyRelatedSet` | `u.posts` | yes | The rows of one parent. `insert()` fills the key; `add()` / `remove()` for many-to-many. |
| `BoundQuerySet` | `post.qs()`, `monorm.of(posts)` | yes | Fills the same objects in place. |
| `Select` (not a query set) | `qs.select(...)` | yes | `Row`s, `group_by()`, `having()`, `distinct()`, `count()`, `exists()`, `prepare()`, `explain()`, `join(cte)`, `from_(cte)` (TypeScript `from(cte)`), `paginate()`, `iterate()`, `batches()`. Its `first()`, `last()`, `get()` and `[n]` (TypeScript `at(n)`) are one-row queries. |
| One-row query (not a query set) | `first()`, `last()`, `get()`, `qs[n]`, `qs.at(n)` | yes, when awaited | One row or `None`. No query runs until it is awaited; it also works in `load()`, `prepare()` and `inspect()`. |
| `Prepared` / `PreparedSelect` (not query sets) | `qs.prepare()`, `select.prepare()` | yes, per call with values | A read that is built once; each call binds the `monorm.param()` values. Python: `await p(**values)`, `.get()`, `.first()`, `.count()`, `.exists()`, `.require()`; a `PreparedSelect` also has `.scalar()` and `.scalars()`. TypeScript: `.all(values)` and the same methods. |
| `PreparedOneRowQuery` (not a query set) | `qs.first().prepare()` and the other one-row queries | yes, per call with values | One row or `None` per call. Python: `await p(**values)`; TypeScript: `.run(values)`. |

## What each kind leaves out

| Kind | Leaves out | Why |
|---|---|---|
| `QuerySet` | `group_by()`, `having()`, `distinct()` | They change what a row means, so the result must be `Row`s: use `select()`. |
| `Select` | `lock()` | A select returns computed rows, not objects: to lock, use `QuerySet.lock()` with `load()`. |
| `Select` | `paginate()` over an aggregate or non-root order term, over `distinct()` without columns, over a slice | The cursor reads the order columns from the rows, so they must be columns of the model itself, in the select list. |
| Relation query set | running alone, writes, `select()`, `paginate()`, `lock()` | It is only an item for `load()`; use `Post.objects` for a standalone query. |
| `RelatedSet` | `.label()`, `.as_prefetch()` | These belong to `load()` items. |
| `Prepared` | every builder (`filter()`, `load()`, `order_by()`, slices), writes, `select()`, `paginate()`, `lock()`, `batches()`, `async for` | The query is fixed when it is built; only the `param()` values change. Writes send their rows without an IR, so there is nothing to prepare. |
| `BoundQuerySet` | `order_by()`, slices, `[n]`, `first()`, `get()`, `paginate()`, `select()`, `group_by()`, `distinct()`, `cte()`, `from_()`, `union()` | Its result is the bound objects, at most one row per object, matched by primary key, in input order. |

The rule for a `BoundQuerySet`: a method is allowed when the query still gives at most one row per bound object.
`filter()`, `exclude()`, `load()` (fields, relations, labeled expressions), `join(cte)` (a primary key that comes back twice raises `QueryError` when the objects are read), `lock()`, `using()`, `without_defaults()`, `update()`, `delete()`, `count()` and `exists()` keep that rule.
