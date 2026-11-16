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
* A composed model: the table with the scope column has the policy. A level table below it has `USING (EXISTS (SELECT 1 FROM <direct parent> ...))`.
  The compile step refuses `@@scope.by` on a model of a composition chain today, so no schema reaches this rule yet.
  A proxy model uses the policy of the table that stores its rows.

SQLite gets no DDL: the ORM filter, the fill and the check still work.
Remove `@@scope.by` and the next migration drops the policies, the trigger and the flags.

## Open keys

`unscoped("shop")` sets `monorm.bypass`.
The policy lets such a statement see all rows only if the connected role is a member of a role in `bypass_roles`.
The policy looks the roles up in `pg_roles`, so a role that does not exist never matches and raises no error.
`monorm migrate` and `monorm audit` warn about a role of `bypass_roles` or `exempt_roles` that the database lacks.
A `migrate()` call from Python code does not warn: only the command line does.
If `bypass_roles` is empty, the database lets nobody through: `unscoped` then only changes the ORM.
Do not put the role of your application in `exempt_roles`: that role then reads every scope.

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
A policy that is in the database but not in the schema, such as a hand-written one, is reported as a `drop policy` difference, and `audit` exits `1`.

`pull` does not turn policies back into `@@scope.by`.
It lists them as `// gap:` lines.

## Moving from guitars

Do the move in one migration, so that no table is open in between:

1. Add `@@scope.by` to the model and run `makemigrations`.
2. In the migration, before the generated statements, drop the old objects of each table.
   Run `\d+ <table>` in `psql` to list them.
   Guitars names its policy `tenant_scope`, its exemption policies `rls_exempt_<role>`, and each autofill trigger after its function.
   Its settings `tenant.<key>` and `tenant.bypass` are not objects: the monorm policy reads `scope.<key>` and `monorm.bypass`.
3. Run `migrate`. The migration runs in one transaction.
4. Run `monorm audit` to check the result. It exits `1` if a policy of the old system is left.

## RLS is not an access-control boundary

RLS guards against mistakes in the application and against other database roles.
It does not guard against a holder of the application credential.
That holder can run `set_config('scope.shop', ...)` and read any scope.
Do not give the credential to code that you do not trust with every scope.
