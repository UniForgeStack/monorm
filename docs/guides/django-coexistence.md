# Django coexistence

A Django service can use monorm models while Django stays in place.
This page says what stays in Django and what moves.

## What stays in Django

* Staff login (the Django admin), sessions and `request.user`. monorm has no bridge to them and does not replace them.
* The Django user table, `auth_user` or your own `AUTH_USER_MODEL`. Django owns its migrations.

## What moves to monorm

* The queries, migrations and tests for the tables monorm owns.
* A monorm model can store the user ID as a plain column or declare a relation to an unmanaged model mapped to the Django user table. Read `request.user.id` in the view and pass it to the monorm query.

## monorm models over Django tables

Run `python -m monorm pull --unmanaged` against the Django database. Each pulled
table model includes `@@managed(false)`. You can also add the attribute by hand
to a single model, such as one mapped with `@@map("auth_user")`. Generate the
models and use them for queries and writes while Django continues to migrate the
tables. Run Django migrations first when a managed monorm table has a foreign
key to a Django table.

A fresh monorm test database has no unmanaged tables. Run Django migrations or
create the required tables in the test setup before running queries or applying
monorm migrations that reference them.

## Django models over monorm tables

monorm migrations own the managed tables in `schema.prisma`.
A Django project reads and writes them through hand-written models.
There is no generator and no drift check: you keep the Django model equal to the schema.

* Set `managed = False`, the exact `db_table` and the exact `db_column` of each field in `class Meta` and in the fields.
* `makemigrations` then writes a state-only migration: it creates no table. Never set `managed = True`: Django would then try to change a monorm table.
* Run `monorm migrate` before Django code uses the table.

```prisma
model Note {
  id    Int      @id @default(autoincrement())
  title String
  body  String?
  @@timestamps.updated_at(updated_at)
  @@soft_delete.deleted_at(deleted_at)
  @@locking.version
  @@map("notes")
}
```

```python
from django.db import models
from django.db.models import F
from django.utils import timezone


class NoteManager(models.Manager):
    def get_queryset(self):
        return super().get_queryset().filter(deleted_at__isnull=True)


class Note(models.Model):
    title = models.TextField(db_column="title")
    body = models.TextField(db_column="body", null=True)
    updated_at = models.DateTimeField(db_column="updated_at", default=timezone.now)
    deleted_at = models.DateTimeField(db_column="deleted_at", null=True)
    version = models.IntegerField(db_column="version", default=0)

    objects = NoteManager()
    all_with_deleted = models.Manager()

    class Meta:
        managed = False
        db_table = "notes"

    def save(self, *args, **kwargs):
        if self.pk is not None:
            self.version = F("version") + 1
        super().save(*args, **kwargs)
```

The `id` column needs no field: Django adds `id` as the primary key.
The column default `now()` is not visible to Django: the Django field needs `default=timezone.now`, because Django sends every column in an insert.
On an update, Django writes back the stored `updated_at`. The database trigger sees the same value and sets the current time.

### Composed models over Django multi-table tables

A Django multi-table model (a child with a `OneToOneField` parent link) matches a `@@composition.model` of monorm.
`delete()` and `hard_delete()` through a composed model, and a delete of its root model that removes the row, send one `DELETE` to the root row and rely on `ON DELETE CASCADE` of each level key.
Django creates that key without a database cascade, so monorm raises `QueryError` that names the level key and deletes nothing.
Add the cascade once for each level key (PostgreSQL; the constraint name comes from `\d <table>`).
Keep `DEFERRABLE INITIALLY DEFERRED`, the way Django creates the key:

```sql
ALTER TABLE app_employee DROP CONSTRAINT app_employee_person_ptr_id_fkey,
  ADD CONSTRAINT app_employee_person_ptr_id_fkey FOREIGN KEY (person_ptr_id) REFERENCES app_person (id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED;
```

### Checklist

| Feature | What Django sees | What to do |
|---|---|---|
| `updated_at` and soft delete | In database mode (the default), triggers act on every writer, so a Django `save()` and `delete()` get the behavior. Application mode adds no trigger: Django writes skip it. | Use database mode for a table that Django writes. In application mode, set `updated_at` yourself. |
| Soft-delete reads | The `deleted_at` column is a plain column. Django reads soft-deleted rows. | Filter `deleted_at__isnull=True` in the default manager, as in the example. A Django `delete()` of a live row sets `deleted_at` and keeps the row (database mode); on PostgreSQL the Django delete count is 0. |
| Optimistic locking | The extension adds only the `version` column. No trigger exists, so a Django write does not add 1 and checks no version. | Add `F("version") + 1` in `save()`, as in the example, or make the model read-only in the Django admin. A Django `save()` does not raise `VersionConflict`. |
| Scope with row-level security | On PostgreSQL the policy `monorm_scope` hides the rows of other scopes. A Django connection that sets no `scope.<key>` sees no row. The insert trigger takes a `NULL` scope column from the setting. | Put the Django database role in `bypass_roles` of the `datasource` block, or run `SELECT set_config('scope.shop', '1', true)` at the start of each Django transaction. A Django superuser or `BYPASSRLS` role skips the policy. |
| Query defaults (`@@query.filter`, `@@query.defaults`) | The filters are ORM defaults. The database has no view or policy for them. | Repeat each filter in the Django manager. |
| History | In track mode, triggers write the events, so Django writes are tracked. In versions mode, only `record()` writes a version. | In versions mode, call `record()` from the Django admin `save_model` (a Django write calls no monorm code), or the admin edits have no history. |
| Composed models | A composed model has one row in the root table and one row in each level table. | Make the Django models read-only in the Django admin. The ORM runs the writes of several tables. |
| File fields | A `Json` column (`jsonb` on PostgreSQL) that holds a storage reference. Django does not upload. | Map it with `JSONField`, and make it read-only in the Django admin. |
| Proto fields | A `bytea` column that holds an encoded message. | Map it with `BinaryField`, and make it read-only in the Django admin. |
| Generic relations | monorm has no content-type table. The type column is an integer enum from `schema.identities.json`, not a `django_content_type` id. | Do not use `GenericForeignKey` on these tables. Read the two columns as plain fields. |

### Django tests

Django creates the test database and runs only its own migrations: tables with `managed = False` are not created.
Run `monorm migrate` on the test database after Django builds it:

```python
import os
import subprocess
import sys

from django.db import connection
from django.test.runner import DiscoverRunner


class MonormRunner(DiscoverRunner):
    def setup_databases(self, **kwargs):
        config = super().setup_databases(**kwargs)
        settings = connection.settings_dict
        url = "postgres://{USER}:{PASSWORD}@{HOST}:{PORT}/{NAME}".format(**settings)
        subprocess.run(
            [sys.executable, "-m", "monorm", "migrate"],
            env={**os.environ, "MONORM_DATABASE_URL": url},
            check=True,
        )
        return config
```

Set `TEST_RUNNER = "myproject.runner.MonormRunner"` in the Django settings.
Django migrations run first, so a monorm table can point at `auth_user`.

## Tests

Django's `TestCase` rolls its own transaction back. monorm tests use `monorm.testing.isolated()` ([Python API](../api/python-api.md#isolated-tests-monormtestingisolated)): a transaction that rolls back, or `mode="truncate"` for code that must commit. Both stay apart from the Django test database.

## Passwords

The [auth app](../apps/auth.md) verifies Django `pbkdf2_sha256$` hashes.
A later move off Django needs no password reset.
