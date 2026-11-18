# Frozen model identities

`monorm identities` explicitly generates `schema.identities.json` beside the root
schema. It collects the root and its explicit `.prisma` imports, preserving table
prefixes and the separate extension catalog import mechanism. It never scans
neighboring schemas. Commit the manifest with the schema.

Initial positive signed 32-bit IDs are allocated in model-name order. Subsequent
models receive IDs above the highest allocation; existing values never move.
Removed models retain permanent retired entries. Reintroducing a name allocates a
new ID unless `monorm identities --restore Model` explicitly restores its single
retired identity; the entry's `restorations` count records that intent, and a
manifest that reactivates a tombstone without it is rejected. Renames use
`monorm identities --rename Old=New`. Rename chains,
including a return to an original name, retain the original ID and history.
The command prints each added, renamed, retired, and restored entry.
Moving a declaration between files or changing its table mapping preserves its ID.

Compilation, generation, runtime definition and migration planning only read
identities. Missing/stale mappings fail with a generation instruction. Compilation
synthesizes one ordinary integer-storage `ContentType` enum, ordered by ID; Python
uses IntEnum and TypeScript named integer members. Its values agree in generated
bindings and runtime-loaded models. Python obtains runtime members from
`registry.get_enum("ContentType")`; TypeScript uses `registry.getEnum("ContentType")`.
Inline `define()` consumes compiled IR with embedded `identities` and `enums`.
No database metadata table or registration rows are created.

Identity-bearing migration snapshots use version 3 (version 4 when the schema has partitions, and a higher version when it has SQL views). They retain all retired IDs
and reject removed/reassigned historical allocations. Identity-only changes still
produce a reviewable snapshot migration. Ordinary integer enum constraints prevent
unknown/retired values in referencing columns; a retirement migration fails if
existing rows still contain the removed discriminator. Clean up those references
explicitly before retrying. Physical model removal is ordinary migration behavior.

Concrete names and generated Python/TypeScript names must be globally unambiguous.
Physical tables cannot collide under database identifier rules. Logical proxies
are excluded using the typed proxy setup metadata and resolve to their concrete
storage owner; they do not receive independent identities.

An ordinary handwritten enum called ContentType remains an ordinary enum when no
identity manifest/generic relation requests synthesis. Declaring an enum never
enables generic relation behavior by itself.

## Generic relation compiler contribution

The built-in `monorm.generic` extension declares these namespaced attributes.
The field form declares a generic relation as one field of the reserved type `Generic`:

```prisma
model Tag {
  id     Int      @id
  target Generic? @generic.relation(targets: ["Post", "Photo"])
}

model Post {
  id   Int   @id
  tags Tag[] @generic.reverse
}
```

`target` creates two fields at its position: `target_type ContentType?` and `target_id`.
`target_id` has the type of the targets' shared primary key; targets with different key types fail.
`?` sets the nullability of both fields; a field that `type:` or `key:` names must have the same nullability.
`target_id` takes only the storage encoding of the target key (`@db.*`, length); never its key, default or `@client_default`.
The pass also adds `@@index([target_type, target_id])`; `index: false` disables it.
An equal `@@index` replaces the added index.
`type:` and `key:` name existing fields instead of created ones.
A schema in the explicit form moves to the field form with no migration in two cases.
It keeps its pair `@@index`, or it uses `index: false` and has no pair index.
A created name that collides with a member fails; name an existing field with `type:` or `key:` then.
`Generic` is only a type on a field with `@generic.relation`; elsewhere it is an unknown type.
A `Generic` field takes no other attribute, such as `@map`, `@unique` or `@db.*`; use the explicit form for those.
A `@generic.reverse` field takes no other attribute.
The error for such an attribute says "remove it"; for `@map`, `@unique`, `@db.*` and defaults on a `Generic` field, use the explicit form.
The manifest of `@generic.relation` sets `field_type = "Generic"`, the manifest of `@generic.reverse` sets `member = false`, and both set `identities = true`.
Any extension can use these keys for its own type or non-member field.
The proxy and composition passes copy fields before the generic pass runs.
So a proxy or a composed child cannot keep a `Generic` field: exclude the field there, or use the explicit form.

`tags Tag[] @generic.reverse` is the reverse side: the field name is the relation name, and `Tag[]` is the source.
It is no ordinary relation, because no foreign key exists.
`relation: "target"` names the source relation; it is necessary only when the source has more than one generic relation with this model in its targets.
A reverse field fails when the source has no generic relation with this model in its targets.
The model-level form `@@generic.reverse("tags", source: "Tag", relation: "target")` is removed; write `tags Tag[] @generic.reverse(relation: "target")` instead.

The explicit form stays available, because it allows `@map`, `@db.*` and comments on the pair fields:

```prisma
model Tag {
  id           Int          @id
  content_type ContentType?
  object_id    Int?

  @@generic.relation("target", type: "content_type", key: "object_id", targets: ["Post", "Photo"])
  @@index([content_type, object_id])
}
```

The compiler validates allowed concrete targets, compatible single-column key
encodings and matching pair nullability. It contributes ordinary check constraints
for paired nulls and the allowed discriminator subset, plus forward/reverse setup
metadata. It contributes no conventional multi-target foreign key. Database checks
preserve pair validity for external scalar writes without promising target existence.

**Enable:** nothing to install. The extension is built into the `monorm` Python wheel (module `monorm_generic`) and the `monorm` npm package (`monorm/extensions/generic`). It has no Python or TypeScript code of its own. No build is needed.
See [Installable packages](../contracts/optional-packaging.md#installable-packages).
The extension asks for the host capabilities `schema-transformations` and `physical-schema`, and for no host feature.
Its pass is `monorm.generic.prepare` (phase `behavior`). It declares the effects `behavior.extension_data.generic` and `storage.generic_checks`.
Its relation and reverse records live in `behavior.extension_data["monorm.generic"]`. The host has no generic code.
Models generated for schema contract 2 fail to load with an instruction to regenerate them.
Frozen identities and `ContentType` work in every build.
How it works, the layout and the tests: [`extensions/generic/README.md`](../../extensions/generic/README.md).

## Attributes

| Attribute | On | Arguments (default) |
|---|---|---|
| `@@generic.relation("name", ...)` | model | `type:` (required), `key:` (required), `targets:` (required list) |
| `@generic.relation(...)` | field of type `Generic` or `Generic?` | `targets:` (required list), `type:` (created field `<name>_type`), `key:` (created field `<name>_id`), `index:` (`true`) |
| `@generic.reverse` | list field of the source model, such as `Tag[]` | `relation:` (the only generic relation of the source that has this model in its targets) |

`@@generic.reverse(...)` on a model is removed.

## Use

The pair fields are ordinary fields.
The type field is an integer enum, `ContentType`, and the key field has the type of the target key.

```python
import monorm

registry = monorm.Registry()
models = monorm.loads(open("schema.prisma").read(), registry=registry)
Tag = models["Tag"]
ContentType = registry.get_enum("ContentType")
db = await monorm.connect(url, registry=registry, default=False)
await Tag.objects.using(db).insert(id=1, target_type=ContentType.Post, target_id=7)
tags = await Tag.objects.using(db).filter(Tag.target_type == ContentType.Post, Tag.target_id == 7)
```

```ts
import { connect, loads, Registry } from "monorm";

const registry = new Registry();
const { Tag } = loads(schemaText, { registry });
const ContentType = registry.getEnum("ContentType");
const db = await connect(url, { registry, default: false });
await Tag.objects.using(db).insert({ id: 1, targetType: ContentType.Post, targetId: 7 });
const tags = await Tag.objects.using(db).filter(Tag.targetType.eq(ContentType.Post), Tag.targetId.eq(7));
```

A query is awaitable: `await queryset` returns the list of rows.
The TypeScript members are camel case (`targetType`, `targetId`).

The migration adds the two columns, a check for the allowed type values and for a paired null, and the pair index.
Sample from PostgreSQL (`monorm sqlmigrate`). The hash in the name of the second check depends on its text:

```sql
CONSTRAINT "tag_target_type_enum_check" CHECK ("target_type" IN (1, 2, 3))
CONSTRAINT "tag_exprd8fa822b_check" CHECK ((("target_type" IS NULL) = ("target_id" IS NULL)) AND ("target_type" IN (1, 2)))
CREATE INDEX "tag_target_type_target_id_idx" ON "tag" ("target_type", "target_id");
```

`pull` reads these columns back as plain fields. It writes no `@generic.relation`.

## Errors

Schema errors have the prefix `extension monorm.generic: pass monorm.generic.prepare:` and the position of the attribute.

| Message | Fix |
|---|---|
| `@generic.relation: targets must be nonempty and unique` | List each target once. |
| `@generic.relation: unknown target Nope` | The target is not a model. |
| `@generic.relation: targets' primary keys have different types; use @@generic.relation with an explicit key field` | Use the explicit form with a `key:` field of a common type. |
| `@generic.relation: type field k must match the nullability of Generic?` | Give both fields the same nullability. |
| `@generic.relation: Tag.t_type: the created type field collides with a member; name an existing field with type:` | Name another field with `type:` or `key:`. |
| `@generic.reverse: Tag has no generic relation with Q in its targets` | The source relation does not list this model. |
| `@generic.reverse: must be on a list field of the source model, such as Tag[]` | Declare the reverse as a list. |
| `@generic.reverse: Tag has more than one generic relation to Post; name one with relation:` | Add `relation: "<name>"`. |

The host raises two more errors before the pass runs:
`Tag.t: @map is not allowed on a Generic field; remove it`, and `Tag.t: unknown type Generic` for a `Generic` field without `@generic.relation`.

## Limits

- The check does not test that the target row exists. A pair that names a missing row inserts. A half pair, or a type that is not a target, raises `IntegrityError`.
- The bindings have no generic accessor, loader, prefetch, reverse query API or generated target union.
