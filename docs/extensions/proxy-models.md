# Same-table proxy models

The `orm-proxy` package (`@orm/proxy` for Node) supplies proxy models.
A proxy is a different logical view of one physical model.
Default filters and default selection come from the `orm-query-defaults` package
([selection-and-defaults.md](../schema/selection-and-defaults.md)).

**Enable:** install the `orm-proxy` package (`extensions/proxy`).
See [build-time-extensions.md](../contracts/build-time-extensions.md#installable-packages).

```prisma
enum Status {
  ACTIVE @map("active")
  OLD @map("old")
  @@storage(text)
}
model User {
  id          Int     @id
  name        String?
  status      Status  @default(OLD)
  legacy_note String?
}
model ActiveUser {
  name   String         @client_default("client default")
  status Status(ACTIVE) @client_default(ACTIVE)

  @@proxy.of(User)
  @@proxy.fields(exclude: ["legacy_note"])
}
```

A proxy inherits its source fields and relations, also through proxy chains.
It retains the root table, physical primary key, column types, enum representation,
constraints and database defaults. Without `@@partition.values`, it creates no
table or index. Physical foreign keys that target a proxy refer to its root storage owner; logical relation loads
still return the proxy model. Cycles, unknown sources, stored-field additions,
encoding changes, key changes and physical object overrides fail during definition.
A proxy cannot override a relation; inherited relations keep their targets.
A proxy cannot declare `@@protected_write`; it shares the protection of its root model.

**Inherited behavior.** A proxy has every member that another extension adds to its root model.
For example, a proxy of a soft-delete model has `all_with_deleted()`, `deleted_only()`, `undelete()` and `hard_delete()`.
A model gets the members of an extension when the prepared record of that extension names it, a proxy too.
A proxy also uses the user query-set class of its root model, unless it names its own.
This holds in generated Python and TypeScript code (`--query-set`) and in `[tool.orm.query_sets]`.
A direct `use_query_set` or `useQuerySet` call on the root model does not reach its proxies.
A method of that class may name a column of the root model, such as `Post.published.eq(true)`.
A query accepts a column of any model on the same table and resolves it by name on its own model.
A field the proxy drops gives the usual "has no field" error.

A proxy also inherits the native model spec of its nearest ancestor that has one (`methods`, computed `result_fields`, field rules, record rules, and the specialization).
A proxy with a spec of its own adds its entries to the inherited ones; an own method, field rule or computed field replaces the inherited one of the same name.
The package build leaves out a field rule or computed field when the proxy does not keep the field that it names.
An inherited record rule that needs a field the proxy drops fails the build.
The error names the rule and the field. Keep the field in the proxy, or declare an own record rule of the same export in the proxy.
The own rule replaces the inherited rule of that export only; any other inherited rule that needs a dropped field still fails the build.
Models that `orm.loads` or `define()` build at run time get extension members only from the caller.

**Inherited fields.** `@@proxy.fields(include: [...])` or
`@@proxy.fields(exclude: [...])` selects the source fields and relations by name.
With no `@@proxy.fields`, the proxy inherits all of them. `include` and `exclude`
together is an error. With `include`, a relation that is not in the list is omitted.
An omitted field is not part of the proxy model: not in its class, its types, its
reads or its writes. An insert through the proxy leaves the column to its database
default. This is different from `@query.selectOut` and `@@query.defaults(fields:)`,
which only change the default selection and keep the field.

Definition fails with the field name when an omission breaks a rule:

* The primary key cannot be omitted.
* A `NOT NULL` field without a database `@default` cannot be omitted, because an
  insert through the proxy then fails. A `@client_default` on the source field does
  not count: the proxy insert does not contain the field.
* A field that a relation of the proxy uses as its key cannot be omitted, unless the
  relation is omitted too. A field that a relation of another model references on
  the proxy cannot be omitted.
* A child proxy selects from the fields of its source proxy, not from the root model.

An omitted nullable field also drops its `@client_default`: a proxy insert stores NULL, and a parent insert stores the default.
A proxy inherits `@@query.defaults` of its source; when it names an omitted field, definition fails with `model P has no field "note"`.
Override it on the proxy with a field list without the omitted field.

**Redeclared fields** change only the logical view. A redeclared field must be in
the inherited set, and it must equal the source field in everything except
nullability, the enum subset and `@client_default`. It can leave out `@default`;
it keeps the database default, because a proxy cannot change it.

* Nullability: `name String` declares a non-null view of a nullable field. Broader
  nullability never relaxes the physical database constraint.
* Enum subset: `Status(ACTIVE, OLD)` lists parent enum member names. Only a proxy
  field takes this type argument. It keeps the full parent runtime enum, so a query
  can still return a value outside the subset as its ordinary parent enum member.
* Client default: `@client_default(...)` (see [the schema](../schema/schema.md#client-defaults))
  replaces the inherited client default. A child proxy inherits the client defaults
  of its source, and a child redeclaration replaces the inherited one. An enum
  default uses member names (`ACTIVE`), and it must be in the field's enum subset. Defaults fill omitted insert values before
  native write transforms and validators. An explicit value, also SQL NULL, wins.

The physical server default remains available to writes through the parent and
other database clients. Filters never synthesize inserted values.

A narrowed declaration is a warning contract. It adds no filter, discards no row,
and rejects no write value that storage accepts. Nulls and enum subset violations
keep their actual decoded values. Counts, ordering and pagination follow SQL exactly.
A physical decoding error remains a decoding error.

Native diagnostics emit JSON to stderr once per operation/model/field/category:

```json
{"code":"orm.proxy.shape","model":"ActiveUser","field":"status","expected_shape":{"non_null":false,"enum_members":["ACTIVE"]},"category":"enum_subset","occurrence_count":2}
```

The records contain schema metadata and counts, never raw field values. The checks
apply to publicly loaded instance fields, also related and returning shapes. The
checks do not inspect unloaded fields. Count and scalar projection results give no
proxy warnings. A missing LEFT JOIN object is an absent object, not a null-shape
violation. Partial public shapes come from `only()` ([selection-and-defaults.md](../schema/selection-and-defaults.md)).

Generated Python and TypeScript types describe the intended non-null/subset view.
These contracts give warnings and do not enforce the shape, thus returned values
can violate those hints. Strict validation requires a separately declared validator.

The proxy pass emits `model_views`, `result_checks` and `storage`, which the host reads.
Its own data stays in `extension_data["orm.proxy"]`.
The host has no proxy feature and no proxy record.
Without the installed package, `@@proxy.of` fails at definition with an unknown attribute.

## Owning a list partition (PostgreSQL)

A proxy of a list-partitioned model can own one partition:

```prisma
model Reward {
  id     Int @default(autoincrement())
  kind   String
  amount Int?
  @@id([id, kind])
  @@partition(list: [kind])
}
model FixedReward {
  amount Int
  @@proxy.of(Reward)
  @@proxy.fields(exclude: ["kind"])
  @@partition.values(["fixed"])
  @@map("reward_fixed")
  @@check("amount IS NOT NULL AND amount >= 0")
  @@unique([amount])
}
```

`@@map` names the physical partition; its default is the proxy name in lower case.
The proxy's checks, indexes, unique rules, exclusion rules, triggers and comment
belong to that partition. Parent rules remain inherited. A matching partition
block produces the same migration objects: transferring ownership between a block
and a proxy produces no migration.

With one bound value, inserts fill the key automatically, including when it is
excluded in `@@proxy.fields`. The fixed key remains available for composite
identity and read-back. With several values, expose the key and supply it on
insert. Enum bounds name members and use their mapped database values. Inserts
write directly to the owned partition, so PostgreSQL rejects a key outside its
bound and enforces its local checks, including explicit `IS NOT NULL` checks.

Reads, counts, updates and deletes use the parent with a permanent key predicate.
`without_defaults()` removes inherited query defaults but keeps that predicate;
PostgreSQL can prune the other partitions. Related loads and ordinary child
proxies keep the storage predicate too. Source-model queries return source
instances from every partition. Proxies that retain the source soft-delete field
inherit its ORM delete behavior without declaring duplicate database triggers.
Updates through the source can move rows between partitions when the destination
checks allow them. An update through a proxy with several bound values can also
change its exposed key to a value outside those bounds. The update succeeds when
the destination partition accepts the row, and the row is no longer visible
through that proxy.

The source must have exactly one list key field. Partition ownership on a proxy
of a proxy, overlapping block/proxy values, invalid enum members and SQLite
partition declarations fail during definition. Uncovered enum members warn unless
a default block exists.

On a source with `autoincrement()`, an insert, `copy` insert or upsert through a
proxy that owns a partition needs PostgreSQL 17 or newer. These writes go directly
to the partition table, and PostgreSQL 16 does not give a partition the identity
of its parent: the insert fails with a not-null error on the key. Reads, updates
and deletes through the proxy, and all writes through the source model, work on
PostgreSQL 16.
