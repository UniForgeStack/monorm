# Django coexistence

A Django service can use monorm models while Django stays in place.
This page says what stays in Django and what moves.

## What stays in Django

* Staff login (the Django admin), sessions and `request.user`. monorm has no bridge to them and does not replace them.
* The Django user table, `auth_user` or your own `AUTH_USER_MODEL`. Django owns its migrations.

## What moves to monorm

* The models you write in `schema.prisma`, and the queries, migrations and tests of those models.
* A monorm model that points at a user holds the user id as a plain column (or a relation to a model you map onto the Django user table with `@@map("auth_user")`). Read `request.user.id` in the view and pass it to the monorm query. Keep that model out of monorm migrations (`pull` it, do not `makemigrations` it) so Django remains its owner.

## Tests

Django's `TestCase` rolls its own transaction back. monorm tests use `monorm.testing.isolated()` ([Python API](../api/python-api.md#isolated-tests-monormtestingisolated)): a transaction that rolls back, or `mode="truncate"` for code that must commit. Both stay apart from the Django test database.

## Passwords

The [auth app](../apps/auth.md) verifies Django `pbkdf2_sha256$` hashes.
A later move off Django needs no password reset.
