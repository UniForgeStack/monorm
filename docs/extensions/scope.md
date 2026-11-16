# Scope: `@@scope.by`

`@@scope.by(shop: shop_id)` ties a table to a scope key.
The application sets the key with `monorm.scope(shop=1)` (TypeScript `scope({ shop: 1 }, fn)`).
The ORM adds the read filter, fills the column on insert and checks every write.
On PostgreSQL the database enforces the same rule with row-level security (RLS).

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
* A composed model: the root table has the policy on its scope column. Each level table has `USING (EXISTS (SELECT 1 FROM <root> ...))`.
  A proxy model uses the policy of the table that stores its rows.

SQLite gets no DDL: the ORM filter, the fill and the check still work.
Remove `@@scope.by` and the next migration drops the policies, the trigger and the flags.

## Open keys

`unscoped("shop")` sets `monorm.bypass`.
The policy lets such a statement see all rows only if the connected role is a member of a role in `bypass_roles`.
If `bypass_roles` is empty, the database lets nobody through: `unscoped` then only changes the ORM.

## What the ORM sends

The ORM sets `scope.<key>` for each key that a policy reads, from the `scope()` context, in the first statement of the transaction.
A `scope=` option on one call is not sent to the database.
Raw SQL (`db.execute`, `db.fetch`) carries no setting.
In a raw statement, set it yourself at the start of the transaction: `SELECT set_config('scope.shop', '1', true)`.

## Check the database: `monorm audit`

```
python -m monorm audit
```

The command compares the RLS flags and the policies of the database with the schema.
It exits `0` if they are equal and `1` if not, and it prints each difference.
It warns when the connected role is a superuser or has `BYPASSRLS`: RLS does not apply to such a role.
`migrate` gives the same warning when it runs as a superuser on a schema with RLS, because data steps in the migration then skip RLS.
`audit` does not compare the fill trigger; `drift` does.

`pull` does not turn policies back into `@@scope.by`.
It lists them as `// gap:` lines.

## Moving from guitars

Do the move in one migration, so that no table is open in between:

1. Add `@@scope.by` to the model and run `makemigrations`.
2. In the migration, before the generated statements, drop the old objects of each table:
   `DROP POLICY "tenant.shop" ON <table>`, `DROP POLICY "tenant.bypass" ON <table>` and the autofill triggers.
3. Run `migrate`. The migration runs in one transaction.
4. Run `monorm audit` to check the result.

## RLS is not an access-control boundary

RLS guards against mistakes in the application and against other database roles.
It does not guard against a holder of the application credential.
That holder can run `set_config('scope.shop', ...)` and read any scope.
Do not give the credential to code that you do not trust with every scope.
