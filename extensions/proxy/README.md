# monorm.proxy

Gives a model a second logical view of the same table: other fields, a narrower enum, other client defaults, and on PostgreSQL a partition.
It adds the attributes `@@proxy.of`, `@@proxy.fields` and `@@partition.values`.

## Install

Nothing to install.
The extension ships inside the one `monorm` wheel (Python module `monorm_proxy`) and the one `monorm` npm package (`monorm/extensions/proxy`).
The Rust crate is `monorm-proxy` (`publish = false`).

## Smallest example

```prisma
datasource db {
  provider = "sqlite"
}

model User {
  id          Int     @id @default(autoincrement())
  name        String
  legacy_note String?
}

model PublicUser {
  @@proxy.of(User)
  @@proxy.fields(exclude: ["legacy_note"])
}
```

```python
import monorm

models = monorm.loads(schema_text)
await db.create_tables()
await models["PublicUser"].objects.using(db).insert(name="Ada")  # writes the table "user"
print(await models["PublicUser"].objects.using(db).all())        # no legacy_note on the rows
```

```ts
import { loads } from "monorm";

const { PublicUser } = loads(schemaText);
await db.createTables();
await PublicUser.objects.using(db).insert({ name: "Ada" });
console.log(await PublicUser.objects.using(db).all());
```

`PublicUser` has no table.
Its reads and writes use the table `user`.

## How it works

**Pass.** `monorm.proxy.lower` runs in the `logical` phase.
Its effects are `behavior.extension_data.proxy`, `behavior.model_views`, `behavior.result_checks`, `storage.proxy` and `models.proxy`.
The pass turns each proxy into an ordinary model that has the fields of its source, and records the proxy in `model_views`.
The proxy keeps the table, the primary key, the column types, the enum representation and the database defaults of its root model.

**What the pass checks.**
It refuses a cycle, an unknown source, a stored field that the source does not have, a changed key, type or encoding, a relation override, a physical object on the proxy, a write rule, and an omitted field that the proxy needs (a primary key, a `NOT NULL` field with no database default, a relation key).
A redeclared field may change only nullability, the enum subset and `@client_default`.
The user doc lists each message.

**SQL.** A proxy adds no table, column or index.
`@@partition.values` is the one exception, and it works on PostgreSQL only.
It adds a partition of the source table.
A real sample from `sqlmigrate`, for a source with `@@partition(list: [kind])` and a proxy with `@@partition.values(["fixed"])`:

```sql
CREATE TABLE "reward_fixed" PARTITION OF "reward" (
    CONSTRAINT "reward_fixed_amount_key" UNIQUE ("amount"),
    CONSTRAINT "reward_fixed_expr8c33a94b_check" CHECK (amount IS NOT NULL AND amount >= 0)
) FOR VALUES IN ('fixed');
```

**Runtime.** The host reads `model_views` and `result_checks`.
A proxy has the members that other extensions add to its root model, because a model gets the members of an extension when the prepared record of that extension names it.
`result_checks` tells the native engine to check the shape of the rows that a proxy returns (non-null, enum subset).
A failed check does not fail the read.
It writes a `monorm.proxy.shape` record as JSON to stderr, once for each operation, model, field and category.

**Rust and bindings.** All logic is in the Rust crate (`src/lib.rs`).
The Python module `monorm_proxy` and the TypeScript module `monorm/extensions/proxy` are empty: the extension adds no member to the query set or the instance.

**Other extensions.** A proxy inherits the `@@query.defaults` of its source, the native model spec, and the query-set class of its root model.
A direct `use_query_set` call on the root model does not reach its proxies.

## Layout

```
extensions/proxy/
  Cargo.toml                 crate monorm-proxy
  monorm-extension.toml      manifest: one pass, three attributes
  src/lib.rs                 the pass and its unit tests
  tests/lowering.rs          pass tests
```

The extension has no `python/`, `typescript/` or `tests/public`.
The end-to-end tests are in `languages/python/tests/test_proxy.py` and `test_proxy_members.py`, and in `languages/js/test/proxy.test.ts` and `proxy-members.test.ts`.

```bash
cargo test -q -p monorm-proxy
python -m pytest -q languages/python/tests/test_proxy.py languages/python/tests/test_proxy_members.py
(cd languages/js && npm test)   # runs every test; the proxy files are proxy.test.ts and proxy-members.test.ts
```

## Limits

- A proxy cannot add a stored field, change a key, or override a relation.
- `@@partition.values` needs PostgreSQL and a source with one list key field.
- A narrowed shape gives a warning and does not filter rows or reject writes.
- A schema error starts with `extension monorm.proxy: pass monorm.proxy.lower:`.

## More

Usage, every attribute, argument and error: [docs/extensions/proxy-models.md](../../docs/extensions/proxy-models.md).
