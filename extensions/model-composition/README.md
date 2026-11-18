# monorm.composition

Stores a model in its own table and reads the fields of its parent from the parent table, with the child key as a foreign key to the parent key.
It adds the attribute `@@composition.model(parent:, parentRef:, childRef:)`.

## Install

Nothing to install.
The extension ships inside the one `monorm` wheel (Python module `monorm_composition`) and the one `monorm` npm package (`monorm/extensions/model-composition`).
The Rust crate is `monorm-composition` (`publish = false`; the directory is `model-composition`).

## Smallest example

```prisma
datasource db {
  provider = "sqlite"
}

model Person {
  id   Int    @id @default(autoincrement())
  name String
  @@map("people")
}

model Employee {
  salary Int
  @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
  @@map("employees")
}
```

```python
import monorm

models = monorm.loads(schema_text)
await db.create_tables()
bob = await models["Employee"].objects.using(db).insert(name="Bob", salary=10)  # a row in people and a row in employees
print(bob.name, bob.salary)
```

```ts
import { loads } from "monorm";

const { Employee } = loads(schemaText);
await db.createTables();
const bob = await Employee.objects.using(db).insert({ name: "Bob", salary: 10 });
console.log(bob.name, bob.salary);
```

## How it works

**Pass.** `monorm.composition.lower` runs in the `logical` phase.
Its effects are `behavior.field_storage`, `behavior.owner_links`, `behavior.storage` and `models.composition`.
For each child the pass:

- creates the child primary key, with the type of the parent key (a child `@id` is allowed if it has that type and no default);
- adds the inherited fields to the child as fields that live in the parent table (`field_storage`, `owner_links`);
- adds the relation `parentRef` on the child and the reverse relation `childRef` on the parent.

**What the pass checks.**
All three arguments are required strings, and no other argument or positional value is allowed.
It also refuses an unknown parent, a cycle, two parents for one child, a child that shares the table of its parent, a parent without one primary key, a child `@id` that differs from the parent key or has a default, and a name clash between a relation and a field.
The user doc lists each message.
The pass works on a copy of the schema, so a refused schema changes nothing.

**SQL.** One table for each model.
The child key is also a foreign key to the parent key.
The relation `childRef` has no foreign key.
A real sample from `sqlmigrate` on PostgreSQL (the `employees` table of the doc schema):

```sql
CREATE TABLE "employees" (
    "id" integer NOT NULL,
    "salary" integer NOT NULL,
    CONSTRAINT "employees_pkey" PRIMARY KEY ("id"),
    CONSTRAINT "employees_id_fkey" FOREIGN KEY ("id") REFERENCES "people" ("id") ON DELETE CASCADE
);
```

**Runtime.** The engine runs the writes, in `crates/engine/src/multi_table.rs`.
A read or a filter on an inherited field becomes a join through the parent tables.
`insert` writes the root row, then each child row, in one transaction.
`attach` and `attach_many` add a child to an existing parent.
`insert_many` on PostgreSQL uses L + 2 statements for L levels.

**Rust and bindings.** The Rust crate has the pass only.
The write paths are in the engine.
The Python module `monorm_composition` and the TypeScript module `monorm/extensions/model-composition` are empty.
The `attach` and `attachMany` members come from the host, which adds them to a composed model.

**Other extensions.** Column roles (`updated_at`, soft delete, version) live on the root table.
A change on any level bumps the root, and a soft delete updates only the root.

## Layout

```
extensions/model-composition/
  Cargo.toml                 crate monorm-composition
  monorm-extension.toml      manifest: one pass, one attribute
  src/lib.rs                 the pass
  tests/lowering.rs          pass tests
  tests/engine.rs            engine write and read tests
```

The extension has no `python/`, `typescript/` or `tests/public`.
The end-to-end tests are in `languages/python/tests/test_composition.py` and `test_composition_roles.py`, and in `languages/js/test/composition.test.ts` and `composition-roles.test.ts`.

```bash
cargo test -q -p monorm-composition
python -m pytest -q languages/python/tests/test_composition.py languages/python/tests/test_composition_roles.py
(cd languages/js && npm test)   # runs every test; the composition files are composition.test.ts and composition-roles.test.ts
```

## Limits

- `update_many` on a composed model is rejected.
- `outer()` and `distinct(on=...)` on inherited columns raise `QueryError: ... requires an explicit owner projection`.
- An ordinary model cannot write inherited storage.
- `attach` takes the local fields of the child only.
- A composed write inside a caller transaction opens no savepoint; a failed statement marks the caller transaction.
- `monorm pull` writes ordinary models with a one-to-one relation and no `@@composition.model`.
- A schema error starts with `extension monorm.composition: pass monorm.composition.lower:`.

## More

Usage, every argument and error: [docs/extensions/model-composition.md](../../docs/extensions/model-composition.md).
