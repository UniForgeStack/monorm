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
  * `lost_children`, owning form: `<Owner>.<relation> owns <Target> and <Target> has no soft delete`, for an owner with soft delete.
    The two fixes are the same.
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
* Mode `application` adds the column and no trigger, also none for an owning relation.
* `rule: "<name>"` names an `ON DELETE DO INSTEAD` rule of a database that the triggers replace.
  The first migration after `baseline` drops the rule.

### Owning relations

An owning relation (`owns`, contract 5.6) into a soft-delete target adds two triggers on each owner table: the sweep and the owned revive.
A target row lives while at least one live owner row points at it.
The table lists them with the cascade pair of the parent table, which the revive cascade completes.

| Trigger | Table | Fires when | Effect |
| --- | --- | --- | --- |
| `soft_delete_cascade` | parent | the parent gets a stamp | stamps the live child rows with the stamp of the parent; none for a `keep_child_rows` relation |
| `soft_delete_revive` | parent | the parent loses its stamp | clears the stamp of the child rows that carry the stamp of the parent; a child with another stamp stays deleted; none for a `keep_child_rows` relation |
| `soft_delete_sweep` | owner | the owner gets a stamp | stamps the owned row with the stamp of the owner when no live owner remains |
| `soft_delete_owned_revive` | owner | the owner loses its stamp | clears the stamp of the owned row when it equals the old stamp of the owner, or when no other live owner exists |

* The guard of the sweep has one `NOT EXISTS` arm for each owning column into the target, over the whole schema.
  A new owning column changes the function of every owner of that target, in the next migration.
  A self-owning arm excludes the target row itself.
  An owner without soft delete counts as always live.
* An owning relation into a level of a composed model is a schema error with a fix: point the relation at the root, or remove `owns: true`.
  Support for it is a later job.
* A `keep_child_rows` relation gets no cascade and no revive: the child rows stay as they are.
* A key move and an archive in one statement, and every owner in one statement, give the same result as separate statements.
  A raw `UPDATE` or `DELETE` gives the same result as the ORM call.
* PostgreSQL: the sweep locks the target row first (`FOR UPDATE`), so two co-owners that archive at the same time stamp the target once.
  `FOR UPDATE` also conflicts with the lock of a foreign key insert, so a new owner of the target waits for the sweep.
  Owning cycles work: the sweep of one owner stops at a row that already has a stamp.
  Two transactions that archive two rows of one owning cycle lock in opposite order, and PostgreSQL aborts one of them: run the statement again.
* SQLite: no row lock, and `recursive_triggers` is off.
  An owning cycle is a schema error on SQLite; a chain works.
  A hard delete of a live owner without soft delete soft-deletes the live target.
  The owning trigger of the core deletes the target after the owner delete, and the own `soft_delete` trigger of the target turns that delete into a soft delete.
  PostgreSQL deletes the target.
* Mode `application` adds no owning trigger.
  Use `sweep_owned()` to find the rows that the ORM left live.
* A revive compares stamps for equality.
  PostgreSQL `now()` is the start time of the transaction, and SQLite has a resolution of 1 ms.
  A row that another delete stamps in the same transaction or millisecond carries the stamp of the parent, and the revive cascade revives it too.
* `skip_triggers("soft-delete")` still raises.

### `sweep_owned()`

* `await sweep_owned(repair=False, models=None)` (TypeScript `await sweepOwned({ repair, models })`) lists the live target rows that every owner has left.
  All their owners are soft-deleted, and at least one is.
  The result maps a model name to the key columns of each row.
  A clean database returns `{}`.
* The Rust library builds the SQL; the bindings only run it. The report, the repair, the key columns and the role check are `kind "owned"` extension records.
* `repair=True` stamps each row with the current time, in one transaction.
  It repeats until a pass finds no row.
  It raises `RuntimeError` (TypeScript `Error`) when it needs more passes than the number of owning relations plus one, and it rolls back all passes.
  In database mode a chain settles in one pass; in application mode it needs one pass for each level.
  It does not clear a stamp that a row already has, and it takes no lock: run it in a quiet window, or run it again.
* On PostgreSQL the role must be a superuser or a role of `bypass_roles`; else it raises `PermissionError` (TypeScript `Error`), before it reads anything.
  SQLite has no role check.
* It runs without the scope filters.
* A CI script that fails when a row is left:

```python
import sys
from monorm_soft_delete import sweep_owned

lost = await sweep_owned()
if lost:
    print(lost)
    sys.exit(1)
```

### Runtime hooks

* The engine rewrites a delete to an update: effect `behavior.delete_rewrites`.
* The read default `deleted` hides the rows. `without_defaults("deleted")` (TypeScript `withoutDefaults("deleted")`) bypasses it.
* `[[rule_claims]]` in the manifest lets `pull` read a matching rule as `@@soft_delete.deleted_at(rule: ...)`.
  One pattern is the guitars rule with its `rules.hard_deletion` bypass. The other is the plain rule.

### Crate and bindings

* The crate `monorm-soft-delete` holds the schema passes, the checks and the trigger SQL.
* `python/monorm_soft_delete` adds `hard_delete`, `undelete`, `all_with_deleted` and `deleted_only` to the query set, the bound query set and the instance. It also has `sweep_owned()`.
* `typescript/src` adds the same methods in camel case (`hardDelete`, `allWithDeleted`, `deletedOnly`, `sweepOwned`).

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
The behavior tests are `languages/python/tests/test_tracking.py`, `test_version_pairs.py`, `test_owned_soft_delete.py` and `test_soft_delete_typing.py`, and the `tracking`, `version-pairs`, `owned-soft-delete` and `soft-delete-typing` tests in `languages/js/test/`.

## Limits

* A cascade or an owning relation into a table without soft delete fails the schema unless the relation has `@soft_delete.keep_child_rows`.
* `skip_triggers("soft-delete")` raises: these triggers cannot be skipped (Python `ValueError`, TypeScript `TypeError`).
* `sweep_owned()` takes no lock. An owning cycle needs PostgreSQL.
* `pull` reads a database rule as `rule:` only if its text equals one of the two patterns.

User doc: [docs/extensions/schema-extensions.md](../../docs/extensions/schema-extensions.md).
