# scope

`@@scope.by(shop: shop_id)` ties a table to a scope key, for multi-tenant data.
The ORM filters reads, fills the column on insert and checks every write.
On PostgreSQL the database enforces the same rule with row-level security (RLS).
The attribute takes one `key: field` pair for each scope key, and no other argument.

## Install

The extension ships inside the one `monorm` package.
Python: `pip install monorm`.
TypeScript: `npm install monorm@npm:@ufstack/monorm`.
There is no separate wheel or npm package.

## Smallest example

```prisma
model Order {
  id      Int @id @default(autoincrement())
  shop_id Int
  total   Int
  @@scope.by(shop: shop_id)
}
```

```python
with monorm.scope(shop=1):
    await Order.objects.insert(total=5)   # shop_id is 1
    await Order.objects.all()             # only the rows of shop 1
```

```ts
await scope({ shop: 1 }, async () => {
  await Order.objects.insert({ total: 5 }); // shopId is 1
  await Order.objects.all();                // only the rows of shop 1
});
```

## How it works

### Schema pass

* `monorm.scope.prepare` (behavior phase) adds, for each key, a read default and a scoped field (the insert fill and the write check).
  It also adds the RLS objects for PostgreSQL. A schema may use several keys: `@@scope.by(shop: shop_id, mode: livemode)`.
* A composed root gets the scope; a level table below it gets a policy with `EXISTS` on its direct parent.
* `@@scope.by` on a composed child below the root fails the schema: `declare it on <root>`.

### SQL

PostgreSQL only; SQLite gets no DDL.
The sample is the migration of `Order`:

```sql
CREATE OR REPLACE FUNCTION "order_monorm_scope_fill"() RETURNS trigger LANGUAGE plpgsql AS $orm$
BEGIN IF NEW."shop_id" IS NULL THEN NEW."shop_id" := (SELECT NULLIF(current_setting('scope.shop', true), '')::integer); END IF; RETURN NEW; END;
$orm$;
CREATE TRIGGER "monorm_scope_fill" BEFORE INSERT ON "order" FOR EACH ROW EXECUTE FUNCTION "order_monorm_scope_fill"();
CREATE POLICY "monorm_scope" ON "order" AS PERMISSIVE FOR ALL USING (("shop_id" = (SELECT NULLIF(current_setting('scope.shop', true), '')::integer))) WITH CHECK (("shop_id" = (SELECT NULLIF(current_setting('scope.shop', true), '')::integer)));
ALTER TABLE "order" ENABLE ROW LEVEL SECURITY, FORCE ROW LEVEL SECURITY;
```

* For each role in `exempt_roles` of the `datasource` block, a policy `monorm_scope_exempt_<role>` (`FOR SELECT`).
* The policy `USING` clause also lets a statement through when the setting `monorm.bypass` names the key and the role is in `bypass_roles`.

### Runtime hooks

* The scope runtime is in the host, not in the extension: `monorm.scope(**values)` and `monorm.unscoped(*keys)` in Python, `scope({ k: v }, fn)` and `unscoped([keys], fn)` in TypeScript.
* The ORM sets `scope.<key>` in the first statement of the transaction.
* Errors: `ScopeMismatch` (`.model`, `.key`, `.expected`, `.actual`) for a write to another scope, and `ScopeMissing` (`.model`, `.key`, a `QueryError`) for a read or a write with no scope.
  A bad scope raises `ValueError` in Python and `TypeError` in TypeScript.
  `unscoped()` with no key raises `unscoped() needs at least one key: name each scope key to open`.

### Crate and bindings

The crate `monorm-scope` holds the schema pass and the RLS SQL.
The extension has no `python/` or `typescript/` source.

### Other extensions

* `history` publishes its own scope keys (`behavior.published_scope_keys`), and the scope extension publishes its keys too.
* The read default of a scope key is `behavior.read_defaults`. Soft delete declares that effect as well (see Limits).
* `query-defaults` can read the scope in a filter: `@@query.filter("tenant", shop_id == scope.shop)`.

## Layout

```
extensions/scope/
  monorm-extension.toml
  src/lib.rs
  tests/fixtures/schema.prisma
  tests/public/{python.py,node.mjs}
```

Run `cargo test -q -p monorm-scope` for the crate.
Run `python extensions/scope/tests/public/python.py` and `(cd languages/js && node ../../extensions/scope/tests/public/node.mjs)` for the public tests.
Set `ORM_TEST_DATABASE_URL` to run them on PostgreSQL too.
The behavior tests are `languages/python/tests/test_scope.py` and `test_scope_rls.py`, and `scope.test.ts` and `scope-rls.test.ts` in `languages/js/test/`.

## Limits

* A schema with a `@@scope.by` model and a soft-delete model fails to load: `conflicting effect behavior.read_defaults (monorm.scope.prepare and monorm.soft-delete.prepare)`.
  A schema with `@@scope.by` and `@@history.track(scope: [...])` fails the same way for `behavior.published_scope_keys`.
  The failure happens even when the attributes sit on different models.
  The check is in `crates/contracts/src/resolve.rs`.
* RLS guards against mistakes in the application and against other database roles, not against a holder of the application credential.
* `pull` does not turn policies back into `@@scope.by`: it lists them as `// gap:` lines.
* `skip_triggers` cannot skip the scope trigger.
* Raw SQL (`db.execute`, `db.fetch`) carries no scope setting.

User doc: [docs/extensions/scope.md](../../docs/extensions/scope.md).
