# soft-delete

`@@soft_delete.deleted_at` adds a nullable `deleted_at` column.
A delete sets it, and every read hides the rows that have it.
The attribute takes `name`, `mode`, `map` and `rule`. The field attribute `@soft_delete.keep_child_rows` takes no argument.

## Install

The extension ships inside the one `monorm` package.
Python: `pip install monorm`.
TypeScript: `npm install monorm@npm:@ufstack/monorm`.
There is no separate wheel or npm package.

## Smallest example

```prisma
model Author {
  id   Int    @id @default(autoincrement())
  name String
  @@soft_delete.deleted_at
}
```

```python
await Author.objects.filter(Author.id == 1).delete()           # sets deleted_at
await Author.objects.all()                                     # the row is hidden
await Author.objects.all_with_deleted()                        # the row is back
```

```ts
await Author.objects.filter(Author.id.eq(1)).delete();         // sets deletedAt
await Author.objects.all();                                    // the row is hidden
await Author.objects.allWithDeleted();                         // the row is back
```

## How it works

### Schema pass

* `monorm.soft-delete.fields` (declaration phase) adds the field before the logical passes, so a proxy copies it.
* `monorm.soft-delete.prepare` (`late_behavior` phase) adds the read default `deleted`, the delete rewrite and the triggers.
  It runs after the `behavior` passes of the other extensions, such as the locking and timestamps passes.
* `monorm.soft-delete.check` (validation phase) runs after every behavior pass.
  It rejects a filter of another extension that repeats the default: `` `<Model>` already hides soft-deleted rows by default; remove this filter``.
* The checks fail the schema with these errors:
  * `` `<Model>` has more than one soft-delete field``.
  * `` `<view>`: proxy must keep the soft-delete field `<f>` ``.
  * `rule names a PostgreSQL rule; SQLite has no rules to replace`, and `rule names a database rule that the triggers replace; it needs mode "database"`.
  * `lost_children`: a cascade from a soft-delete table into a table without soft delete.
    The message names the fix: add `@@soft_delete.deleted_at` to the child, or add `@soft_delete.keep_child_rows` to the relation field.
  * `@soft_delete.keep_child_rows` on a model, or with an argument: `write it on the relation field that cascades, not on the model`, or `takes no argument`.

### SQL

Mode `database` (default) adds the column and triggers, so a raw `DELETE` also soft-deletes.
The sample is the PostgreSQL migration of `Author` and `Book` (`onDelete: Cascade`), both with soft delete:

```sql
-- "deleted_at" timestamp with time zone
CREATE OR REPLACE FUNCTION "author_soft_delete"() RETURNS trigger LANGUAGE plpgsql AS $orm$
BEGIN UPDATE "author" SET "deleted_at" = now() WHERE "id" = OLD."id"; RETURN NULL; END;
$orm$;
CREATE TRIGGER "soft_delete" BEFORE DELETE ON "author" FOR EACH ROW WHEN (OLD."deleted_at" IS NULL AND pg_trigger_depth() = 0) EXECUTE FUNCTION "author_soft_delete"();

CREATE OR REPLACE FUNCTION "author_soft_delete_cascade"() RETURNS trigger LANGUAGE plpgsql AS $orm$
BEGIN UPDATE "book" SET "deleted_at" = NEW."deleted_at" WHERE "author_id" = NEW."id" AND "deleted_at" IS NULL; RETURN NULL; END;
$orm$;
CREATE TRIGGER "soft_delete_cascade" AFTER UPDATE OF "deleted_at" ON "author" FOR EACH ROW WHEN (OLD."deleted_at" IS NULL AND NEW."deleted_at" IS NOT NULL) EXECUTE FUNCTION "author_soft_delete_cascade"();
```

* The `soft_delete` trigger cancels the delete of a live row and sets the column.
  A row that already has the column set is deleted for real.
* `pg_trigger_depth() = 0` makes a delete that a foreign-key cascade runs a hard delete.
  A cancelled cascade delete would run no foreign-key check and leave an orphan.
* The `soft_delete_cascade` trigger exists for a cascade into a child with soft delete.
  It also adds 1 to the `version` of a child that has the locking extension.
* SQLite gets the same triggers with the names `<table>_soft_delete_trigger` and `<table>_soft_delete_cascade_trigger`.
  The delete trigger ends with `SELECT RAISE(IGNORE)`.
* Mode `application` adds the column and no trigger.
* `rule: "<name>"` names an `ON DELETE DO INSTEAD` rule of a database that the triggers replace.
  The first migration after `baseline` drops the rule.

### Runtime hooks

* The engine rewrites a delete to an update: effect `behavior.delete_rewrites`.
* The read default `deleted` hides the rows. `without_defaults("deleted")` (TypeScript `withoutDefaults("deleted")`) bypasses it.
* `[[rule_claims]]` in the manifest lets `pull` read a matching rule as `@@soft_delete.deleted_at(rule: ...)`.
  One pattern is the guitars rule with its `rules.hard_deletion` bypass. The other is the plain rule.

### Crate and bindings

* The crate `monorm-soft-delete` holds the schema passes, the checks and the trigger SQL.
* `python/monorm_soft_delete` adds `hard_delete`, `undelete`, `all_with_deleted` and `deleted_only` to the query set, the bound query set and the instance.
* `typescript/src` adds the same methods in camel case (`hardDelete`, `allWithDeleted`, `deletedOnly`).

### Other extensions

* It runs after locking and timestamps, so a soft delete moves `updated_at` and adds 1 to `version`.
* A proxy must keep the field. A level of a composed model follows the soft delete of its root.

## Layout

```
extensions/soft-delete/
  monorm-extension.toml
  src/lib.rs
  python/monorm_soft_delete/
  typescript/src/
  tests/other_read_defaults.rs
```

Run `cargo test -q -p monorm-soft-delete` for the crate.
The behavior tests are `languages/python/tests/test_tracking.py`, `test_version_pairs.py` and `test_soft_delete_typing.py`, and the `tracking`, `version-pairs` and `soft-delete-typing` tests in `languages/js/test/`.

## Limits

* A cascade into a child without soft delete fails the schema unless the relation has `@soft_delete.keep_child_rows`.
* `skip_triggers("soft-delete")` raises `ValueError`: these triggers cannot be skipped.
* `pull` reads a database rule as `rule:` only if its text equals one of the two patterns.

User doc: [docs/extensions/schema-extensions.md](../../docs/extensions/schema-extensions.md).
