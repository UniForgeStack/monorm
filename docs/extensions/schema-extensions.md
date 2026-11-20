# Column-role extensions: `updated_at`, soft delete, optimistic locking

Three built-in extensions give a column a role that the ORM keeps on writes.
Each one is a crate in `extensions/`; the host has no code for the roles, and the crate pushes neutral write records.
They come with the `monorm` Python wheel and the `monorm` npm package (`monorm/extensions/timestamps`, `monorm/extensions/soft-delete`, `monorm/extensions/locking`); there is nothing else to install.
`monorm-extension-package package <crate dir> --out <dir>` builds them for the host platform (see `optional-packaging.md`).

| Crate | Extension id | Attribute |
|---|---|---|
| `extensions/timestamps` | `monorm.timestamps` | `@@timestamps.updated_at(name, map:, mode:, upsert:)` |
| `extensions/soft-delete` | `monorm.soft-delete` | `@@soft_delete.deleted_at(name, map:, mode:, rule:)` |
| `extensions/locking` | `monorm.locking` | `@@locking.version(name, map:, upsert:)` |

The model attribute adds the column and gives it the role.
The optional first argument is the snake_case field name; the default is `updated_at`, `deleted_at` or `version`.
TypeScript shows the camelCase name.
The column has the field name, or the `map:` argument: `@@soft_delete.deleted_at(map: "_deleted_at")` keeps a live column `_deleted_at`.
A model that already has a field of that name, or a field on that column, fails the build with an error that names both fields.
On an `abstract` or `extends` base (the inherit extension), the attribute reaches each concrete child.
The old field attributes (`@timestamps.updated_at`, `@soft_delete.deleted_at`, `@locking.version`) fail the build with the exact model-attribute line to use.
The line keeps the old column: an old field with `@map("x")` prints `map: "x"`, and an old field `editedAt` prints `@@timestamps.updated_at("edited_at", map: "editedAt")`.
`@@locking.version` always adds an `Int` field.
For an old `BigInt` version field, the line has a note: change the type by hand.
A build without the extension fails a schema with its attribute: "not compiled into this artifact and no installed extension owns it; install its package or rebuild with its extension (namespace @@<prefix>.*, see docs/README.md)".

## Arguments

| Argument | `timestamps.updated_at` | `soft_delete.deleted_at` | `locking.version` |
|---|---|---|---|
| first, `name` | field name, default `updated_at` | default `deleted_at` | default `version` |
| `map:` | column name, default the field name | same | same |
| `mode:` | `"database"` (default) or `"application"` | same | none: no trigger exists |
| `upsert:` | `"drop"` (default) or `"pick"` | none | `"drop"` (default) or `"pick"` |
| `rule:` | none | name of a PostgreSQL rule that the triggers replace; no default | none |

* `@soft_delete.keep_child_rows` is a field attribute without an argument (see "Soft delete").
* Any other argument fails the build.
  A bad `mode` fails with `mode must be "database" or "application"`, and a bad `upsert` with `upsert must be "drop" or "pick"`.

## Modes

`@@timestamps.updated_at` and `@@soft_delete.deleted_at` take `mode: "database"` (the default) or `mode: "application"`.

* Database mode adds row triggers to the migration, so every writer gets the behavior, also raw SQL.
  The trigger names are in the table in "Triggers and indexes".
* Application mode adds no DDL. Only ORM writes get the behavior.

ORM writes set the column in the statement in both modes, so `RETURNING` gives the new value on Postgres and SQLite.
The modes differ in two cases.
A write that names the stored value of `updated_at` again is a same-value write.
In database mode, the trigger replaces it with the current time; in application mode, the stored value stays.
An upsert sets `updated_at` and the version in its `DO UPDATE` clause in both modes (see "Behavior change").

## `updated_at`

```prisma
model Note {
  id    Int    @id
  title String
  @@timestamps.updated_at
}
```

```python
await Note.objects.filter(Note.id == 1).update(title="b")   # updated_at is now
```

```ts
await Note.objects.filter(Note.id.eq(1)).update({ title: "b" }); // updatedAt is now
```

* The attribute adds `updated_at DateTime @default(now())`.
* `upsert: "drop"` (the default) leaves the field out of the default upsert update list; `upsert: "pick"` puts it in.
  An explicit `update_fields` wins.
* Each ORM update (query set `update()`, instance `update()`, `update_many()`) sets the field to the current time, unless the update sets the field.
* An insert uses the field default.
* Database mode, Postgres: a `BEFORE UPDATE ... FOR EACH ROW WHEN (NEW.updated_at IS NOT DISTINCT FROM OLD.updated_at)` trigger sets `NEW.updated_at := now()`.
  An update that writes a value that differs from the stored value keeps its value.
  An update that writes the stored value again (a same-value write) gets the current time.
* Database mode, SQLite: SQLite cannot assign `NEW` in a trigger.
  An `AFTER UPDATE ... WHEN NEW.updated_at IS OLD.updated_at AND NEW.updated_at IS NOT strftime(...)` trigger updates the row to `strftime('%Y-%m-%d %H:%M:%f','now')` (milliseconds).
  The second condition stops the trigger from firing itself when `recursive_triggers` is on.
  SQLite `RETURNING` does not show a change made by an `AFTER` trigger; ORM updates set the field themselves, so the trigger does not run for them.

## Soft delete

The API follows django-safedelete.

```prisma
model Author {
  id         Int       @id
  books      Book[]
  @@soft_delete.deleted_at
}
model Book {
  id         Int       @id
  author_id  Int
  author     Author    @relation(fields: [author_id], references: [id], onDelete: Cascade)
  @@soft_delete.deleted_at
}
```

* The attribute adds a nullable `DateTime?` field.
  One field for each model: a second one fails with `<Model> has more than one soft-delete field`.
* `delete()` on a query set or an instance sends `UPDATE ... SET deleted_at = <now> WHERE ... AND deleted_at IS NULL`, never `DELETE`.
  The count and `RETURNING` are the rows that the call soft-deleted; rows that are already soft-deleted do not count.
* The members below come from the soft-delete extension's generated code (`monorm_soft_delete`, `monorm/extensions/soft-delete`).
  A model gets them when it declares `@@soft_delete.deleted_at`; other models, and proxy models, do not.
* `hard_delete()` (`hardDelete()`) sends `DELETE`.
  In database mode, the trigger soft-deletes a live row, so `hard_delete()` deletes only rows that are already soft-deleted.
  On a query set, it bypasses only the `"deleted"` default, so the other default filters (a tenant filter) still apply.
  An instance `hard_delete()` bypasses only `"deleted"` too. It deletes the row it holds, and no scope check applies to it.
  An instance `hard_delete()` does not check the version, also with `@@locking.version`.
* `undelete()` on a query set clears the field of the matching soft-deleted rows; on an instance, it clears the field of its row.
  On a query set, it bypasses only the `"deleted"` default, like `all_with_deleted()`.
* The extension adds the read default `"deleted"` (`deleted_at IS NULL`) to each soft-delete model.
  Plain reads, counts, relation joins and loads return live rows only.
* `all_with_deleted()` (`allWithDeleted()`) is `without_defaults("deleted")`: it removes only that default.
  Other defaults, such as a tenant filter, still apply.
  A bare `without_defaults()` removes every default, a tenant filter too: to bypass only the soft-delete default on a query set, name it, `without_defaults("deleted")` (`withoutDefaults("deleted")`).
  `deleted_only()` (`deletedOnly()`) bypasses `"deleted"` and gives only the soft-deleted rows.
* An instance method that needs its own row (`update()`, `load()`, `delete()`) finds the row even when a default hides it.
  `post.qs()` and `of(objs)` also apply no default to their own rows.
* Database mode adds a `BEFORE DELETE ... FOR EACH ROW` trigger.
  Its guard is `WHEN (OLD.deleted_at IS NULL)`; on Postgres the guard also has `AND pg_trigger_depth() = 0`.
  It sets the field and cancels the delete (Postgres `RETURN NULL`, SQLite `RAISE(IGNORE)`).
  A raw `DELETE` of a live row soft-deletes it.
  On Postgres, the statement reports 0 rows.
  On SQLite, a `batch` counts the write of the trigger, so a raw `DELETE` of 3 live rows reports 3.
  A second `DELETE` deletes the soft-deleted row.
  With `@@locking.version`, the trigger also adds `version = version + 1`.
* Cascade, database mode: a cancelled delete does not fire `ON DELETE CASCADE`.
  For each soft-delete child with `onDelete: Cascade`, the parent gets an `AFTER UPDATE OF deleted_at` trigger.
  When the parent row becomes soft-deleted, the trigger sets the same time on the live child rows.
  A child without soft delete would keep its rows, so schema definition refuses it (see the next item).
* A lost cascade is a schema error.
  If a soft-delete model has a foreign-key relation with `onDelete: Cascade` from a child that has no soft delete, the schema does not load.
  The check runs in database and application mode.
  It follows the cascades through every level, and through proxy and storage owner tables.
  The error names the parent, the child, the relation and the two fixes: add `@@soft_delete.deleted_at` to the child, or add `@soft_delete.keep_child_rows` to the relation field.
  `@soft_delete.keep_child_rows` takes no argument.
  With it, a soft delete of the parent keeps the child rows (the old behavior); a hard delete of the parent still deletes them through the database cascade.
  The Postgres delete trigger runs only at trigger depth 0 (`pg_trigger_depth() = 0`), so a hard delete of a soft-deleted parent leaves no orphan: the database cascade hard-deletes its children.
  The cost: a `DELETE` that runs inside another trigger is always a hard delete, and each row that the trigger tests calls `pg_trigger_depth()`.
* SQLite: a hard delete of a soft-deleted parent whose live child rows exist fails with `FOREIGN KEY constraint failed`, and no row is deleted.
  Delete or hard-delete the children first.
* Migration: a changed trigger body (the depth guard, the version bump) shows in the next migration diff; apply that migration to an existing database.
* Application mode has no cascade; the schema check above is the same in both modes.
* Revive cascade, database mode: `undelete()` of a parent clears the stamp of the child rows that carry the stamp of the parent.
  A child that was soft-deleted on its own, with another stamp, stays deleted.
  A relation with `@soft_delete.keep_child_rows` has no cascade and no revive: the child rows stay as they are.
  This is new in `undelete()`: a revive now reaches the children.
* Owning relations (`owns`): an owned row with soft delete gets the stamp of the last owner that is soft-deleted, and only then.
  The sweep trigger on each owner stamps the owned row when no live owner remains; an owner without soft delete counts as live.
  The owned revive clears the stamp when the owner has the same stamp, or when no other live owner exists.
  An owning relation into a table without soft delete is a schema error with the same two fixes as a lost cascade.
  An owning relation into a level of a composed model is a schema error: point it at the root, or remove `owns: true`.
  Application mode has no owning trigger.
* PostgreSQL and SQLite differ for owning relations.
  PostgreSQL locks the owned row (`FOR UPDATE`), so two co-owners that archive at the same time stamp it once, and a new owner waits for the sweep.
  It allows an owning cycle; two transactions that archive two rows of one cycle can deadlock, and PostgreSQL aborts one.
  SQLite has no row lock and no recursive triggers: an owning cycle is a schema error, a chain works, and a hard delete of a non-soft owner soft-deletes the live owned row.
* `sweep_owned(repair=False, models=None)` (`sweepOwned({ repair, models })`) finds the live owned rows that every owner has left; `{}` means none.
  `repair=True` stamps them with the current time, in one transaction, in at most owning relations + 1 passes; it raises and rolls back all passes at the bound.
  On PostgreSQL it needs a superuser or a role of `bypass_roles`, and it takes no lock.
  A CI script runs it and exits 1 when it finds a row; see `extensions/soft-delete/README.md`.
* A composed model (`model-composition`) takes the roles on its root only.
  `@@timestamps.updated_at`, `@@soft_delete.deleted_at` and `@@locking.version` on a level below the root are a schema error that names the root.
  A change on any level bumps the root's `updated_at` and its version.
  A soft delete updates only the root row and keeps every level row; `undelete()` restores the whole row; `hard_delete()` through any level model (the root or a child) removes every level row of that row, sibling children too (in database mode a live row needs a second call, because the root trigger soft-deletes it first).
  In database mode the root has the triggers, and each level table below it has an `AFTER UPDATE` trigger `updated_at_<column>_root` that touches the root, so a raw update of a level table bumps `updated_at`; `skip_triggers("timestamps")` skips it.
  The version bump comes from the ORM in both modes; the touch trigger bumps `updated_at` only, so a raw update does not bump the version.
  A composed update or delete with a hook or expression assignment on a role is rejected.
* A proxy model takes the roles of its root storage owner: the soft-delete, `updated_at` and version records.
  The proxy must keep those fields.
  In application mode, a proxy that excludes the `updated_at` field is rejected, and the error names the field.

### Moving from guitars

guitars soft-deletes with an `ON DELETE ... DO INSTEAD UPDATE` rule.
monorm adds no rule mode: it replaces the rule with its triggers, in one migration.

1. Run `monorm pull`.
   It reads a rule of the guitars shape, and the plain shape `DO INSTEAD UPDATE t SET col = now() WHERE key = OLD.key`, as `@@soft_delete.deleted_at("<column>", rule: "<rule name>")`.
   Check that `pull` printed `The schema reproduces the database.`
   A `serial` column adds a `// gap:` line: `pull` then prints `The schema matches the database, except for what is not reproduced.` Read the gap lines instead.
   A rule that you turned off (`ALTER TABLE ... DISABLE RULE`) is not claimed: it stays a gap line, with the note `the rule is disabled`.
   A rule of another shape stays a `// gap:` line. The move never drops it.
   The cascade rules of guitars (`ON UPDATE ... DO ALSO UPDATE child`) are such a gap too.
   Leave them in place: a Django foreign key has no `ON DELETE CASCADE`, so the monorm cascade trigger does not reach those children, and the rule still does.
   Drop such a rule by hand only when the relation has `onDelete: Cascade`.
2. Run `baseline` on an empty migrations folder, before any `makemigrations`.
   The baseline leaves the soft-delete triggers out and keeps the rule names, so the database stays as it is.
3. Run `makemigrations`. The move migration has `DROP RULE IF EXISTS` for each recognized rule before the trigger statements.
   `migrate` runs it in one transaction, so no delete is hard in between.
   A new database has no rule, so the drop does nothing there.
4. A parent with `onDelete: Cascade` to a child without soft delete now fails the schema load (see above).
   Add `@@soft_delete.deleted_at` to the child, or `@soft_delete.keep_child_rows` to the relation.
5. If code sets `rules.hard_deletion` to delete rows for real, change it before you run the move migration.
   After the move that setting has no effect: use `hard_delete()`.

What changes after the move:

* The setting `rules.hard_deletion` has no effect. Use `hard_delete()`, or delete rows that are already soft-deleted.
* A second `DELETE` of a soft-deleted row deletes it. The guitars rule left it as it was.
* `rule:` needs `mode: "database"` and PostgreSQL. The schema load refuses `rule:` on SQLite, which has no rules.
* The `down` of the move migration warns and does not bring the rule back, so deletes are hard after the `down` until you create the rule again.

### Behavior changes

* Error messages changed when the role code left the host.
  A role on a level below the composed root names the extension id, not the attribute: `a write record from monorm.timestamps on Employee.changed is not allowed on a model below the root of its ownership chain; declare it on Person`.
  A write record on a field of the wrong type says `wrong type for a time assignment` (it said `wrong type for updated_at`).
  The soft-delete extension still requires a nullable `DateTime`; the host rule for a delete rewrite accepts any `DateTime`, so another extension can rewrite deletes on a required column.

* Reads hide deleted rows.
  Before, soft delete hid nothing, and a model needed `@@query.filter("deleted", deleted_at == null)`.
  Remove that filter: while it stays, `all_with_deleted()`, `deleted_only()`, `undelete()` and `hard_delete()` cannot see deleted rows.
  `get()` returns `None` (`null`) for a deleted row; use `all_with_deleted()` to read it.
* A to-one reference to a soft-delete model is nullable in the generated types: `Author | None` in Python, an `opt` hop in TypeScript paths.
  The `"deleted"` default can hide the target row.
  This is a breaking type change: code that reads `book.author.name` must handle `None`.
* A many-to-many join model that has a default filter applies it to the link table.
  When the filter reads `scope.<name>` and the scope is not set, the query fails with an error that names the default.
  Set the scope with `scope()`, or bypass the default.
  This is fail-closed by design: a query with no scope set does not read the link table without the filter.
* Query-set `hard_delete()` and `undelete()` keep the other default filters.
  Before, they dropped all of them, including a tenant filter.
* An upsert (`on_conflict(update=True)`) whose key a soft-deleted row holds changes nothing: the conflict blocks the insert, the `"deleted"` default blocks the update.
  Undelete the row first, or hard-delete it.
* A bound query set (`of(objs)`, `Model.objects.of(objs)`, `post.qs()`) applies no default of the model to its own rows.
  Before, it applied them, so a soft-deleted or tenant-hidden object was not read, locked, updated or deleted by `of()`.
  Now `of(objs).update()` and `of(objs).delete()` ignore the tenant and soft-delete filters on the root, as `post.save()` and `post.delete()` do.
  `filter()` still narrows, and a relation inside a bound `load()` keeps the default of its target model.

```python
await Author.objects.filter(Author.id == 1).delete()    # sets deleted_at
await Author.objects.all()                              # the row is hidden
await Author.objects.all_with_deleted()                 # the row is back
```

```ts
await Author.objects.filter(Author.id.eq(1)).delete();  // sets deletedAt
await Author.objects.all();                             // the row is hidden
await Author.objects.allWithDeleted();                  // the row is back
```

## Upsert

An upsert (`on_conflict(update=True)`) sets `updated_at` and adds `version = version + 1` in its `DO UPDATE` clause, unless the call sets the field.
`update=True` without `update_fields` or `update_values` does not overwrite the version field with the given value.
The version is a concurrency token, so it stays out of the update list; an explicit `updated_at` from the caller wins over the generated value.

## Optimistic locking

```prisma
model Doc {
  id      Int    @id
  title   String
  @@locking.version
}
```

* The attribute adds `version Int @default(0)`.
  One field for each model.
* Each ORM write that updates a row (query set `update()`, instance `update()`, `update_many()`, a soft delete, `undelete()`, and the `DO UPDATE` branch of an upsert) adds `version = version + 1`, unless the write sets the field.
* An insert stores the version that it names (`insert(version=7)`, `insert_many(version=5)`); the default is 0.
* An instance `update()` or `delete()` also matches the loaded version: `WHERE pk = ... AND version = <loaded>`.
  When no row matches and the row has another version, it raises `monorm.VersionConflict` (`VersionConflict` in TypeScript).
  When the row is gone, `update()` raises `DoesNotExist`.
* The instance must have the version field loaded.
* The version enters writes only: a read or `lock()` through `post.qs()` refreshes a stale object.
  Bound writes (`post.qs().update()`, `monorm.of(posts).delete()`, `undelete()`, `hard_delete()`) match the pairs `(pk, version)` of the loaded objects and raise `VersionConflict` when a row has another version.
  A write of one object needs no transaction.
  A write of several objects runs in a transaction (a savepoint inside one transaction).
  A conflict rolls the transaction back, changes no row and restores the objects.
  The public `update`, `delete` and `undelete` methods come from the host.
  An object that a filter or a delete leaves out is not a conflict.
  Two objects of one row with two versions are a conflict.
* Instance `update()` rejects an explicit version field with `TypeError`.
  Use `Model.objects.filter(pk=...).update(version=...)` in Python or `Model.objects.filter({ pk: ... }).update({ version: ... })` in TypeScript for an intentional query-set override.
  An explicit upsert update list may also override the version.
* A query set `update()` does not check the version, as Rails `update_all`.
* An instance `hard_delete()` has no version check.
* A proxy inherits its root storage owner's version and soft-delete roles and must keep those fields.
  Proxy instance `update()` and `delete()` check the loaded version, including proxies of proxies.
* The only DDL is the column `version integer DEFAULT 0 NOT NULL`: no trigger and no index.
  A write that does not go through the ORM does not add 1.

```python
doc = await Doc.objects.get(Doc.id == 1)
try:
    await doc.update(title="new")
except monorm.VersionConflict:
    await doc.load(reload=True)  # load the other writer's change, then decide
```

```ts
const doc = await Doc.objects.get(Doc.id.eq(1));
try {
  await doc.update({ title: "new" });
} catch (e) {
  if (!(e instanceof VersionConflict)) throw e;
  await doc.load({ reload: true }); // load the other writer's change, then decide
}
```

## Triggers and indexes

| Extension | PostgreSQL | SQLite |
|---|---|---|
| `updated_at` (database mode) | trigger `updated_at_<column>` (`BEFORE UPDATE`), function `<table>_updated_at_<column>` | trigger `<table>_updated_at_<column>_trigger` (`AFTER UPDATE`) |
| `updated_at` on a composed level table | trigger `updated_at_<column>_root` (`AFTER UPDATE`), function `<table>_updated_at_<column>_root` | trigger `<table>_updated_at_<column>_root_trigger` |
| soft delete (database mode) | triggers `soft_delete` (`BEFORE DELETE`) and `soft_delete_cascade` (`AFTER UPDATE OF <column>`), functions `<table>_soft_delete` and `<table>_soft_delete_cascade` | triggers `<table>_soft_delete_trigger` and `<table>_soft_delete_cascade_trigger` |
| locking | none | none |

The extensions add no index.
Application mode adds no trigger, only the column.

## Errors

| Error | Cause and fix |
|---|---|
| `<Model> has more than one soft-delete field` (also `version field`) | One model declares the attribute twice. Keep one. |
| proxy must keep the soft-delete field, the version field or the `updated_at` field | A proxy excludes the field. Keep it in the proxy. |
| `lost_children` (a schema error that names parent, child and relation) | A cascade from a soft-delete model reaches a table without soft delete. Add `@@soft_delete.deleted_at` to the child, or `@soft_delete.keep_child_rows` to the relation. |
| `rule names a PostgreSQL rule; SQLite has no rules to replace` | `rule:` on SQLite. Remove it. |
| `rule names a database rule that the triggers replace; it needs mode "database"` | `rule:` with `mode: "application"`. Use database mode. |
| `<Model> already hides soft-deleted rows by default; remove this filter` | A `@@query.filter` repeats the `deleted` default. Remove it. |
| `monorm.VersionConflict` (`VersionConflict`) | The row has another version. Reload and decide. |
| `TypeError` from an instance `update()` | The call sets the version field. Use a query set `update()`. |
| `ValueError` from `skip_triggers("soft-delete")` | These triggers cannot be skipped. |
| `QueryError` from `skip_triggers("timestamps")` on SQLite | SQLite has no role switch. `skip_triggers("timestamps")` works on PostgreSQL for a role in `bypass_roles`. |

## Limits

* Application mode does not cover a raw writer.
  In database mode on SQLite, the `updated_at` trigger ignores `bypass_roles`.
* `pull` reads the columns as plain fields.
  On PostgreSQL, `pull` also writes each role trigger as a `function` block and a `@@trigger(...)` line, and reports "The schema reproduces the database."
  On SQLite, `pull` lists each role trigger as a `// gap:` line ("SQLite triggers are not read").
  It reads a soft-delete rule as `@@soft_delete.deleted_at(..., rule:)` (see "Moving from guitars") and no other role.
