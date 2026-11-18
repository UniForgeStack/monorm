# monorm.generic

Lets one row point to a row of one of several models, with a type field and a key field and no foreign key.
It adds the attributes `@generic.relation` (field form), `@@generic.relation` (model form) and `@generic.reverse`, and the field type `Generic`.

## Install

Nothing to install.
The extension ships inside the one `monorm` wheel (Python module `monorm_generic`) and the one `monorm` npm package (`monorm/extensions/generic`).
The Rust crate is `monorm-generic` (`publish = false`).
The frozen model identities and the `ContentType` enum belong to the host and work in every build.

## Smallest example

```prisma
datasource db {
  provider = "sqlite"
}

model Post {
  id   Int   @id
  tags Tag[] @generic.reverse
}

model Photo {
  id Int @id
}

model Tag {
  id     Int      @id
  target Generic? @generic.relation(targets: ["Post", "Photo"])
}
```

The schema needs a manifest of model identities.
Create it once and commit it:

```bash
python -m monorm --schema schema.prisma identities
```

```python
import monorm

registry = monorm.Registry()
models = monorm.load("schema.prisma", registry=registry)
ContentType = registry.get_enum("ContentType")
await models["Tag"].objects.using(db).insert(id=1, target_type=ContentType.Post, target_id=7)
```

```ts
import { loads, Registry } from "monorm";

const registry = new Registry();
const { Tag } = loads(schemaText, { registry });
const ContentType = registry.getEnum("ContentType");
await Tag.objects.using(db).insert({ id: 1, targetType: ContentType.Post, targetId: 7 });
```

`target` becomes two columns, `target_type` (the integer enum `ContentType`) and `target_id` (the type of the shared target key).

## How it works

**Pass.** `monorm.generic.prepare` runs in the `behavior` phase.
Its effects are `behavior.extension_data.generic` and `storage.generic_checks`.
For each relation the pass creates the pair fields (or finds the fields that `type:` and `key:` name), adds the pair index, and records the relation and its reverse in `behavior.extension_data["monorm.generic"]`.
The `ContentType` enum and the id of each model come from the host: the file `schema.identities.json` freezes them.

**What the pass checks.**
It refuses an empty or repeated target list, a target that has no single scalar primary key, targets whose keys have different types, a type field that is not the `ContentType` integer enum, a pair that has a different nullability for each field, a pair with a server default, a name that collides with a member, and a reverse field with no matching relation.
The user doc lists each message.

**SQL.** No foreign key.
The pass adds two check constraints and one index.
A real sample from `sqlmigrate` on PostgreSQL:

```sql
CONSTRAINT "tag_target_type_enum_check" CHECK ("target_type" IN (1, 2, 3))
CONSTRAINT "tag_exprd8fa822b_check" CHECK ((("target_type" IS NULL) = ("target_id" IS NULL)) AND ("target_type" IN (1, 2)))
CREATE INDEX "tag_target_type_target_id_idx" ON "tag" ("target_type", "target_id");
```

The first check comes from the host (the enum).
The second check is the `storage.generic_checks` effect: the pair is both null or both set, and the type is one of the targets.
SQLite gets the same checks.

**Runtime.** No runtime hook.
The type and key fields are ordinary fields.
The extension does not check that the target row exists.

**Rust and bindings.** All logic is in the Rust crate (`src/lib.rs`).
The Python module `monorm_generic` and the TypeScript module `monorm/extensions/generic` are empty.

**Other extensions.** The `proxy` and `composition` passes copy fields before this pass runs.
A proxy or a composed child cannot keep a `Generic` field: exclude the field there, or use the model form.
The manifest keys `field_type = "Generic"` and `member = false` are host features that other extensions can use too.

## Layout

```
extensions/generic/
  Cargo.toml                 crate monorm-generic
  monorm-extension.toml      manifest: one pass, three attributes
  src/lib.rs                 the pass and its unit tests
```

The extension has no `python/`, `typescript/`, `tests/` or `tests/public`.
The end-to-end tests are in `languages/python/tests/test_generic_identities.py` and `languages/js/test/identities.test.ts`.

```bash
cargo test -q -p monorm-generic
python -m pytest -q languages/python/tests/test_generic_identities.py
(cd languages/js && npm test)   # runs every test; the file is identities.test.ts
```

## Limits

- The database does not check that the target row exists.
- A half pair, or a type that is not a target, raises `IntegrityError`.
- The bindings have no generic accessor, loader, prefetch, reverse query API or generated target union.
- `monorm pull` reads the columns back as plain fields and writes no `@generic.relation`.
- A schema error starts with `extension monorm.generic: pass monorm.generic.prepare:`.

## More

Usage, every argument, the identity manifest and every error: [docs/schema/generic-identities.md](../../docs/schema/generic-identities.md).
