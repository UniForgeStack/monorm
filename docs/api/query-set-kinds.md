# Query-set kinds

The Python and TypeScript APIs have four kinds of query set and two related query objects.
Each kind leaves out the methods that do not fit its result.
"Planned" marks a kind or a change that is not on `main` yet.

## Kinds

| Kind | Get it from | Runs alone | Use |
|---|---|:---:|---|
| `QuerySet` (or a custom subclass) | `Post.objects` | yes | Every normal read and write. |
| Relation query set | `User.posts.objects` (class path) | no: `TypeError` outside `load()` | A `load()` item: filter, order, slice, `.label()`, `.as_prefetch()`. |
| `RelatedSet` / `ManyRelatedSet` | today `u.posts`; planned `u.posts.objects` | yes | The rows of one parent. `insert()` fills the key; `add()` / `remove()` for many-to-many. |
| `BoundQuerySet` (planned) | `post.qs()`, `orm.of(posts)` | yes | Fills the same objects in place. |
| `Select` (not a query set) | `qs.select(...)` | yes | `Row`s, `group_by()`, `having()`, `distinct()`. |
| One-row query (planned, not a query set) | `first()`, `last()`, `get()`, `qs[n]`, `qs.at(n)` | yes, when awaited | One row or `None`. No query runs until it is awaited; it also works in `load()`, `prepare()` and `inspect()`. |

## What each kind leaves out

| Kind | Leaves out | Why |
|---|---|---|
| `QuerySet` | `group_by()`, `having()`, `distinct()` | They change what a row means, so the result must be `Row`s: use `select()`. |
| `Select` | `count()`, `exists()`, `lock()`, `prepare()`, `explain()`, `join(cte)` | Gaps, not a design choice. A grouped report can not be counted or prepared today. |
| `Select` | `last()`, `paginate()`, `[n]` | No unique order exists over groups. |
| `Select` | `from_()`, `iterate()`, `batches()` | Not built. |
| Relation query set | running alone, writes, `select()`, `paginate()`, `lock()` | It is only an item for `load()`; use `Post.objects` for a standalone query. |
| `RelatedSet` | `.label()`, `.as_prefetch()` | These belong to `load()` items. |
| `BoundQuerySet` (planned) | `order_by()`, slices, `[n]`, `first()`, `get()`, `paginate()`, `select()`, `group_by()`, `distinct()`, `join()`, `cte()`, `union()` | Its result is the bound objects, at most one row per object, matched by primary key, in input order. |

The rule for a `BoundQuerySet`: a method is allowed when the query still gives at most one row per bound object.
`filter()`, `exclude()`, `load()` (fields, relations, labeled aggregates), `lock()`, `using()`, `without_defaults()`, `update()`, `delete()`, `count()` and `exists()` keep that rule.
