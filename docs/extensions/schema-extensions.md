# Column-role extensions: `updated_at`, soft delete, optimistic locking

Three build-time extensions give a column a role that the ORM keeps on writes.
Each one is a crate in `extensions/`; the host has no code for the roles, and the crate pushes neutral write records.
Select the crate in the `orm-extension-build` configuration (see `build-time-extensions.md`).

| Crate | Extension id | Attribute |
|---|---|---|
| `orm-timestamps` (`extensions/timestamps`) | `orm.timestamps` | `@@timestamps.updated_at(name, map:, mode:, upsert:)` |
| `orm-soft-delete` (`extensions/soft-delete`) | `orm.soft-delete` | `@@soft_delete.deleted_at(name, map:, mode:)` |
| `orm-locking` (`extensions/locking`) | `orm.locking` | `@@locking.version(name, map:, upsert:)` |

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

## Modes

`@@timestamps.updated_at` and `@@soft_delete.deleted_at` take `mode: "database"` (the default) or `mode: "application"`.

* Database mode adds row triggers to the migration, so every writer gets the behavior, also raw SQL.
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
  One field for each model.
* `delete()` on a query set or an instance sends `UPDATE ... SET deleted_at = <now> WHERE ... AND deleted_at IS NULL`, never `DELETE`.
  The count and `RETURNING` are the rows that the call soft-deleted; rows that are already soft-deleted do not count.
* The members below come from the soft-delete extension's generated code (`orm_ext/orm_soft_delete`, `orm-ext/orm-soft-delete`).
  A model gets them when it declares `@@soft_delete.deleted_at`; other models, and proxy models, do not.
* `hard_delete()` (`hardDelete()`) sends `DELETE`.
  In database mode, the trigger soft-deletes a live row, so `hard_delete()` deletes only rows that are already soft-deleted.
  On a query set, it bypasses only the `"deleted"` default, so the other default filters (a tenant filter) still apply.
  An instance `hard_delete()` uses a bare bypass, so it drops all defaults: a tenant filter and its scope check too. It deletes the row it holds.
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
* Database mode adds a `BEFORE DELETE ... FOR EACH ROW WHEN (OLD.deleted_at IS NULL)` trigger.
  It sets the field and cancels the delete (Postgres `RETURN NULL`, SQLite `RAISE(IGNORE)`).
  A raw `DELETE` of a live row soft-deletes it.
  On Postgres, the statement reports 0 rows.
  On SQLite, a `batch` counts the write of the trigger, so a raw `DELETE` of 3 live rows reports 3.
  A second `DELETE` deletes the soft-deleted row.
  With `@@locking.version`, the trigger also adds `version = version + 1`.
* Cascade, database mode: a cancelled delete does not fire `ON DELETE CASCADE`.
  For each soft-delete child with `onDelete: Cascade`, the parent gets an `AFTER UPDATE OF deleted_at` trigger.
  When the parent row becomes soft-deleted, the trigger sets the same time on the live child rows.
  A child without soft delete keeps its rows.
  The Postgres delete trigger runs only at trigger depth 0 (`pg_trigger_depth() = 0`), so a hard delete of a soft-deleted parent leaves no orphan: the database cascade hard-deletes its children.
  The cost: a `DELETE` that runs inside another trigger is always a hard delete, and each row that the trigger tests calls `pg_trigger_depth()`.
* SQLite: a hard delete of a soft-deleted parent whose child rows still exist fails with `FOREIGN KEY constraint failed`, and no row is deleted.
  Delete or hard-delete the children first.
* Migration: a changed trigger body (the depth guard, the version bump) shows in the next migration diff; apply that migration to an existing database.
* Application mode has no cascade.
* A composed model (`model-composition`) keeps its own hard delete plan and takes no role.
  Schema definition rejects `updated_at`, soft-delete and version fields anywhere in its ownership chain, including the composed model and every ancestor through the root.
  The error names the composed model, the owner, the field and the extension id that wrote the record (`orm.timestamps`, `orm.soft-delete` or `orm.locking`), because composed writes cannot maintain those records.

### Behavior changes

* Error messages changed when the role code left the host.
  A composed-chain error names the extension id, not the attribute: `a write record from orm.timestamps on Person.changed is not supported in a composed model's ownership chain`.
  A write record on a field of the wrong type says `wrong type for a time assignment` (it said `wrong type for updated_at`).
  The soft-delete extension still requires a nullable `DateTime`; the host rule for a delete rewrite accepts any `DateTime`, so another extension can rewrite deletes on a required column.

* Reads hide deleted rows.
  Before, soft delete hid nothing, and a model needed `@@query.filter("deleted_at == null")`.
  Remove that filter: while it stays, `all_with_deleted()`, `deleted_only()`, `undelete()` and `hard_delete()` cannot see deleted rows.
  `get()` returns `None` (`null`) for a deleted row; use `all_with_deleted()` to read it.
* A to-one reference to a soft-delete model is nullable in the generated types: `Author | None` in Python, an `opt` hop in TypeScript paths.
  The `"deleted"` default can hide the target row.
  This is a breaking type change: code that reads `book.author.name` must handle `None`.
* A many-to-many join model that has a default filter applies it to the link table.
  When the filter reads `scope.<name>` and the scope is not set, the query fails with an error that names the default.
  Set the scope with `scope()`, or bypass the default.
* Query-set `hard_delete()` and `undelete()` keep the other default filters.
  Before, they dropped all of them, including a tenant filter.
* An upsert (`on_conflict(update=True)`) whose key a soft-deleted row holds changes nothing: the conflict blocks the insert, the `"deleted"` default blocks the update.
  Undelete the row first, or hard-delete it.
* A bound query set (`of(objs)`, `Model.objects.of(objs)`, `post.qs()`) applies no default of the model to its own rows.
  Before, it applied them, so a soft-deleted or tenant-hidden object was not read, locked, updated or deleted by `of()`.
  Now `of(objs).update()` and `of(objs).delete()` ignore the tenant and soft-delete filters on the root, as `post.save()` and `post.delete()` do.
  `filter()` still narrows, and a relation inside a bound `load()` keeps the default of its target model.
* No changelog file exists, so this list is the record.

## Behavior change: upsert

An upsert (`on_conflict(update=True)`) now sets `updated_at` and adds `version = version + 1` in its `DO UPDATE` clause, unless the call sets the field.
Before, an upsert kept both values in application mode and in the SQLite `RETURNING` row.
`update=True` without `update_fields` or `update_values` no longer overwrites the version field with the given value.
No changelog file exists, so this note is the record.

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
  When no row matches and the row has another version, it raises `orm.VersionConflict` (`VersionConflict` in TypeScript).
  When the row is gone, `update()` raises `DoesNotExist`.
* The instance must have the version field loaded.
* The version enters writes only: a read or `lock()` through `post.qs()` refreshes a stale object.
  Bound writes (`post.qs().update()`, `orm.of(posts).delete()`, `undelete()`, `hard_delete()`) match the pairs `(pk, version)` of the loaded objects and raise `VersionConflict` when a row has another version.
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
* No DDL.

```python
doc = await Doc.objects.get(Doc.id == 1)
try:
    await doc.update(title="new")
except orm.VersionConflict:
    await doc.load(reload=True)  # load the other writer's change, then decide
```
