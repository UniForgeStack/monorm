# timestamps

`@@timestamps.updated_at` adds an `updated_at` column to a model and moves it to `now` on each update.
The model attribute takes the arguments `name`, `mode`, `upsert` and `map`.

## Install

The extension ships inside the one `monorm` package.
Python: `pip install monorm`.
TypeScript: `npm install monorm@npm:@ufstack/monorm`.
There is no separate wheel or npm package.

## Smallest example

```prisma
model Note {
  id   Int    @id @default(autoincrement())
  body String
  @@timestamps.updated_at
}
```

```python
note = await Note.objects.insert(body="a")
await Note.objects.filter(Note.id == note.id).update(body="b")   # updated_at is now
```

```ts
const note = await Note.objects.insert({ body: "a" });
await Note.objects.filter(Note.id.eq(note.id)).update({ body: "b" }); // updated_at is now
```

## How it works

### Schema pass

* `monorm.timestamps.fields` (declaration phase) adds the field before the logical passes, so a proxy of the model copies it.
  The field is a `DateTime`, not null, with the database default `now()` (SQLite: `CURRENT_TIMESTAMP`).
* `monorm.timestamps.prepare` (behavior phase) records the write assignments and the triggers.
* The field form (`updated_at DateTime @timestamps.updated_at`) fails with `a field attribute is no longer supported; delete the field X.f and write <line> in the model block`.
* A model without a primary key fails with `` `<Model>` has no primary key``.
* `mode` is `database` (default) or `application`. `upsert` is `drop` (default) or `pick`.

### SQL

Mode `database` adds the column and a trigger, so every writer gets the new value.
The sample is the PostgreSQL migration of `Note`:

```sql
CREATE OR REPLACE FUNCTION "note_updated_at_updated_at"() RETURNS trigger LANGUAGE plpgsql AS $orm$
BEGIN NEW."updated_at" := now(); RETURN NEW; END;
$orm$;
-- in the table: "updated_at" timestamp with time zone DEFAULT now() NOT NULL
CREATE TRIGGER "updated_at_updated_at" BEFORE UPDATE ON "note" FOR EACH ROW WHEN (NEW."updated_at" IS NOT DISTINCT FROM OLD."updated_at") EXECUTE FUNCTION "note_updated_at_updated_at"();
```

* The `WHEN` clause lets a statement that sets `updated_at` itself keep its value.
* SQLite gets an `AFTER UPDATE` trigger, named `<table>_updated_at_<column>_trigger`, that updates the column with `strftime`.
  A `WHEN` guard stops the trigger from firing itself.
* A composed model gets `updated_at_<column>_root` touch triggers on its level tables.
* Mode `application` adds the column and no trigger.
  It refuses a proxy that excludes the field: `` `<view>` excludes `<name>`, so its writes cannot set it``.

### Runtime hooks

The ORM assigns `now` on Update, Upsert and `DeleteRewrite` (a soft delete).
The assignment is the effect `behavior.write_assignments.timestamps`.
`skip_triggers("timestamps")` skips the PostgreSQL trigger for a role in `bypass_roles`.

### Crate and bindings

The crate `monorm-timestamps` holds the schema pass and the trigger SQL.
The extension has no `python/` or `typescript/` source: the engine applies the write assignment, so both languages behave the same.

### Other extensions

* `monorm.soft-delete.prepare` runs after `monorm.timestamps.prepare`, so a soft delete also moves `updated_at`.
* `monorm.history.triggers` runs after it too.
  An update that changes only `updated_at` writes no history event.
* A proxy copies the field.

## Layout

```
extensions/timestamps/
  monorm-extension.toml   passes and attributes
  src/lib.rs              the crate: passes, DDL
```

There are no `tests/public`. The behavior tests are `languages/python/tests/test_db_timestamp.py`, `languages/js/test/db-timestamp.test.ts` and `crates/engine/tests/db_timestamp.rs`.
Run `cargo test -q -p monorm-timestamps` for the crate.
Run `python -m pytest -q languages/python/tests/test_db_timestamp.py` for Python.

## Limits

* In mode `application`, a statement that bypasses the ORM does not set the column. Use mode `database` for such writers.
* `skip_triggers("timestamps")` raises `QueryError` on SQLite, and works only for a role in `bypass_roles` on PostgreSQL.
* The SQLite trigger ignores `bypass_roles`.

User doc: [docs/extensions/schema-extensions.md](../../docs/extensions/schema-extensions.md).
