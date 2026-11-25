# Scope: `@@scope.by`

`@@scope.by(shop: shop_id)` ties a table to a scope key.
The application sets the key with `monorm.scope(shop=1)` (TypeScript `scope({ shop: 1 }, fn)`).
The ORM adds the read filter, fills the column on insert and checks every write.
On PostgreSQL the database enforces the same rule with row-level security (RLS).

**Enable:** nothing to install. The extension is built into the `monorm` Python wheel and the `monorm` npm package.
The scope runtime (`scope`, `unscoped`) is part of the host.
See [Installable packages](../contracts/optional-packaging.md#installable-packages).

## Arguments

| Argument | Meaning | Default |
| --- | --- | --- |
| `<key>: <field>` | The scope key and the field that holds it. Give one pair for each key, for example `@@scope.by(shop: shop_id, mode: livemode)`. | none; at least one pair |

The attribute takes no other argument.
A key is declared once for each model.
The field must be a field of the model. On PostgreSQL it must be a scalar column (not `Json`, not an array).
The datasource block takes `bypass_roles` and `exempt_roles` (lists of database roles). Both default to the empty list.
It also takes three keys that name the settings of the database, for a database that another ORM shares:

| Key | Meaning | Default |
| --- | --- | --- |
| `scope_prefix = "tenant"` | The setting of key `shop` is `tenant.shop`. The policy reads the setting as a comma list (see below). | no prefix: `scope.<key>`, one value |
| `scope_bypass = { setting = "tenant.bypass", value = "on" }` | `unscoped()` sets this setting to this value, and the policy opens every key when the setting equals the value. | `monorm.bypass`, the list of the open keys |
| `scope_role_check = true` | The bypass also needs a role of `bypass_roles`. | `false` |

A key whose setting is the bypass setting is a schema error.

## Use

```python
import monorm

with monorm.scope(shop=1):
    await Order.objects.insert(total=5)     # shop_id is 1
    await Order.objects.all()               # only the rows of shop 1
with monorm.unscoped("shop"):
    await Order.objects.all()               # every shop
```

```ts
import { scope, unscoped } from "monorm";

await scope({ shop: 1 }, async () => {
  await Order.objects.insert({ total: 5 }); // shopId is 1
  await Order.objects.all();                // only the rows of shop 1
});
await unscoped(["shop"], async () => {
  await Order.objects.all();                // every shop
});
```

* `scope()` sets one value for each key. An inner block replaces the value of an outer block. A task that the block starts keeps the value.
* A read or a write with no scope set fails closed with `ScopeMissing`. `ScopeMissing` is a subclass of `QueryError`.
* `unscoped` turns off the fill, the check and the read filter for the named keys, in the block and in work it starts.
  A nested block adds its keys to the keys of the outer block.
* Bulk writes (`insert_many`, `update_many`, `delete`) follow the same rules as a single write.

```prisma
datasource db {
  provider     = "postgresql"
  bypass_roles = ["app_admin"]
  exempt_roles = ["metabase_ro"]
}

model Order {
  id      Int @id @default(autoincrement())
  shop_id Int
  total   Int
  @@scope.by(shop: shop_id)
}
```

## What the migration creates (PostgreSQL)

* `ENABLE` and `FORCE ROW LEVEL SECURITY` on the table, so the table owner obeys the policies too.
* One policy, `monorm_scope`, for all commands. `USING` and `WITH CHECK` are the same.
  Each key gives one term: the column equals the setting `scope.<key>`, or the caller is open for the key (see below).
  The setting read is `(SELECT ...)`, so PostgreSQL evaluates it once for each statement (an InitPlan), not once for each row.
* A `BEFORE INSERT` row trigger, `monorm_scope_fill`, that takes a `NULL` scope column from the setting. A block cannot skip it.
* For each role in `exempt_roles`, a policy `monorm_scope_exempt_<role>`: `FOR SELECT TO <role> USING (true)`.
  The role reads every row and writes none. The policy is created only if the role exists.
* A composed model: declare `@@scope.by` on the root. The root table has the policy. A level table below it has `USING (EXISTS (SELECT 1 FROM <direct parent> ...))`.
  `@@scope.by` on a model below the root fails the schema: `a write record from monorm.scope on <Model>.<field> is not allowed on a model below the root of its ownership chain; declare it on <Root>`.
  A proxy model uses the policy of the table that stores its rows.

SQLite gets no DDL: the ORM filter, the fill and the check still work.
Remove `@@scope.by` and the next migration drops the policies, the trigger and the flags.

## Open keys

`unscoped("shop")` sets `monorm.bypass`.
By default the policy lets such a statement see all rows for any role: the bypass needs no role.
With `scope_role_check = true` the connected role must also be a member of a role in `bypass_roles`.
The policy then looks the roles up in `pg_roles`, so a role that does not exist never matches and raises no error.
`monorm migrate` and `monorm audit` warn about a role of `bypass_roles` or `exempt_roles` that the database lacks.
A `migrate()` call from Python code does not warn: only the command line does.
With `scope_role_check = true` and an empty `bypass_roles`, the database lets nobody through: `unscoped` then only changes the ORM.
`scope_role_check` is about the scope policy only. It does not change who may use `skip_triggers`.

With `scope_bypass = { setting, value }`, `unscoped()` with no key opens every key, and the ORM sets the setting to the value.
`unscoped("shop")` raises an error when the schema has more keys, because one value opens all keys.
Name every key, or give none.
Without `scope_bypass`, a bare `unscoped()` is an error: name the keys.
Do not put the role of your application in `exempt_roles`: that role then reads every scope.

## List values

With `scope_prefix`, `scope()` takes a list (or a set) of values for a key: `scope({ shop: [1, 2] }, fn)` in TypeScript, `scope(shop=[1, 2])` in Python.
The ORM sends the list as text with `,` between the items, for example `1,2`, and the policy reads the setting as a comma list: the column matches when its text is one item of the list.
A raw `set_config('tenant.shop', '1,2', true)` gives the same result.
A list with no `scope_prefix` is an error, and so is an empty list or an item that has a `,`.
A read matches the rows of every listed value.
A write needs a cell that names one listed value: an omitted cell raises `ScopeMissing`, and a value outside the list raises a scope error.
The insert fill takes a value only from a single-item setting. A list gives `NULL`, so the insert fails.
A hook that reads a list scope raises an error, because hooks take one value for each key.

## What the ORM sends

The ORM sets `<prefix>.<key>` for each key that a policy reads, from the `scope()` context, in the first statement of the transaction.
Without `scope_prefix`, the name is `scope.<key>`.
The bypass setting is `monorm.bypass`, or the `setting` of `scope_bypass`.
A `scope=` option on one call is not sent to the database.
Raw SQL (`db.execute`, `db.fetch`) inside `scope()` carries the same settings, in a transaction and outside one.
A connection that MonOrm does not open (Django, `psql`) sets it itself at the start of the transaction: `SELECT set_config('scope.shop', '1', true)`.

## Check the database: `monorm audit`

```
python -m monorm audit
```

The command compares the RLS flags and the policies of the database with the schema.
It exits `0` if they are equal and `1` if not, and it prints each difference.
It warns when the connected role is a superuser or has `BYPASSRLS`: RLS does not apply to such a role.
`migrate` gives the same warning when it runs as a superuser on a schema with RLS, because data steps in the migration then skip RLS.
`audit` does not compare the fill trigger; `drift` does.
On PostgreSQL, `audit` also lists each trigger of the schema with its state (see [the schema guide](../schema/schema.md)), and it exits `1` if a trigger is disabled, outdated or missing.
A policy that is in the database but not in the schema, such as a hand-written one, is reported as a `drop policy` difference, and `audit` exits `1`.

`pull` does not turn policies back into `@@scope.by`.
It lists them as `// gap:` lines.

## Errors

| Error | Cause and fix |
| --- | --- |
| `ScopeMismatch` (`model`, `key`, `expected`, `actual`) | A write sets a scope column to a value other than the scope. Write inside the scope, or use `unscoped(key)` to write another scope. |
| `ScopeMissing` (`model`, `key`), a `QueryError` | A read or a write needs a scope key that is neither set nor open. Wrap the call in `scope(key=value)`. |
| `ValueError` (Python), `TypeError` (TypeScript) | `scope()` got a key that no open schema declares, `None`, an empty string or a list. `unscoped()` got an unknown key, or no key without `scope_bypass`. |
| `<Model>: the scope key <key> is declared twice` | One model repeats a key in `@@scope.by`. |
| `<Model> has no field <field>, named by @@scope.by(<key>:)` | The value of a pair is not a field of the model. |
| `<Model>: the scope field <field> must be a scalar column` | PostgreSQL only: the field is `Json` or an array. |

## Limits

* `skip_triggers` cannot skip the scope trigger.
* SQLite has no RLS: the ORM filter, the fill and the check are the only guard.

## Moving from guitars

Do the move in one migration, so that no table is open in between:

1. Add `@@scope.by` to the model and run `makemigrations`.
2. In the migration, before the generated statements, drop the old objects of each table.
   Run `\d+ <table>` in `psql` to list them.
   Guitars names its policy `tenant_scope`, its exemption policies `rls_exempt_<role>`, and each autofill trigger after its function.
   Its settings `tenant.<key>` and `tenant.bypass` are not objects. Set `scope_prefix = "tenant"` and `scope_bypass = { setting = "tenant.bypass", value = "on" }` in the datasource, and the monorm policy reads them.
3. Run `migrate`. The migration runs in one transaction.
4. Run `monorm audit` to check the result. It exits `1` if a policy of the old system is left.

## Tables of another ORM

A model with `@@managed(false)` and `@@scope.by(shop: shop_id)` keeps the read filter, the insert fill and the write check of the ORM.
MonOrm adds no policy, trigger or flag to its table: the table already has the policy of the other ORM.
This needs `scope_prefix` or `scope_bypass` in the datasource, which says that this policy reads the same settings.
With neither key, the schema fails and the error names both keys.
`drift` and `audit` skip such a table, so they report nothing about its policy, flags or triggers.
Two policies on one table are permissive: a row passes when either policy lets it through.
MonOrm makes no cascade from an unmanaged parent: a child of a soft-deleted or scoped unmanaged row follows its own rules only.

## Changing the role check or the prefix

The default is `scope_role_check = false`: the bypass needs no role of `bypass_roles`.
A change of `scope_role_check` or `scope_prefix` makes the next `makemigrations` replace each `monorm_scope` policy.
Set `scope_role_check = true` to make the bypass need a role of `bypass_roles`.

## RLS is not an access-control boundary

RLS guards against mistakes in the application and against other database roles.
It does not guard against a holder of the application credential.
That holder can run `set_config('scope.shop', ...)` and read any scope.
Do not give the credential to code that you do not trust with every scope.
