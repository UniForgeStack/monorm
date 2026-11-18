# query-defaults

`@@query.defaults` and the `@@query.*` attributes give a model default filters, a default field list, default eager loading and a default order for every read.
`@query.selectOut` hides one field from the default read.
The extension adds no column, no index and no trigger.

## Install

The extension ships inside the one `monorm` package.
Python: `pip install monorm`.
TypeScript: `npm install monorm@npm:@ufstack/monorm`.
There is no separate wheel or npm package.

## Smallest example

```prisma
model Article {
  id        Int     @id @default(autoincrement())
  title     String
  body      String
  published Boolean @default(false)
  @@query.filter("published", published == true)
  @@query.fields(["id", "title"])
}
```

```python
await Article.objects.all()                       # published rows; id and title only
await Article.objects.without_defaults().all()    # every row, every field
```

```ts
await Article.objects.all();                      // published rows; id and title only
await Article.objects.withoutDefaults().all();    // every row, every field
```

## How it works

### Schema pass

`query.defaults.lower` (behavior phase) lowers each declaration to the effect `query-defaults`.

| Attribute | Argument | Default |
| --- | --- | --- |
| `@@query.defaults(...)` | `filter: <expr>`, `fields: [...]`, `related: [...]`, `parent: "<Model>"`, `order: [...]` | each option unset |
| `@@query.filter("<name>", <expr>)` | a name and an expression; repeatable | none |
| `@@query.fields([...])` | one list | every public scalar field |
| `@@query.related([...])` | one list | no eager loading |
| `@@query.order("a", "-b")` | strings; no argument clears | no order |
| `@@query.parent("Base")` | one model name | no parent |
| `@query.selectOut` | none; on a field | the field stays in the default read |

Checks (the schema load fails with these):

* `@@query.filter takes a name and an expression: @@query.filter("<name>", <expr>)`.
* The filter name must be letters, digits and underscores. The name `query` is reserved.
* `@@query.filter "<name>" is given twice`, and `@@<attr> is given twice` for the other attributes.
* `<Model>: <option> is set by both <a> and <b>; keep one`: one option has one source on a model.
* An order column must be a field of the model, must not repeat, and a nullable column needs `nulls first` or `nulls last`.
* `query-default inheritance cycle`, and `unknown policy parent <X>`.

### SQL

None. The defaults change the statements that the ORM builds for a read.
The filter becomes a `WHERE` term, the field list the select list, the order an `ORDER BY`, and each related name a join or a follow-up query.

### Runtime hooks

* The effect `query-defaults` becomes read defaults of the model.
  The filter of `@@query.defaults` and the field list, the related list and the order form one default named `query`.
  Each `@@query.filter("<name>", ...)` is its own default with that name.
* `without_defaults("<name>")` (TypeScript `withoutDefaults("<name>")`) drops that default.
  `without_defaults()` drops all of them.
  An unknown name fails with `QueryError`: `<name>: unknown default "nope"; available defaults: [...]`.
* A child (by `@@query.parent`, a proxy or a composed model) inherits each default of its parent.
  `@@query.filter("<name>", "none")` removes an inherited default.

### Crate and bindings

The crate `monorm-query-defaults` holds the schema pass.
The extension has no `python/` or `typescript/` source: the engine and both bindings apply the read defaults.

### Other extensions

* Another extension adds its own read defaults with other names, for example `deleted` from soft delete.
  A filter here that repeats the `deleted` default fails: `<Model> already hides soft-deleted rows by default; remove this filter`.
* A filter can read the scope: `@@query.filter("tenant", shop_id == scope.shop)`.
  A write path that needs the filter and has no scope fails closed with `ScopeMissing` (a `QueryError`) that names the model and the default.

## Layout

```
extensions/query-defaults/
  monorm-extension.toml
  src/lib.rs
  tests/fixtures/schema.prisma
  tests/public/{python.py,node.mjs}
```

Run `cargo test -q -p monorm-query-defaults` for the crate.
Run `python extensions/query-defaults/tests/public/python.py` and `(cd languages/js && node ../../extensions/query-defaults/tests/public/node.mjs)` for the public tests.
Set `ORM_TEST_DATABASE_URL` to run them on PostgreSQL too.
The behavior tests are `languages/python/tests/test_selection_defaults.py` and `selection-defaults.test.ts`.

## Limits

* A field that the defaults hide is `NotLoaded` after any write, until `without_defaults(...)` reads it.
* `pull` does not write `@@query.*`: it has no database object to read.
* `@@query.filter(<expr>)` with one argument is an error. Give the name first.

User doc: [docs/schema/selection-and-defaults.md](../../docs/schema/selection-and-defaults.md).
