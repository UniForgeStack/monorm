# Schema language, extensions and migrations

The schema is written once, in a `.prisma` file, and every language uses it:

```
schema.prisma ──▶ orm-core (Rust) ──▶ schema IR (JSON) ──▶ Python: orm.load() / generated models.py + .pyi
   ▲                 │                                └──▶ TypeScript: loads() / generated models.ts
extension files      │
  postgres/*.toml    │
                     ├──▶ migrations/NNNN_name/{up.sql, down.sql, snapshot.json}
                     └──▶ PostgreSQL / SQLite DDL (create_tables)
```

The file uses Prisma's schema syntax, so Prisma's VS Code extension highlights,
formats and completes it. What Prisma has no syntax for (checks, exclusions,
triggers, functions, ...) is written as our own attributes and blocks; the editor
marks those lines as errors, which is expected. Nothing calls Prisma's CLI or
engines: our parser, migrator and code generators do the work. The reasoning, and
the mapping from the old `.orm` syntax, is in [`prisma-syntax.md`](prisma-syntax.md).

No Python, Rust or JS class describes the schema. The parser, the extension catalog,
the migration generator and the code generators live in the binding-free `crates/core/` crate,
so each language binding only loads the result. The `orm` command-line tool (`crates/cli/`,
crate `orm-cli`) uses the same crate and needs no Python.

| Path | What |
|---|---|
| `crates/core/src/dsl/` | parser (`syntax.rs`) and lowering to the IR (`lower.rs`) |
| `crates/core/src/ext.rs`, `crates/core/extensions/postgres/*.toml` | extension files and the catalog built from them |
| `crates/core/src/migrate/` | snapshot model, diff, PostgreSQL / SQLite renderers, migration folders |
| `crates/core/src/codegen/python.rs`, `typescript.rs` | `models.py` / `models.pyi` and `models.ts` generators |
| `crates/cli/src/` | the `orm` CLI |
| `languages/python/orm/model.py` | `orm.load()` / `orm.loads()` / `orm.define()`: model classes from the IR |
| `languages/python/orm/migrations.py`, `__main__.py` | migration runner and `python -m orm` |

## SQLite

Set the schema target explicitly; omitting a datasource keeps PostgreSQL as the
default. See [the SQLite example](../../examples/sqlite/schema.prisma).

```prisma
datasource db {
  provider = "sqlite"
}

enum Status {
  draft
  published
  @@storage(text)
}
```

Use `sqlite://:memory:` for an in-memory database or
`sqlite:///absolute/path.db` for a persistent file. Load or generate the models
from that SQLite schema, then pass the URL to Python's `orm.connect()` or
TypeScript's `connect()`. A registry accepts schemas for one dialect; connecting
it to another dialect fails before queries run.

Each client owns one serialized connection on a dedicated worker thread.
`max_connections` / `maxConnections` does not create a SQLite pool. Transactions
hold that connection until commit or rollback; nested transactions use savepoints.
Foreign key enforcement is enabled. Separate clients using `:memory:` have
separate databases.

Enums must choose `@@storage(text)` or `@@storage(int)` explicitly. JSON and UUID
values use text storage; `String @db.Uuid` retains UUID conversion. Dates use
`DateTime @db.Date`, and timestamps round-trip in UTC. `now()` uses SQLite's
`CURRENT_TIMESTAMP`. AUTOINCREMENT requires an integer primary key. Aggregates,
windows, CTEs, sliced prefetch and bulk updates use the shared query APIs.

Unsupported features fail schema validation: exact Decimal, arrays, native enums,
PostgreSQL extensions and declared SQL functions, native type overrides and SQL
conversion templates, comments, exclusion constraints, covering indexes, non-btree
index methods, operator classes, explicit NULL ordering in indexes, index storage
parameters, deferrable uniqueness and NULLS NOT DISTINCT. SQLite supports inline
row triggers with one INSERT, UPDATE or DELETE event. Statement triggers, TRUNCATE,
INSTEAD OF on tables, trigger functions, languages and arguments are unsupported.
Row/advisory locks and DISTINCT ON fail query validation. Raw SQL expressions and
trigger bodies must use SQLite syntax. Generated/computed columns remain out of
scope.

Any schema change currently rebuilds every managed table in one transaction.
Rebuilds copy rows, apply table/column rename hints, preserve AUTOINCREMENT high
water marks, recreate managed indexes/triggers, and validate foreign keys before
commit. A failure rolls back the schema, data and migration record. Unmanaged
indexes or triggers attached to a managed table stop the migration with
`orm_unmanaged_index_or_trigger`; incorporate them into the schema or explicitly
remove them before retrying. Review generated up/down SQL before applying it:
dropped columns cannot recover their old values on downgrade. Avoid rebuilding
large databases without planning for the copy cost.

SQLite snapshots use version 2 and record their dialect. PostgreSQL snapshots
retain version 1 for compatibility. Cross-dialect snapshots are rejected; use a
separate migrations directory for each target.

## A schema

```prisma
// examples/blog/schema.prisma (excerpt)
model Post {
  id         BigInt    @id @default(autoincrement())
  author_id  BigInt
  title      String    @db.VarChar(200)
  body       String    @db.Text
  views      Int       @default(0)
  published  Boolean   @default(false)
  created_at DateTime  @default(now())
  author     User      @relation(fields: [author_id], references: [id], onDelete: Cascade)
  comments   Comment[]

  @@index([author_id])
  @@index([author_id, created_at(sort: Desc)], where: raw("published"))
  @@index([title(ops: raw("gin_trgm_ops"))], type: Gin) // pulls in pg_trgm
  @@map("posts")
  @@check("views >= 0", name: "posts_views_not_negative")
}
```

Using it from Python:

```python
models = orm.load("schema.prisma")       # compile at runtime; models["Post"] ...
# or generate a module (typed, autocompletes):  python -m orm generate
from blog.models import Post, User
```

## Language reference

As in Prisma, each field and each attribute is on its own line. Comments are `// ...`.
Strings are `"..."` (escapes `\" \\ \n \t`) or `"""..."""`, taken verbatim with common
indentation removed, which suits SQL bodies. Raw SQL (predicates, expressions,
bodies) is written as it appears in the DDL, using column names.

Two rules keep a file intact when the Prisma formatter runs on save, and the parser
enforces both:

1. **Each attribute is on one line.** The formatter would tear a split `@@trigger(...)`
   apart. Put a long trigger body in a `function` block.
2. **`"""` strings only appear in top-level blocks** (`function`, `datasource`), never
   inside a model. Inside a model, write `"...\n..."` on one line.

The parser doesn't depend on the order of `@@` attributes (the formatter puts Prisma's
own first and ours after them).

### Top level

```prisma
import "extensions/acme.toml" // an extension file (path relative to this file)

datasource db {
  provider   = "postgresql"
  url        = env("DATABASE_URL") // accepted and ignored: the URL comes from ORM_DATABASE_URL
  extensions = [postgis(schema: "ext", version: "3.4"), uuid_ossp(map: "uuid-ossp")]
}

function audit_row {
returns  = trigger
args     = ""
language = plpgsql
body     = """
BEGIN
INSERT INTO audit_log (tbl) VALUES (TG_TABLE_NAME);
RETURN NULL;
END;
"""
}
```

* The `datasource` is optional (Postgres is implied), but Prisma's editor wants one.
  `extensions` pins where an extension goes and which version; extensions the schema
  uses are added anyway. `map:` gives the real name of one that isn't an identifier.
* `function name { ... }` is a stand-alone SQL function: `returns` (default `trigger`),
  `args` (the SQL argument list, e.g. `"a integer, b text"`), `language` (default
  `plpgsql`), `volatility` (`immutable` / `stable` / `volatile`), `security_definer`,
  `body`. The formatter removes the indentation inside it, which SQL doesn't mind.
* `generator` blocks (Prisma Client's) are read, and only `extensions = ["id", ...]` has an effect:
  it selects installed extension libraries by id (see [Installed extension libraries](#installed-extension-libraries)). `type` and `view`
  aren't supported yet; `enum` is below.

A model's table is the model name in lower case, or `@@map("name")`.
`@@comment("...")` and `@@renamed_from("old_table")` set its comment and previous name.

### Optional schema imports

Prefer a single schema file for an ordinary project. Explicit schema imports are
available for reusable libraries or a small module that needs its own generated
models; no directory discovery is required. A schema from an installed package
needs no path: see [Installed app import](#installed-app-import).

```prisma
// schema.prisma: the main schema
import "billing/schema.prisma" (prefix: "billing_")
import "audit/schema.prisma" // prefix is optional

datasource db {
  provider = "postgresql"
  extensions = [pg_trgm]
}
```

Import paths resolve relative to the file containing them. Only `.prisma` imports
load schemas; existing `.toml` imports still load extension definitions. Imports
must be explicit so compilation has one main schema that owns the datasource,
dialect and extension configuration. Imported schemas cannot declare a datasource
or import extension definitions; put those settings and definitions in the main
schema. Imported models can use the main schema's extension types. As usual,
extension usage is inferred across the complete compiled schema, and the database
URL comes from the connection API or `ORM_DATABASE_URL`.

All imported models, enums and SQL functions compile together, with one migration
history. Relations can reference models in another file directly. Model and enum
names must be globally unique; a table prefix does not create a model namespace.
A prefix is prepended to default table names, explicit `@@map` names and
`@@renamed_from` table names. Nested imports concatenate their prefixes. Enum type
names, explicit constraint/index names and raw SQL are preserved; use unique
explicit names and write raw SQL with the final table names. Cycles and repeated
imports, including the same file through different paths, are errors.

#### Installed app import

`import "app:<name>"` imports the schema of an installed Python or npm package.
`<name>` is the distribution name (`orm-auth` on PyPI and on npm), so one string works
in a Python project and in a TypeScript project.

```prisma
import "app:orm-auth" (prefix: "auth_")
```

* The app declares its schema in its package metadata. Without it, the import is an error that names the missing metadata. There is no default file name.
  * Python: an entry point in the group `orm.apps`, named like the distribution. The value is `<package>:<schema file>`, for example
    `orm-auth = "orm_auth:schema.prisma"` under `[project.entry-points."orm.apps"]` in `pyproject.toml`.
    `pyproject.toml` accepts only this form and rejects a `/` path. A subpackage is `orm_auth.db:schema.prisma`.
  * npm: `"orm": {"app": "./schema.prisma"}` in `package.json`.
* Resolution happens when you run `orm generate`, `orm makemigrations` and `orm migrate`, and in `load(path)` and `Migrations(path)`. It reads package metadata and imports no app code.
  `loads(source)` has no file, so an app import in it is an error: use `load(path)` or generate the models.
  The command line finds the app, and the schema loader gets a map from the name to the file. It does not import the app package.
  * Python: `importlib.metadata` finds the entry point, and `importlib.util.find_spec` finds the package directory. It works for an editable install (`uv pip install -e`).
  * Node: from the directory of the importing schema (its real path), it goes up through `node_modules/<name>/package.json`. This resolves an app that only another app depends on under pnpm's strict layout.
  * The standalone `orm` binary does not look for packages. Give it the file: `orm generate --app orm-auth=path/to/schema.prisma`. `--app` can repeat; the Python and Node commands add it themselves.
* An app that is not installed is an error: ``import "app:orm-auth": not installed; install the `orm-auth` package``. There is no version lock.
* The rules of an imported schema stay the same: no `datasource`, no extension imports, one migration history, the prefix on table names.
* An app schema can import another app. The second app is found from the first app's own package.
  Each file loads once, so an app that two schemas both import is a repeated import and an error (as for relative imports).
* The module lookup of `@@query_set` and `@@model_methods` starts from the resolved schema file (see [Query set and model methods](#query-set-and-instance-methods-query_set-model_methods)).

Python generation writes `models.py` and `models.pyi` beside each schema. Keep
schema modules in separate directories. Use a shared Python package tree, or put
the main schema in an importable package when importing an external library. Each
public module exports its own models, enums and generated typing helpers. A private
`_orm_models.py` / `.pyi` beside the main output holds the complete compiled schema
and shared classes, allowing either public module to be imported first:

```python
from myproject.models import User
from myproject.billing.models import Invoice
```

`-o` relocates the main module and its private shared module; imported modules stay
beside their schema files. Generate from the main schema so every module receives
the same database settings and prefixes. TypeScript generation currently writes
one combined module from the same compiled schema.

#### Apps

An app is a schema import plus a language package.
The schema holds the models.
The package holds the code that works with them (model methods, query sets, helper functions).
The package declares where its schema is, so `import "app:<name>"` needs no path.
An app has no native code.
The Python package and the npm package of an app each hold a copy of the schema file.
Keep the copies equal. The tests of `orm-auth` check this.
An app that uses `@@inherit.abstract` needs a build with the `orm.inherit` extension.

The first app is [`orm-auth`](../apps/auth.md): users, roles, permissions and passwords.

### Fields

`name Type[?] @attr...`, where `?` makes the column nullable.

| Type | SQL | Python value |
|---|---|---|
| `BigInt`, `Int` | `bigint`, `integer` | `int` |
| `Float` | `double precision` | `float` |
| `Boolean` | `boolean` | `bool` |
| `String`, `String @db.VarChar(n)` | `varchar`, `varchar(n)` | `str` |
| `String @db.Text` | `text` | `str` |
| `String @db.Uuid` | `uuid` | `uuid.UUID` (strings accepted) |
| `DateTime`, `DateTime @db.Date` | `timestamp with time zone`, `date` | `datetime` (aware), `date` |
| `Json` | `jsonb` | dicts, lists, scalars |
| `Decimal`, `Decimal @db.Decimal(p, s)` | `numeric`, `numeric(p, s)` | `decimal.Decimal` (exact; `int`, `float`, `"12.50"` accepted) |
| an enum (below) | a Postgres enum type, `text` or `integer` | members of a generated `StrEnum` / `IntEnum` |
| `Type[]`, e.g. `String[]`, `Int[]`, `Role[]` | `text[]`, `integer[]`, `"role"[]`, ... | `list` of the element type |
| `String @db.Citext` | `citext` (the extension type) | `str` |
| `Unsupported("vector(384)")`, `Unsupported("geography(Point, 4326)")` | an extension type, from its extension file | from the file's `value` |

Prisma's other `@db.*` types (`@db.SmallInt`, `@db.Timestamp(3)`, `@db.Char(2)`,
`@db.Inet`, ...) set the column's SQL type; values convert as the field type's.

On Postgres, `char(n)` pads its values with spaces. Equality compares as `bpchar`, which ignores the padding.
`like`, `ilike`, `startswith`, `endswith`, `contains` (and their `i`-variants and `~` forms) compare `CAST(col AS text)`,
so `endswith("b")` finds `'ab'` and `contains(" ")` does not match the padding.
A pattern that contains the padding does not match it.
A prefix search on a `char(n)` column uses an index only with an expression index: `CREATE INDEX ON t ((col::text) text_pattern_ops)`.
SQLite does not pad.

Decimals travel in Postgres' binary `numeric` format, digit for digit, so money never
goes through a float: `SUM` of a decimal column is a `Decimal`, and so is `AVG` (other
averages are floats).

Arrays (`Type[]`) work for every scalar type and enum, but not for extension types or
primary keys. `@default([])` / `@default(["a", "b"])` give list defaults (`[member]`
for enum arrays). `String[]` is `text[]`; `String[] @db.VarChar(20)` is
`varchar(20)[]`, and values are cast to it. `Type[]?` makes the array itself nullable;
elements can always be `NULL` (`None`).

### Enums

```prisma
enum Role {            // a Postgres enum type: CREATE TYPE "role" AS ENUM (...)
  member
  editor
  admin  @map("ADMIN") // stored as 'ADMIN'
  @@map("user_role")   // the type's name (default: the enum name in lower case)
}

enum Color {           // a text column with CHECK ("color" IN ('red', 'green'))
  red
  green
  @@storage(text)
}

enum Priority {        // an integer column with CHECK ("priority" IN (1, 2, 3))
  low    @value(1)
  normal @value(2)
  high   @value(3)
  @@storage(int)
}

model Tag {
  id       BigInt   @id
  role     Role     @default(member)
  priority Priority @default(normal)
  colors   Color[]  @default([red])
}
```

`@@storage` picks how values are stored: `native` (the default, Prisma's behaviour)
uses the database's enum type where the dialect has one; `text` and `int` work on any
database and are kept to the enum's values by a `CHECK` constraint named
`<table>_<column>_enum_check`. Integer enums give each value with `@value(n)` (there is
no implicit numbering, so reordering the schema never changes stored data).

In Python each enum is a class of the generated module: a `StrEnum` whose values are the
stored labels, or an `IntEnum`. Columns read back as members (`profile.role is
Role.member`); writes and filters take members or the stored values (`"admin"`, `3`).

Migrations create the type before the tables using it and drop it after them. Values
added to a native enum become `ALTER TYPE ... ADD VALUE ... [BEFORE ...]`, which keeps
their place; Postgres can't use a new value in the transaction that added it, so the
step carries a warning. A removed or reordered value recreates the type: the old one is
renamed, the new one created, every column using it converted through text (defaults
dropped and restored), and the old type dropped. That fails if rows still hold a removed
value, and says so in a warning.

| Attribute | Meaning |
|---|---|
| `@id` | primary key |
| `@default(autoincrement())` | `GENERATED BY DEFAULT AS IDENTITY` |
| `@unique` | single-column unique constraint |
| `@default(0)` `@default("x")` `@default(true)` | literal default |
| `@default("{\"a\": 1}")` on `Json` | JSON default, written as JSON text |
| `@default(now())` | `now()` |
| `@default(dbgenerated("gen_random_uuid()"))` | any SQL default |
| `@client_default(...)` | ORM insert default (below); not in the DDL |
| `@map("db_name")` | column name, if different from the field name |
| `@db.*` | the SQL type (above) |
| `@check("views >= 0")` | column check constraint |
| `@comment("...")` | column comment |
| `@renamed_from("old")` | previous column name: the migration renames it instead of dropping |

### Nested types

A `type` block declares a value with named subfields.
A model field of that type holds the value.

```prisma
type Geo {
  lat Float
  lng Float?
}

type Address {
  city   String
  floor  Int @default(7)
  opened DateTime
  geo    Geo?
}

model Shop {
  id       Int      @id
  address  Address?
  snapshot Address  @storage(json)
}
```

* A type holds scalars, enums, other types and lists.
  A type cannot contain itself, and it holds no `@id`, no relation and no `autoincrement()` default.
* `@storage(columns)` is the default: one column per leaf, named `<field>_<subfield>`.
  `@storage(json)` stores the field as one JSON value.
  `@@storage(...)` on a type sets the default for every field of that type, and `@storage(...)` on a field overrides it.
* A nullable field in `columns` storage gets a database `CHECK`, so a half-set value never reaches the table.
  The type needs at least one required subfield for that rule.
* A JSON field refuses a dynamic default (`@default(now())`, `@client_default(...)`), `@check`, `@unique` and `@index` on a subfield of its type.
  Use `@storage(columns)` for such a type.
* `makemigrations` handles a change of a nested type:
  * A new required subfield of a `columns` type adds a column. Under a nullable field it adds the column without `DEFAULT`, fills the `@default` in the rows where the field is set, and then extends the `CHECK`.
  * A new required subfield of a `json` type runs one `UPDATE` that sets the `@default` where the key is missing or holds a JSON `null`, and its parent object exists. A subfield that changes from nullable to required runs the same `UPDATE`. Without a `@default`, `makemigrations` fails: add one, or make the subfield nullable.
    A new required subfield inside a list of a type fails too: update those rows by hand.
  * The snapshot keeps the shape of each `json` field for this. A snapshot from an older version has none: then every required subfield with a `@default` gets the same guarded `UPDATE`, which changes no row that already has the key.
  * `down` does not remove the key from the rows.
  * `@renamed_from("old")` on a model field of a type, or on a subfield, renames the columns of a `columns` field. On a model field with `@storage(json)` it renames the one column. A subfield of a `json` type takes no `@renamed_from`: a migration cannot rename a key yet, so the schema is refused.
  * The snapshot keeps the `@default` fills of a `columns` type, so `down` fills a required subfield that it adds back.
* `@@index([address.city])` and `@@unique([address.street, address.city])` name a subfield by its dotted path.

The language API is in [python-api.md](../api/python-api.md#nested-types) and [typescript-api.md](../api/typescript-api.md#nested-types).

### Client defaults

`@default(...)` is the database default.
It goes into the DDL and the migrations, and it applies to every writer.
`@client_default(...)` is the ORM default.
It fills an omitted insert value before native transforms and validators, in Python and TypeScript.
It is not in the DDL or in a migration snapshot.

```
id      String   @id @client_default(uuid7()) @db.Uuid
token   String   @client_default(uuid())
status  Status   @default(OLD) @client_default(ACTIVE)
created DateTime @default(now())
```

* `@client_default` takes a literal, `uuid()`, `uuid7()` or `now()`.
  A literal has the form of a `@default` literal: an enum member name, a list, or JSON text on a `Json` field.
  `"null"` on a `Json` field is the JSON value null, like `@default("null")`.
  A `Decimal` takes a number or text (`1.5` or `"1.5"`) and keeps every digit, and a `DateTime` takes an RFC 3339 timestamp with an offset.
  An enum field takes the member name, not a string.
* A field takes one client default; `@default(uuid())` counts as one.
* `uuid()` and `uuid7()` fit a `String` field, also with `@db.Uuid`.
  `now()` fits a `DateTime` field, also with `@db.Date` (the UTC date).
* `autoincrement()` and `dbgenerated(...)` fail, because only the database can make them.
* A field can have both defaults.
  The client default fills ORM inserts, and the database default covers other writers.
* An explicit value, also `NULL`, wins over both.
* `insert`, bulk insert and upsert make a new `uuid()`, `uuid7()` or `now()` value for each row.
* A field with either default is optional in the generated insert types.

### Relations

```
author   User      @relation(fields: [author_id], references: [id])  // to-one: holds the key
author   User?     @relation(fields: [author_id], references: [id], onDelete: SetNull)
posts    Post[]                                                         // to-many: the other side
```

To-one relations name their key field in `fields:` and the target field in
`references:`, and create the foreign key. Their options are `onDelete:` / `onUpdate:`
(`Cascade`, `SetNull`, `SetDefault`, `Restrict`, `NoAction`; the default is the
database's, no action), `deferrable: immediate | deferred`, and `map: "name"`, the
foreign key's name (default `<table>_<column>_fkey`). The relation is
optional (`User?`) exactly when the key field is nullable; the compiler checks this.
A to-many relation (`Post[]`) is paired with the to-one relation on the other model.
When two models have more than one relation between them, name both sides:
`@relation("author", fields: ...)` and `@relation("author")`.
A composite foreign key names several fields:
`item Item @relation(fields: [shop, item_id], references: [shop, id])`.
Its `references:` are the target's primary key or a unique constraint, in that order.
Its default name is `<table>_<columns>_fkey`, such as `lines_shop_item_id_fkey`.
A generic relation can't point at a model with a composite key.

```
profile  Profile?                                   // one-to-one: Profile.user_id is @unique
tags     Tag[]    @relation(through: PostTag)       // many-to-many through a join model
followers User[]  @relation(through: Follow, through_fields: [followee, follower])
```

* **One-to-one** is a to-one relation (`user User @relation(fields: [user_id], ...)`)
  whose key is unique (`@unique`, `@id` or a one-field `@@unique`), and its other side
  without `fields:`, which must be optional (`Profile?`): a user may have no profile.
* **Many-to-many** goes through an explicit join model with a to-one relation to each
  side (`PostTag.post`, `PostTag.tag`). `through:` names it; the relations are found by
  their targets, or named with `through_fields: [<to this model>, <to the target>]`
  (needed when both point at the same model, as in followers). The join model is an
  ordinary model: give it a `@@unique([post_id, tag_id])` and any extra columns, and
  query it directly too. Filters, aggregates and `load()` go through it in one
  hop (see [`python-api.md`](../api/python-api.md)). Prisma's implicit many-to-many (no join
  model) isn't supported.

### Model attributes (`@@`)

```
@@id([shop, id], name: "items_pkey")
@@index([author_id])
@@index([author_id, created_at(sort: Desc, nulls: last)], where: raw("published"), name: "x")
@@index([author_id, -created_at(nulls: last)])   // -field is sort: Desc
@@index([title(ops: raw("gin_trgm_ops"))], type: Gin)
@@index([sql("lower(email)", collate: "C")], unique: true)
@@index([floor], include: [name], nulls_not_distinct: true)
@@index([embedding(ops: vector_cosine_ops)], type: hnsw, with: { m: 16, ef_construction: 64 })

@@unique([author_id, slug], nulls_not_distinct: true, deferrable: deferred)
@@check("starts_at < ends_at", name: "bookings_order")
@@exclude([room_id(op: "="), sql("tstzrange(starts_at, ends_at)", op: "&&")], where: "not cancelled")

@@trigger(touch, before: [update], update_of: [title], when: "OLD.title <> NEW.title", body: "BEGIN NEW.updated_at := now(); RETURN NEW; END;")
@@trigger(audit, after: [insert, update, delete], for_each: statement, function: audit_row, args: ["posts"])
```

* `@@id([a, b])` is a composite primary key, in that column order.
  A model has `@id` or `@@id`, never both.
  A field in `@@id` can take `@default(autoincrement())`.
  `name:` or `map:` names the constraint; the default is `<table>_pkey`.
  A changed key migrates as a drop and an add of the constraint:
  the foreign keys that reference it drop first and come back after it.
* Index keys are field names or `sql("expression")`, with options `sort: Asc|Desc`,
  `nulls: first|last`, `ops: <operator class>` (a name or `raw("...")`),
  `collate: "..."`, and `op: "..."` in `@@exclude`.
* `-field` is the short form of `field(sort: Desc)`, and gives the same migration.
  An expression key keeps `sort: Desc`.
  `@@unique` takes no order, because a unique constraint has none:
  use `@@index([a, -b], unique: true)`.
* `@@index([field])` with one plain key is that column's index.
* `type:` is the access method (default btree for indexes, gist for exclusions):
  Prisma's `BTree`, `Hash`, `Gist`, `Gin`, `SpGist`, `Brin`, or any other method
  (`hnsw`, `ivfflat`, `bloom`). `with:` sets storage parameters. `where:` takes
  `raw("...")` or a string. The database name is `name:` or `map:`.
* For a unique *expression* or a partial unique rule, use `@@index(..., unique: true)`.
* `@@exclude` with `=` on a plain column under GiST pulls in `btree_gist` by itself.
* A trigger fires on exactly one of `before:` / `after:` / `instead_of:`. Give it
  either a one-line `body:` (a function `<table>_<name>()` is generated and replaced in
  place when the body changes) or a `function:` declared in a `function` block.

Mistakes are reported with file, line and column:

```
schema.prisma:22:14: Post.title: unknown type Strin; expected a model or BigInt, Int, Float, Boolean, String, ...
schema.prisma:30:24: Post: @@index: unknown argument `wher`
schema.prisma:44:14: relation Comment.author: author_id is not nullable, so the relation type is `User`
schema.prisma:34:3: model Post: an attribute must be on one line (put a long trigger body in a `function` block)
```

### Query set and model methods (`@@query_set`, `@@model_methods`)

```prisma
model User {
  id       Int    @id @default(autoincrement())
  password String
  @@query_set("UserQueries")
  @@model_methods("UserMethods")
}
```

* `@@query_set("UserQueries")` names a query set class. Codegen finds its module `queries`, as `Post.objects` uses it.
* `@@model_methods("UserMethods")` names a class whose methods go on each instance of the model, for example `user.check_password(raw)`. Codegen finds its module `methods`.
* Each takes one string, the class name. It is the same in Python and TypeScript.
* Module lookup. The schema file is in the same package as its code. For `import "app:orm-auth"` the schema file is the resolved one in the installed package, so the lookup finds `orm_auth` and `orm-auth`, not your project.
  * Python: codegen finds the import root above the schema file.
    The import root is the nearest directory that has a `pyproject.toml`, `setup.py` or `setup.cfg` (or `src/` under it, when the schema is there), or a `site-packages` / `dist-packages` directory.
    The module is the dotted path from the import root to the schema directory, then `queries` or `methods`: `orm_auth.queries:UserQueries` and `orm_auth.methods:UserMethods`.
    This also works for a namespace package (`acme.auth.queries:UserQueries` for `src/acme/auth/`) and for a directory without an `__init__.py`.
    A schema directly in the import root gives `queries:UserQueries`.
    With no import root, codegen fails and tells you to give the full path with `--query-set` / `--model-methods`.
  * TypeScript, an installed package (the schema path has a `node_modules` directory): codegen reads `name` from the nearest `package.json`. It writes `orm-auth/queries.js#UserQueries` and `orm-auth/methods.js#UserMethods`.
    The `package.json` of the package must list `"./schema.prisma"`, `"./queries.js"` and `"./methods.js"` in `exports`.
  * TypeScript, your own project (the nearest `package.json` of the schema also holds the generated file), or no `package.json`: the import is relative to the generated models file (`./queries.js#UserQueries`).
    `generateTypescript(schema)` of the tooling addon assumes `models.ts` is next to the schema file.
* A consumer replaces the schema class: `--query-set User=...` and `--model-methods User=...` (or `[tool.orm.query_sets]` / `[tool.orm.model_methods]`, `"orm": {"querySets"}` / `{"modelMethods"}`).
  The order is flag, then config, then the attribute.
* The attributes create no DDL. Migrations ignore them. The model IR keeps both names.
* A method name that is also a model member (a field, a relation, a member of the model or an extension member) is an error when the model is prepared, in both languages: `User.delete: model method collision`.
  The display members `__str__` and `__repr__` (Python) and `toString` (TypeScript) are not collisions: a methods class may override them.
* A proxy model takes the `@@model_methods` of its root model, as it takes the `@@query_set`. Name the class on the proxy to replace it.
* An inherited attribute resolves in the package of the schema that declared it (the IR field `classes_from`).

### Protected writes (`@@protected_write`)

`@@protected_write` is an application-level check in the ORM. It does not protect the database.
Raw SQL (`db.execute`), migrations, other ORM processes without this schema, and other database clients can still write.
For database-level protection, use `@@trigger` or database grants.

```prisma
model Post {
  id    Int    @id
  title String
  @@protected_write
}
```

* All ORM writes to a protected model fail outside an `allow_writes` scope (`allowWrites` in TypeScript) with `WriteProtected`:
  inserts, bulk inserts, updates, `update_many`, deletes, upserts, instance writes and multi-table writes.
* The check is on the table that the SQL writes.
  `post.tags.add()` writes `PostTag` rows, so `PostTag` must be protected for it to fail.
  A proxy writes the table of its root, so the protection of the root applies to the proxy.
  A multi-table write also writes the tables of its storage ancestors.
* A scope allows the tables of the models it names. Scopes nest: an inner scope adds its models to the outer ones.
* Reads are not changed.
* `@@protected_write` creates no trigger, no grant and no DDL. Migrations ignore it.

A database-level option is a trigger that rejects writes (PostgreSQL).
It rejects every writer, also the service that should write; exempt that service with a database role or grants, not with this trigger:

```prisma
model Post {
  id    Int    @id
  title String
  @@trigger(read_only, before: [insert, update, delete], body: "BEGIN RAISE EXCEPTION 'posts are read-only'; END;")
}
```

### Names

Generated names follow what Postgres itself would choose: `posts_pkey`,
`users_email_key`, `posts_author_id_fkey`, `posts_author_id_created_at_idx`,
`posts_views_check`, `bookings_room_id_expr…_excl`. Expressions contribute a stable hash.
Names over 63 bytes are cut and get a hash suffix. Indexes and constraints share one
namespace. Two objects that would get the same name (a btree and a trigram index on
one column, say) are an error, so give one an explicit `name:`.

## Extensions

An extension is a TOML file saying what it adds to the schema language:

```toml
# crates/core/extensions/postgres/vector.toml (excerpt)
name = "vector"                                   # the CREATE EXTENSION name
index_methods = ["hnsw", "ivfflat"]
opclasses = ["vector_l2_ops", "vector_cosine_ops"]
functions = ["cosine_distance"]

[types.vector]                                    # a field type: Unsupported("vector(384)")
sql = "vector({dims})"                            # {arg} placeholders come from `args`
args = ["dims"]
# defaults = { dims = "3" }                       # optional default per argument
value = "json"                                    # how values travel: text, json, big_int, uuid, ...
read = "CAST(CAST({} AS text) AS jsonb)"          # optional: SQL around the column when read
write = "CAST(CAST({} AS text) AS vector({dims}))" # optional: SQL around each bound value
python = "list[float]"                            # optional type hints for generated code
typescript = "number[]"
```

Built in (`crates/core/extensions/postgres/`, compiled into the core):

| file | adds |
|---|---|
| `citext` | type `citext`, written `String @db.Citext` (str; values are cast, so comparisons, `in_()` and `ON CONFLICT` ignore case) |
| `pg_trgm` | opclasses `gin_trgm_ops`, `gist_trgm_ops`; `similarity()`, ... |
| `vector` (pgvector) | types `vector(dims)`, `halfvec(dims)` (`list[float]`); methods `hnsw`, `ivfflat`; `*_ops` opclasses |
| `postgis` | types `geometry(shape, srid)`, `geography(shape, srid)` (EWKT strings); GiST opclasses; `ST_*` functions |
| `btree_gist`, `btree_gin`, `hstore` | operator classes |
| `bloom` | method `bloom` |
| `pgcrypto`, `uuid-ossp`, `unaccent` | functions (`crypt()`, `uuid_generate_v4()`, ...) |

Anything an extension provides pulls it in: a column of its type, an index using its
method or operator class, or a call to one of its functions in a default, check or
predicate. The next migration then starts with `CREATE EXTENSION IF NOT EXISTS`, and
the down migration drops it last (with a warning). `extensions = [name(schema:, version:)]`
in the `datasource` only pins where it goes and which version. Your own extensions are
files next to the schema, loaded with `import "extensions/acme.toml"`; their types are used
as `Unsupported("money(12)")`. Unknown keys in a file are an error.

The planner applies `read` / `write` in `SELECT`, `RETURNING`, `INSERT`, `UPDATE` and
comparisons, so queries on extension-typed columns look like any other.

These catalog files add database types and functions only.
Behavior (new attributes, proxies, defaults) comes from build-time extensions, below.

### Build-time extension attributes

A namespaced attribute belongs to a build-time extension crate.
The extension package must be installed ([build-time-extensions.md](../contracts/build-time-extensions.md#installable-packages));
otherwise definition fails with `@<name>: not compiled into this artifact and no installed extension owns it; install its package or rebuild with its extension (namespace @@<prefix>.*, see docs/README.md)`.

| Attribute | Extension | Doc |
|---|---|---|
| `@@proxy.of(Model)`, `@@proxy.fields(include: [...] \| exclude: [...])` | orm-proxy | [proxy-models.md](../extensions/proxy-models.md) |
| `@@query.defaults(filter:, fields:, related:, parent:, order:)`, `@@query.filter`, `@@query.fields`, `@@query.related`, `@@query.order`, `@@query.parent`, `@query.selectOut` | query-defaults | [selection-and-defaults.md](selection-and-defaults.md) |
| `@@composition.model(parent:, parentRef:, childRef:)` | orm-composition | [model-composition.md](../extensions/model-composition.md) |
| `@storage.file(storage:)` | orm-file-storage (package) | [file-storage.md](../extensions/file-storage.md) |
| `x Generic? @generic.relation(targets: [...])`, `@@generic.relation(...)`, `@generic.reverse` | generic (package) | [generic-identities.md](generic-identities.md) |

`@client_default`, enum subsets in proxies (`Status(ACTIVE)`) and `@@protected_write` are core syntax.

### Installed extension libraries

An extension can also ship as a native library that the loader opens at run time, so the ORM needs no rebuild.
The package has an `orm-extension.toml` with `abi` and `library` and no `host_contract` (fields: [build-time-extensions.md](../contracts/build-time-extensions.md#installed-libraries)).
A schema selects a library by the namespace of an attribute it uses, or by `generator client { extensions = ["acme.outside"] }`.
An extension that the schema does not use is not loaded and adds no stamp.

* Python: an `orm.extensions` entry point named like the extension id; its value is the package that holds the manifest.
* Node and Bun: a direct dependency whose `package.json` has `"orm": {"extension": "<id>"}`; the manifest is in the package root.
  The lookup starts at the project of the schema file, or at the working directory when there is no file (`loads(source)`).
  Python reads the interpreter environment.
* The standalone `orm` binary does not look for packages: `orm generate typescript --extension acme.outside=path/to/orm-extension.toml --extension-module acme.outside=acme-outside`. Both flags can repeat; use the Python import name for Python generation. The Python and Node commands add them automatically.
* `load(path)` and `loads(source)` resolve extensions the same way and import each selected package's runtime module, as generated models do. Definition loads no package native library or pass; changed or missing runtime manifest hashes emit `OrmStaleModelsWarning` and advise running `orm generate` ([runtime contract](../contracts/build-time-extensions.md#installed-libraries)).
* The loader reads the id, versions and attribute names of every installed manifest, and reads the rest of a manifest only when the schema selects its package.
  A manifest that does not parse fails a schema only when that schema can need it: the schema selects it by id, or uses an attribute namespace that no other package owns.
* The loader opens each library file once for each process and keeps it by its canonical path. A change to the file on disk has no effect until the process restarts.
  The manifest must still agree with the `abi` that the library reports, on every load.
* An extension that is not installed is an error that names the id and `--extension`. An ABI major that differs, or a library minor above the engine minor, is an error that names both versions.

## Code generation

```bash
python -m orm generate                   # models.py + models.pyi next to the schema (or -o path)
npx orm generate                         # models.ts
orm generate python -o app/models.py     # the standalone binary: say which language
```

`models.py` embeds the compiled IR and builds the classes with `orm.define()`, so no
schema detail is restated in Python. `models.pyi` gives editors and type checkers the
typed classes, relation paths (`User.posts.created_at` is `ColumnRef[datetime]`),
insert / update `TypedDict`s and query sets (see [`python-api.md`](../api/python-api.md)).
`languages/python/tests/test_migrations.py` and `languages/js/test/migrations.test.ts` check that the committed blog modules are up to date.
Without generation, `orm.load("schema.prisma")` returns the same classes at runtime.
TypeScript: see [`typescript-api.md`](../api/typescript-api.md).

## Migrations

```bash
python -m orm makemigrations [name]          # migrations/NNNN_name/{up.sql,down.sql,snapshot.json}
python -m orm makemigrations --check         # exit 1 if the schema changed without a migration (CI)
python -m orm makemigrations --empty data    # an empty migration for hand-written SQL or a data step
python -m orm sqlmigrate 2 [--down]
python -m orm migrate [target]               # apply pending migrations
python -m orm rollback [--steps N | --to 0002_x | --to zero] [--force]
python -m orm showmigrations
python -m orm pull [-o schema.prisma] [--force]   # the schema file from a live database
python -m orm baseline                       # mark the first migration applied (after pull)
python -m orm fmt --native                   # convert supported SQL expressions offline
python -m orm drift                          # exit 1 if the database differs from the last snapshot
```

There is one command line, written in Rust (`crates/cli/`): `python -m orm`, `npx orm` and the
standalone `orm` binary (`cargo install --path cli`) all run it, through the Python
extension, the Node addon or on their own, so they take the same arguments, write the
same files and apply migrations the same way. A migration with a data step is the
exception: only the host of its language applies it (see "Data migrations"). The bindings only change the defaults:
the name in `--help`, the configuration file read first, and the language `generate`
writes (the binary needs `python` / `typescript`, or an `-o` path that tells).

Settings come from flags (`--schema`, `--dir`, `--url`), else `[tool.orm]` in
`pyproject.toml` or the `"orm"` key of `package.json` (`schema`, `migrations`), and the
URL from `ORM_DATABASE_URL`. Exit codes: 0 success, 1 failure (including
`makemigrations --check` finding changes and `drift` finding differences), 2 bad usage. This
repository's `pyproject.toml` points at the blog example; its first migration is
[`examples/blog/migrations/0001_initial`](../../examples/blog/migrations/0001_initial/up.sql).

From Python: `Migrations("migrations", "schema.prisma")` (or a `Registry`) has
`.plan()`, `.make(name)` and `.all()`. `Migrator(db, migrations)` has `.upgrade()`,
`.downgrade()`, `.status()`, `.baseline()` and `.drift()`; `await orm.migrations.pull(db)`
reads a live database.

### What the generator does

The generator diffs the schema against the **snapshot stored with the last migration**,
not a live database, so it works offline and gives the same files everywhere.

* **Tables and columns** are matched by name first, then by `@renamed_from` /
  `@@renamed_from`. A hint applies only when the current name is not in the snapshot
  and the hinted name is, on Postgres and on SQLite. Two hints that name the same old
  table, or the same old column of one table, are a schema error when the schema
  loads, not only when a migration is planned. The snapshot stores only the new name,
  so the hint is safe to remove after the migration is generated; a kept hint does
  nothing in later migrations. New tables are created
  in foreign-key order with their keys and constraints inline; a foreign-key cycle is
  closed with `ALTER TABLE ... ADD CONSTRAINT`. Changed columns are altered in place
  (`TYPE ... USING`, `SET / DROP NOT NULL`, `SET / DROP DEFAULT`, identity).
* **Indexes and constraints** are matched by name, then by definition. If only the name
  changed (a renamed table or column), the object is renamed. If its definition
  changed, it is dropped and recreated.
* **Triggers** are recreated when they change. A changed trigger body only replaces its
  function.
* **Order**: extensions, functions, then drops (triggers, foreign keys, indexes,
  constraints), renames, dropped tables, new tables, column changes, new constraints
  (keys before foreign keys), indexes, triggers, comments, unused functions, and
  unused extensions last.
* **Down** is the same diff in reverse.
* **Warnings** (CLI output and `-- WARNING:` lines) flag steps that can lose data or
  fail on existing rows: drops, type changes, `SET NOT NULL`, a `NOT NULL` column
  without a default, new unique / check / exclusion / foreign-key constraints,
  dropping an extension.

### Applying

Each migration runs in one transaction together with its `orm_migrations` row (name,
SHA-256 of `up.sql`, time), under an advisory lock. A failing migration leaves the
database at the previous one, and concurrent deploys don't apply one twice. The runner
stops if an applied `up.sql` changed, if migrations were applied out of order, if one
recorded in the database is missing from the directory, or if two migrations share a
number. `create_tables()` / `drop_tables()` remain as development helpers (idempotent
DDL of the whole schema; drop with `CASCADE`).

### Data migrations

A migration folder may hold a data step next to `up.sql`: `data.py` with
`async def run(db)`, or `data.ts` with `export async function run(db)`.

```python
# migrations/0002_slug/data.py
async def run(db):
    await db.execute("UPDATE author SET slug = lower(name)")
```

The step runs after `up.sql`, in the same transaction and under the same advisory lock,
before the migration's `orm_migrations` row. Queries on `db` inside it, and ORM queries
on that database, go into that transaction, so they see the new columns. A
`db.transaction()` block in the step is a savepoint, and `db.on_commit()` callbacks run
after the migration commits. An error rolls back the SQL, the data changes and the row,
and drops the callbacks. `makemigrations --empty name` gives a folder for a step without
schema changes.

The models are the current ones, not the models at the time of the migration. Model
queries in a data step also pick up today's query defaults and extensions. A later model
change can make an old step fail on a new database (a column that the migration did not
add yet), or silently read fewer rows (a later default filter or soft delete on an
existing column). There are no historical models. Use `db.execute` and `db.fetch` in a
data step; use model queries only in a step that you delete later. `data.py` and
`data.ts` are outside the checksum, so a step can be rewritten after it ran.

* Python applies `data.py`: `Migrator.upgrade()`, or `python -m orm migrate`, which then
  runs through `Migrator` instead of the Rust runner. It imports the data modules of the
  pending migrations before it connects, so the models they import are in the default
  registry. Without a schema file, it connects with no models.
* TypeScript applies `data.ts` the same way: `Migrator.upgrade()` or `npx orm migrate`.
  Node 22.18 and later strip the types of a `.ts` file; Bun runs it.
* Each tool refuses the other's file, and the standalone `orm` binary refuses both,
  before it applies anything.
* A data module is a single file: it cannot import a sibling file with a relative import.
  Put shared code in your package.
* Await every query in `run`. A query that runs after `run` returns fails, because the
  transaction is closed.
* `rollback` runs `down.sql` only; a data step has no down step. A later `migrate` runs
  the step again, so write steps that you can run two times.
* The checksum covers `up.sql` only, so a change to a data step after it ran is not
  detected.
* SQLite runs a migration with foreign keys off (`PRAGMA foreign_keys` has no effect in
  a transaction), so `ON DELETE CASCADE` does not fire in a data step. A second migrator
  waits 5 seconds for the database lock, then fails with `database is locked`.

### Pull, baseline and drift: a live database under migrations

`orm pull` reads the database the URL names and writes the schema file (`--schema`, or
`-o FILE`; `--force` overwrites one). Postgres is read from `pg_catalog` for the current
schema, SQLite from `sqlite_master` and its `PRAGMA`s. It reads tables, columns,
defaults, comments, primary keys, unique, check, exclusion and foreign-key constraints,
indexes, enum types, extensions, SQL functions and triggers. Database names are kept:
an index or constraint whose name differs from the generated one gets `name:` / `map:`,
and a foreign key gets `@relation(..., map: "name")`. Model names are the tables in
PascalCase with `@@map`, a relation field is its key column without `_id`, and the other
side is `<table>_set` (or `<table>` for a one-to-one).

Then `pull` checks its own result: it creates the pulled schema in a shadow and compares
it with the database. It prints `The schema reproduces the database.`, or the steps a
migration from the schema would still run. What the schema language cannot say is left
out, printed, and listed at the end of the file as `// gap:` lines:

* rules (`CREATE RULE`; a gap that changes behaviour: a database made from the schema
  runs the statement that a rule rewrote, for example it deletes rows that a soft-delete
  rule keeps), views, row-level security policies, domains, composite and
  range types, generated columns, column collations;
* tables without a primary key, columns of a type
  without a schema type (`tstzrange`, `interval`, ...);
* a `serial` column (a `nextval` default on a sequence that the column owns) is read as
  an identity column (`@default(autoincrement())`) with a gap note, so a baselined
  Django database has no drift for it; the sequence keeps its name and its value. A
  default on a shared or unowned sequence stays a default: the schema makes it an
  identity, and drift reports `drop default` and `make ... an identity column`. The
  SQL for that conversion reseeds the new identity above the largest existing key
  (`setval(pg_get_serial_sequence(...))`), so the next insert does not collide;
* a primary key not named `<table>_pkey`; `GENERATED ALWAYS` identities (read as
  `BY DEFAULT`); constraint triggers and triggers that call a function of another schema;
* row-level security, `UNLOGGED` tables, table storage options, `INCLUDE` columns of a
  unique constraint, triggers with `REFERENCING` transition tables, foreign keys to a
  table of another schema;
* on SQLite: triggers. A declared type becomes the type of its storage class
  (`INTEGER`, `REAL`, `TEXT`), except `BOOL...` (Boolean), `DATE...` and `TIME...`
  (DateTime), and `DECIMAL` / `NUMERIC` (Float). Drift reports the declared type as a
  difference.

If the shadow check cannot run (no right to create a schema, or too few locks for a
large schema), `pull` writes the file anyway and lists the failed check as a gap.

The pulled schema starts a new migrations directory. Its text is the database's
(`(views >= 0)`), so `makemigrations --check` against migrations that a schema file made
reports changes.

`orm baseline` writes the first migration from the schema when the directory has none.
It records that migration in `orm_migrations` and does not run it. The next
`makemigrations` then holds only new changes. Baseline fails if the database already has
an applied migration. It warns when the database differs from the migration, or when it
cannot compare them.

`baseline` sets `baseline = true` on that row of `orm_migrations`. `rollback` refuses to
revert a baselined row, names it, and reverts nothing; `--force` (`downgrade(force=True)`,
`downgrade({ force: true })`) reverts it, and its `down.sql` then drops the tables that
were in the database before, with their rows. The column is added to an older
`orm_migrations` table the first time a migration command runs. A binary from before this
change ignores the marker and still reverts a baselined row.

`orm drift` compares the database with the snapshot of the newest migration. It exits 1
when they differ and prints the steps that bring the database to the snapshot. A
pending migration is part of the newest snapshot, so it shows as drift until it is
applied. Rules, views and the other gaps above are listed as not compared.

Postgres prints expressions in its own form (`CHECK ((views >= 0))`,
`'x'::character varying`, `role = ANY (ARRAY[...])`), so the snapshot is never compared
as text. Drift and `pull` run the snapshot's DDL in a shadow and read the shadow back
the same way as the database. On Postgres the shadow is a schema `orm_shadow_<backend
id>` in a transaction that is rolled back; on SQLite it is an in-memory database. The
live schema is re-created in a second shadow too. An object that both shadows agree on
counts as equal, because Postgres does not always print a parsed expression back the
same way. So drift needs the right to create a schema, and the extensions that the
snapshot uses must be installed.

### Why not SeaORM's or Refinery's tooling

`sea-orm-migration` and Refinery are runners for hand-written migrations. Refinery
has no down migrations and doesn't use sqlx. SeaORM's entity schema sync only adds
missing tables and columns. None has triggers, exclusion constraints, extensions,
renames or snapshot diffs, and using them would tie the migration IR to one engine.
The runner is in Rust (`crates/engine/src/migrate.rs`), shared by every binding.

## Declarative partitions (PostgreSQL)

Declare a parent with `@@partition(list: [kind])`, `@@partition(range: [created_at])`,
or `@@partition(hash: [shop_id])`. Every parent primary key and unique rule must
include every partition key column. Put a rule that omits that key on each partition
for uniqueness within that partition.

```prisma
model Event {
  id Int @default(autoincrement())
  created_at DateTime
  @@id([id, created_at])
  @@partition(range: [created_at])
}

partition event_2026 {
of = Event
from = ["2026-01-01"]
to = ["2027-01-01"]
}

partition event_rest {
of = Event
default = true
}
```

PostgreSQL routes inserts and moves rows when their partition key changes.
SQLite rejects partition declarations.

A partitioned parent with `autoincrement()` works on PostgreSQL 16: the migration
creates it, and inserts through the parent model fill the key. A direct insert
into a partition table needs PostgreSQL 17 or newer, because PostgreSQL 16 does not
give a partition the identity of its parent. Writes through a proxy that owns a
partition are direct inserts (see the proxy models guide), so on PostgreSQL 16
they fail with `null value in column "id" ... violates not-null constraint`.
The ORM tests run on PostgreSQL 18.

A partition block's name is its table name. List bounds use `values = [A, B]`
(enum members or literals); range bounds use `from` and `to` lists, with
`MINVALUE` and `MAXVALUE` allowed; hash bounds use `modulus` and `remainder`.
A default partition uses `default = true` (list or range). Every partitioned
parent needs at least one partition. List values cannot overlap; PostgreSQL checks
range overlap. Uncovered enum members produce a warning unless a default exists.

Blocks accept `@@check`, `@@unique`, `@@index`, `@@exclude`, `@@trigger`,
`@@comment`, `@@renamed_from`, and `@@partition` for subpartitioning. A
subpartition's `of` names its partitioned parent block. Parent rules are inherited;
local rules apply only to the block's table. Removal detaches and drops the table
and warns about data loss; a changed bound detaches and reattaches it, validating
existing rows. A changed parent strategy or key rebuilds its table and descendants
and warns about data loss. `pull` reproduces keys, bounds and local objects;
inherited copies are excluded from drift comparisons.

A proxy can own a list partition with `@@partition.values([A, B])`; `@@map` names
its table. Its local checks and indexes belong to that table, and its permanent
key predicate survives `without_defaults()`. A single bound value is filled on
insert. See [proxy-models.md](../extensions/proxy-models.md#owning-a-list-partition-postgresql)
for the syntax and interaction with composite identity and default filters.

## Not done yet

* `CREATE INDEX CONCURRENTLY` / non-transactional migrations.
* Generated columns, views, domains, row-level security, multiple
  database schemas.
* Renaming a native enum value in place (`ALTER TYPE ... RENAME VALUE`): a renamed
  value is a removal plus an addition today.

For offline SQL-to-native adoption and its supported subset, see
[native expression conversion](native-conversion.md).
