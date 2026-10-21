# Partial models and schema query defaults

`select()` retains its projection-row/scalar behavior. `load()` with columns returns model
instances with an explicit public scalar-field selection:

```python
row = await User.objects.load(User.name, User.note).get(User.id == 1)
row.name                     # loaded value
row.note                     # None is a loaded SQL NULL
row.bio                      # raises orm.NotLoaded
row.pk                       # identity remains available internally
row.to_dict()                # public loaded scalar fields only
await row.load(reload=True)  # reads all scalar fields: the partial row becomes full
await row.load(User.bio, reload=True)  # reads that field
await row.update(name="new") # preserves the public selection
```

TypeScript uses the same `load(User.name, User.note)` syntax, `NotLoaded`,
`toJSON()`, and `load(...columns, { reload: true })`. Explicit reload columns add to the
current projection. Its partial result type has only the loaded fields
(`Pick<UserData, "name" | "note"> & Instance<UserSpec>`); methods and `pk` remain available.
Python returns the generated model class; field annotations describe a loaded
value, and omitted field access raises `NotLoaded` rather than returning `None`.
Columns of later `load()` calls add to the selection. `load()` without items resets to every public scalar field. It does not change
filters or reference loading. Column expressions and writes remain usable for
excluded fields. Insert requirements remain unchanged.
A bare relation path beside columns of that relation (`load(Book.author, Book.author.bio)`) loads the related model's default selection united with those columns.

`only()` is in the core. Schema query defaults need the `query-defaults` extension.

**Enable:** select the `orm-query-defaults` crate (`extensions/query-defaults/`) in your build.
See [build-time-extensions.md](../contracts/build-time-extensions.md#select-feature-extensions).

The optional `query-defaults` native capability and compiler crate
`orm-query-defaults` support schema policies:

```prisma
model User {
  id Int @id @default(autoincrement())
  name String
  active Boolean @default(true)
  bio String @query.selectOut
  @@query.defaults(filter: "active == true", fields: ["id", "name"], related: [])
}
```

Each option also has its own attribute.
One option comes from one place on a model.
`@@query.filter("<name>", "<expr>")` adds a named filter; see "Named filters" below:

```prisma
model Base {
  id Int @id @default(autoincrement())
  @@query.filter("positive", "id > 0")
}
model Profile {
  id Int @id @default(autoincrement())
  user User?
}
model User {
  id Int @id @default(autoincrement())
  name String
  active Boolean @default(true)
  created_at DateTime @default(now())
  bio String @query.selectOut
  profile_id Int @unique
  profile Profile @relation(fields: [profile_id], references: [id])
  @@query.filter("active", "active == true")
  @@query.fields(["id", "name"])
  @@query.related(["profile"])
  @@query.order("-created_at", "id")
  @@query.parent("Base")
}
```

A second copy of `@@query.fields`, `@@query.related`, `@@query.order` or `@@query.parent` is an error.

### Named filters

`@@query.filter("<name>", "<expr>")` adds one filter with a name. A model can have several.
The filters of a model join with AND, and with the filters of other extensions.
Each named filter is its own default, so `without_defaults("<name>")` drops that filter only.

```prisma
model Post {
  id        Int     @id @default(autoincrement())
  shop_id   Int
  published Boolean
  @@query.filter("tenant", "shop_id == scope.shop")
  @@query.filter("published", "published == true")
}
```

```python
Post.objects.without_defaults("published")   # still filtered by "tenant"
Post.objects.without_defaults()              # no default filter
```

The name is letters, digits and underscores.
A name is unique on a model across sources: the first entry keeps the name and a second one is a schema error that names both sources.
`"query"` is the query-defaults convention, not a core rule: that entry holds the order, the fields and the related paths,
and also the filter of `@@query.defaults(filter: ...)`. `without_defaults("query")` drops those with that filter.
The `query` entry exists on every model once any `@@query.*` is declared.
A one-argument `@@query.filter("<expr>")` is a schema error that shows the two-argument form.

A child (`@@query.parent`, a proxy or a composed model) inherits each name of its parent.
A filter with the same name on the child replaces the inherited one; `parent.default_filter` in it means
the parent's filter of that name. `@@query.filter("<name>", "none")` removes the inherited name.

Build-select the compiler as a dependency with alias `query_defaults`; its
manifest selects the `query-defaults` host capability. Without that compiler,
`@@query.*` declarations fail at definition. The neutral `read_defaults` prepared
contract is accepted in every build; other extensions can contribute to it.
An `annotate` item of that contract must be a relation aggregate.
A read that returns model instances sets it as an attribute of each object.
A `select(...)` projection, an aggregate, `group_by`, `distinct` and a subquery never get it.
An explicit annotation of the same name wins over the default.
A prefetched `load()` read of a related model gets that model's default annotations; a joined or composed read does not.
Ordinary explicit partial selections remain available in baseline builds.

Options omitted on a child inherit the nearest parent's policy. An explicit `filter:` option
replaces the inherited filter (named filters inherit by name, above). `parent.default_filter and active == true` composes
it deliberately. A standalone policy may specify `parent: "Base"`; proxy and
composition declarations supply their parent. `filter: "none"` clears the filter,
`fields: ["*"]` resets to all fields, and `related: []` clears default loading.
An explicit field list replaces the inherited list; local `@query.selectOut`
removes that field from the default list. It retains the field in the model.
An empty selected list exposes no scalar fields, while retaining identity helpers.

Filter expressions support root fields, string/integer/boolean literals, `scope.<name>` values, NULL,
`== != < <= > >=`, parentheses, `and`/`or` (or `&&`/`||`), and `not`/`!`.
`not` binds looser than comparisons: `not a == 1` means `not (a == 1)`.
Literal types are checked by the database when the query runs.
`parent.default_filter` requires an inherited filter. Unknown fields, inheritance
cycles and recursive default loading reject during compilation/definition.
Related paths use schema names separated by dots and must be to-one references.

Python `without_defaults(*names)` and TypeScript `withoutDefaults(...names)` bypass the
named defaults, or all of them with no name. Names add up across calls, and a call with no name
bypasses all defaults. A name that the model does not define raises `QueryError`:
`<Model>: unknown default "<name>"; available defaults: [...]`. A bypassed default drops its
filter, selection, order and eager loading. In Rust the call is `without_defaults_named(names)`.
Generated models type the names: a Python `Literal[...]` and a TypeScript string-literal union
(a model with no defaults keeps `str`).

`current_filter()` (TypeScript `currentFilter()`) lists the default filters of a query set as
`[{source, name, expr, active}]`. `expr` is the IR expression with scope references unresolved.
`active` is false for a bypassed default. A query set over a CTE gives `[]`.
Without names, the call bypasses default
filters, selections and eager loading, including joined and `IN (...)` loads, while preserving caller filters and explicit
loading/selection. `without_related()` / `withoutRelated()` clears the default joins and the joins
`load()` asked for so far (relations loaded by `IN (...)` query stay); later `load()` calls add references.
Defaults affect reads, count/exists, and queryset update/delete selection, including
bulk updates. Inserts use declared client/database defaults and caller values;
filters never provide inserted values.

### Scope values: `scope.<name>`

A filter can read a value that the application sets for the current request or task:

```prisma
model Order {
  id      Int @id @default(autoincrement())
  shop_id Int
  @@query.filter("tenant", "shop_id == scope.shop")
}
```

```python
with orm.scope(shop=shop.id):          # a sync `with`; tasks started inside get it
    orders = await Order.objects.filter(Order.total > 10)
```

```ts
await scope({ shop: shop.id }, async () => { const orders = await Order.objects.filter(...); });
```

* The frontend sends the scope values as parameters of each statement; the value is bound with the type of the column it is compared with.
* Closed by default: a read, count, exists, update, delete or `update_many` on a model whose default filter reads `scope.shop` raises `QueryError` when no enclosing `scope()` sets `shop`. Relation hops, prefetches and joined targets of that model check it too.
  The error names the model, default name and source that read the value, and suggests
  `without_defaults("<name>")`, which bypasses only that default.
* A NULL scope value (`None`, `null`) raises `QueryError` ("scope.shop is None/null") when a default filter reads it, because `shop_id = NULL` would match no rows.
  In TypeScript, the binding rejects `undefined` before any filter reads it ("undefined is not a value").
  `0`, `""` and `false` are valid values.
  To read the rows with a NULL `shop_id`, use `without_defaults()` and filter on `shop_id == None`.
* `sql()`, prepared queries and `prepare_update()` / `prepareUpdate()` read the scope as the query does.
* `without_defaults()` / `withoutDefaults()` skips the filter and the check.
* Inner `scope()` values replace outer ones.
* Inserts do not read the scope: set `shop_id` yourself.
  A composed model reads its new or changed rows back by key, without the default filter.
* An awaited query set keeps its rows; awaiting it again under another scope gives the first rows.
* For a check in the database as well, use Postgres row-level security with `db.tenant(id)` (see the API docs).

The default order is one string for each order column: a field, `-` before it for descending, and an optional ` nulls first` or ` nulls last`.
`@@query.defaults` takes the same strings as a list: `order: ["-created_at", "id"]`.
Order columns are fields of the model itself, not related columns.
A child inherits the order, and `@@query.order()` or `order: []` clears it.
The schema load checks the resolved order of each model, inherited ones included: each column is a field of the model, once; a nullable column has `nulls first` or `nulls last`; and a `json` or `xml` column is an error.
Index the order columns: without an index, each read sorts all matching rows.
A composed child that inherits the order reads the parent's columns through a subquery, which no index serves.

The default order applies to model reads that give no `order_by()` / `orderBy()`, which include `first()`, `last()` and the rows of a relation query set in `load()` without its own order.
An explicit order replaces it, and `without_defaults()` removes it.
`select(...)` rows, subqueries, `count`, `exists`, `update` and `delete` ignore it.
For a composed model, the internal reads of an insert, update or delete keep it; it does not change which rows they write.
`batches()` and `iterate()` keep their primary-key order.

A filtered-out joined target is `None`/`null`, including a physically required
reference. Its default filter is in the LEFT JOIN condition, so the source
survives. Without a target default filter, a set key with no joined row still
raises `NotLoaded`.
Under `without_defaults()` the join has no filter. A set key with no joined row then
raises `NotLoaded`, also when the target has a default filter.
Generated reference types become nullable for filtered targets. This is absence in
the filtered view, not evidence that the physical row is absent.

UPDATE/DELETE `returning` uses the query's scalar selection, including schema
defaults. It returns the actual written row even when the mutation leaves the
filter. Instance update, delete and load use internal identity and bypass the default on the row itself (they run through `qs()`);
instance update fetches only the instance's public fields and helpers.
A bound query set (`of(objs)`, `post.qs()`) applies no default to its own rows, but a relation it loads keeps the defaults of its target.
A plain `await post.qs()` with no `load()` reads the normal selection, so a partial object gets those fields.
Insert and bulk-update returning retain their existing complete-row result contract.

Internally, `ResultShape` maps logical `FieldId`s to actual row slots and marks
public versus helper fields. PK, reference keys and computation dependencies can
be fetched privately. `Select.model_helpers` requests additional private columns.
Whole-model shapes, including `load()` without items, retain the original
materialization fast path. Binding loaders
use Python `_field_value(name)` or TypeScript `fieldValue(row,name)` to read helper
keys; neither operation changes public loaded state.
