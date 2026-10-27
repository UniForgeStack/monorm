# Build-time behavioral extensions

You select behavioral extensions before you build the Python or Node native library.
They are ordinary Rust dependencies; a crate install does not modify an existing native library.
Catalogs of database types, functions, and indexes stay separate:
`import "catalog.toml"` supplies database definitions, not executable Rust.

## Contracts and preparation

`orm-contracts` contains schema IR, dialects and versioned extension contracts.
It depends on neither bindings nor extension implementations.
Compiler machinery lives in `orm-extension-build`.
Native profile types live in `orm_contracts::native`.
Host contract 1 supports schema transformations, physical schema contributions, scalar string validation/transformation, borrowed string record validation, native string results, and static model methods.

Python `define(schema, registry=...)` and TypeScript `define(schema, registry)` prepare a complete candidate before publishing models.
Invalid declarations, missing exports, unsupported language/database combinations, stale native specializations and member collisions fail during definition.
Failed definitions preserve the previous registry.
Existing models retain their prepared snapshot.
Returned IR is owned by its caller; mutating it does not change prepared models.

Python class declarations use `Registry(dialect="sqlite")` when needed, followed by `registry.prepare()`.
Queries on an unprepared class registry fail.
A connection also prepares registered classes during connection setup.
Class preparation can normalize field and relation descriptors;
model renames/additions and removal of inherited descriptors require a fresh `define` batch.
Callable Python client field defaults run before native transformations and validation;
they are not SQL defaults and do not affect other database writers.

## Build configuration

Run `cargo run -p orm-extension-build -- path/to/build.json`.
Paths are relative to the configuration file.
`output` must be a new directory.
For example:

```json
{
  "host": "../orm",
  "output": "../compiled-orm",
  "dependencies": {
    "rules": {"package": "my-rules", "path": "../my-rules"}
  },
  "bindings": ["python", "node"],
  "specialization": {
    "schema": "schema.json",
    "models": [{
      "model": "User",
      "fields": [{"field": "username", "transforms": ["app.trim"], "validators": ["app.username"]}],
      "records": [{"dependencies": ["username"], "export": "app.user"}],
      "computed": [{"field": "display", "dependency": "username", "export": "app.display"}],
      "methods": [{"name": "validate_username", "export": "app.username"}]
    }]
  }
}
```

`offline: true` uses installed Cargo dependencies.
`prepare_only: true` emits the workspace and composition without building the bindings.
`inputs` lists extra configuration/resource files that must participate in rebuild detection.
Declare all selected behavioral extension crates in `dependencies`, including extensions required by another manifest.
Cargo resolves Rust package versions;
the frontend checks extension dependency constraints against that resolved selection.

No manifest capability turns on a host Cargo feature.
`file-storage` has no host code and only requires `async-field-adapter.v1`.
The model-composition extension (`orm.composition`, `extensions/model-composition`) has none either.
The column-role extensions (`orm.timestamps`, `orm.soft-delete`, `orm.locking`) have no capability and no host feature; see `schema-extensions.md`.
The inheritance extension (`orm.inherit`, `extensions/inherit`) also has none; see [inherit.md](../extensions/inherit.md).
The query-defaults extension (`orm.query-defaults`) has none either: its pass writes only the neutral `read_defaults` record.
These seven are installable packages, not selected crates: `orm-extension-build` rejects a library manifest (see [Installable packages](#installable-packages)).
`scripts/feature-check.sh` builds one artifact per feature extension and one with all of them, installs the seven packages in every environment, and runs every suite on each.

Binding backend and generator defaults (including `reference-loading`) are preserved when composition is added, so an extension artifact is never minimal.
For a minimal artifact, select the host features explicitly with `binding_features`, for example `{"python": ["sqlite"], "node": ["postgres"]}`.
Each supplied entry replaces that binding's defaults with the listed features plus required static composition;
an omitted entry preserves its original defaults.
Unknown features, `default`, and entries for unselected bindings fail before the native build.
Use a host with the optional-packaging contracts for backend/generator exclusion;
Cargo dependency feature unification remains authoritative.
Selection is recorded in the build configuration and emitted binding manifests, and participates in rebuild guards.

## Select feature extensions

The repository ships static feature extensions as crates that you put in `dependencies`.

| Feature | Dependency entry (`"alias": {...}`) | Doc |
|---|---|---|
| file-storage | `"file_storage": {"package": "orm-file-storage-extension", "path": "extensions/file-storage"}` | [file-storage.md](../extensions/file-storage.md) |

Paths are relative to the repository root; write them relative to your configuration file.
This configuration selects them and emits the workspace only (`prepare_only`):

```json
{
  "host": "/path/to/orm",
  "output": "/path/to/build-all/build",
  "prepare_only": true,
  "bindings": ["python", "node"],
  "dependencies": {
    "proxy": {"package": "orm-proxy", "path": "/path/to/orm/extensions/proxy"},
    "file_storage": {"package": "orm-file-storage-extension", "path": "/path/to/orm/extensions/file-storage"}
  }
}
```

Build and package it (from the repository root; `DIR` is the parent of `output`):

```bash
cargo run -q -p orm-extension-build -- "$DIR/config.json"
export ORM_CORE_COMPOSITION=$DIR/build/composition.rs ORM_ENGINE_COMPOSITION=$DIR/build/engine-composition.rs
export ORM_PYTHON_METHODS=$DIR/build/python-methods.rs ORM_NODE_METHODS=$DIR/build/node-methods.rs
(cd "$DIR/build" && cargo build -p orm-python -p orm-node)    # add --release for production
mkdir -p "$DIR/py/orm_native_custom"
cp languages/python/packaging/custom/orm_native_custom/__init__.py "$DIR/py/orm_native_custom/"
cp "$DIR/build/target/debug/lib_native.dylib" "$DIR/py/orm_native_custom/_native$(python -c 'import sysconfig; print(sysconfig.get_config_var("EXT_SUFFIX"))')"
cp "$DIR/build/target/debug/liborm_node.dylib" "$DIR/orm.node"     # .so on Linux
```

Use the artifact with `PYTHONPATH=$DIR/py ORM_PROFILE=custom` in Python and `ORM_NATIVE=$DIR/orm.node` (an absolute path) in Node.
Without `prepare_only`, the frontend builds the release libraries itself.
`scripts/feature-check.sh all` does the same steps and runs every suite on the result.
A schema that uses an extension attribute on an artifact without that extension fails during definition:
`@proxy.of: not compiled into this artifact and no installed extension owns it; install its package or rebuild with its extension (namespace @@proxy.*, see docs/README.md)`.
The message names the namespace of the attribute.

Protected writes (`@@protected_write`), client defaults, cursor pagination, the N+1 finder and reference loaders are in the core.
They need no extension.

The frontend type-checks the embedded TypeScript with `tsc` from `languages/js/node_modules`.
Run `npm install` in `languages/js` before the first build, or the frontend fails with `TS2688: Cannot find type definition file for 'node'`.
Python extensions also need `mypy` and `pyright` on `PATH`.

### The `ext` modules

`orm.ext` (Python) and `orm/ext` (TypeScript) are the host surface for extension code.
`database_of(instance)` and `databaseOf(instance)` give the database that loaded an instance, or none.

A bound query set mixin reads the objects of a bound write with these helpers (the Python name, then the TypeScript name):

* `bound_objects(bound)` gives the objects in input order. `boundGroups(bound)` gives them in groups, one group for each primary key.
* `bound_model(bound)` and `boundMeta(bound)` give the model.
* `bound_guarded(bound)` and `boundGuarded(bound)` are true for the set of an instance write.
  That set matches the loaded guard values already, so a mixin adds no check of its own.
* `bound_fresh(bound, objs, *columns)` and `boundFresh(bound, objs, ...columns)` read the current row of each object.
  They give `(object, row)` pairs and leave the objects as they are.
* `bound_atomic(bound)` and `boundAtomic(bound, fn)` run a block in a transaction, or in a savepoint inside one.
  When the block fails, the write rolls back and the objects get the state that they had before.
* `loadedValue(object, field)` (TypeScript only) gives the loaded value of a field and throws `NotLoaded` when it is not loaded.
* `key_condition(model, key)` (Python only) gives the condition of the row with the primary key `key`.

### The full profile

`languages/python/packaging/full/build.json` and `languages/js/packaging/full/build.json` are configurations of this kind.
They select the static first-party extension `file-storage`.
The timestamps, soft-delete, locking, inherit, query-defaults, generic, proxy and composition extensions are installable packages, not part of the profile.
They set `prepare_only` and `binding_features` to `["profile-full"]`.
`profile-full` has the features of `profile-tooling`: the backends, `cli`, both generators and `reference-loading`.
It also gives the profile its name in `profile_metadata()`.
`build-full-wheel.py` and `build-node-profile.mjs full` run the steps above.
The Python script then runs `maturin build --release` in the emitted workspace.
The Node script runs `cargo build -p orm-node` there.
[Static native profiles](optional-packaging.md#the-full-profile) has the list of the two extensions, the install lines and the rest of the contract.

## Application source modules

Instead of a dependency, `modules` accepts an application source module:
`{"alias":"rules","source":"rules.rs","manifest":{...},"dependencies":{...}}`.
The frontend wraps the source in a Cargo crate and supplies `orm-contracts`.
Its metadata and export contracts are identical to those of a packaged crate.

The frontend copies host sources into the output workspace, resolves dependencies and reads Cargo metadata before generating direct calls.
It emits `artifact.json`, `manifests.json`, `source-identities.json`, `normalized.schema.json` for selected profiles, and fixed Rust composition sources.
It builds ordinary `orm-python` and `orm-node` release libraries.
Package the resulting library with the matching Python/TypeScript package, replacing its native library.
Do not mix libraries from different profiles.
Existing package build/install tools handle the platform library suffix and Python extension suffix.

Rebuilding an emitted workspace requires its composition environment paths:
`ORM_CORE_COMPOSITION`, and for native behaviors `ORM_ENGINE_COMPOSITION`, `ORM_PYTHON_METHODS`, `ORM_NODE_METHODS`.
A generated build guard rejects changed recorded configuration/source inputs and asks for a fresh frontend output.
The guard reads `crates/core/composition-inputs.json` from its current workspace,
so revisiting an older profile through a shared Cargo cache retains the correct composition and stale-input checks.
Use the frontend for a changed selection rather than editing generated files.

## Extension metadata and source syntax

A crate declares `[package.metadata.orm-extension]`, either inline or as `manifest = "orm-extension.toml"` referencing a resource inside its package.
The version must match its Cargo package version.
Required fields include `id`, `version`, `host_contract` (not for a library), `schema_contract`, `languages` (`python`, `typescript`), and `databases` (`postgres`, `sqlite`).
Contract versions are strings, `"major.minor"`: the majors must be equal, and an extension minor must not be above the engine minor.
Optional lists describe host `capabilities`, semver extension `dependencies`, `attributes`, `passes`, and `exports`.
Unknown metadata fields are errors.

An attribute declares its `name`, `target` (`model` or `field`), and typed `arguments` (`string`, `integer`, `boolean`, `list`, `value`).
Only a `value` argument takes a bare name (as a string), `null` and objects; the other kinds take strict literals, also inside a list.
The extension pass checks the shape of a `value` argument.
Source declarations use namespaced attributes, for example `name String @app.rename(to: "public_name")` or `@@app.audit(level: 2)`.
Names, arguments and source locations are preserved in schema requirements.
Unknown namespaces request a rebuild; `@db.*` keeps its existing database meaning.

A pass declares `id`, `phase`, `rust`, `after`, and owned `effects` paths.
Phases run declaration, logical, storage, behavior, validation, generation.
Ordering is stable;
cycles, missing pass dependencies, duplicate IDs and overlapping effect ownership fail.
Its Rust function takes `&mut orm_contracts::ir::SchemaIr` and returns `Result<(), String>`.
Rust paths can use `crate::function`, the owning crate name, or its selected dependency alias;
composition rewrites the prefix.
Namespaced model declarations may derive their primary key during selected passes.
Ordinary models require `@id` before lowering; annotated models must have one after preparation.
Feature-derived relations should be contributed by the pass.

Completed passes and prepared model names (`lowered_models`) are recorded so a generated or serialized normalized schema is not lowered twice.
Additional definition batches lower their new declarations against the complete existing definition context.
Attribute-independent passes use `lowered_models` to distinguish previously prepared models from a new batch.

## Logical fields and physical storage

The compiler captures a physical schema before lowering namespaced declarations.
Logical renames/removals leave that storage snapshot intact.
Passes intending a physical change update `behavior.storage.models` explicitly.
Migration planning and snapshots use the physical schema, so a public field removal does not drop its column.
Logical fields must resolve to an existing physical column with a compatible encoding and primary key.
Prepared `ResolvedField` entries retain logical identity, storage owner, column identity and both value types.

A same-table proxy can contribute another logical model sharing its parent's physical owner without creating a second migration table.
Shared-primary-key storage composition contributes `behavior.field_storage` entries (`model`, `field`, `owner`, `column`) and `behavior.owner_links` (`child`, `parent`, `child_key`, `parent_key`).
Definition resolves these to numeric owner/column identities, requires an explicit `behavior.storage` snapshot, rejects cycles or incompatible keys/encodings, and requires an ordinary physical foreign key.
A field owner must be the local owner or an ancestor.
Inherited scalar reads, projections and filters use ordinary SQL through those prepared owners.
Inherited relation links, `outer()` and `distinct(on=...)` on inherited columns need an expanded host primitive.

Writes across owner tables belong to `orm_engine::multi_table`.
It is in every build, so any extension can write a two-table model with these records; `tests/outside-extension` does so.
It plans creates, attaches, filtered updates and deletes, and each runs in one transaction or savepoint.
`update_many` on a multi-table model is rejected.
Ordinary model writes reject inherited storage instead of writing nonexistent columns.
Code generation and both runtimes derive the child models, their own fields and the key default from `behavior.owner_links` and `behavior.field_storage`; a pass writes no hint for them.
The language APIs of these features are in [model-composition.md](../extensions/model-composition.md), [proxy-models.md](../extensions/proxy-models.md) and [selection-and-defaults.md](../schema/selection-and-defaults.md).

## Native behavior profiles

Exports declare stable `id`, `rust`, `kind`, `input`, and optionally `output`:

| Kind | Rust signature |
|---|---|
| `string_validator` | `fn(&str) -> Result<(), String>` |
| `string_transform` | `fn(&str) -> Result<String, String>` |
| `string_record_validator` | `fn(&[&str]) -> Result<(), String>` |
| `string_computed` | `fn(&str) -> Result<String, String>` |

Inputs are scalar String/Text fields.
Transforms run in declared order before validators.
Record validators inspect transformed values through borrowed inputs.
Bulk writes validate their supplied rows before executing SQL.
Expression writes, upsert conflict outcomes and omitted/defaulted validated insert values are rejected;
use database constraints or explicit transactions for those strategies.
Null is rejected unless a field rule explicitly sets `allow_null: true`, in which case its field functions are skipped.
Record dependencies require all non-null values whenever any dependency is updated; partial records are rejected.
Raw SQL bypasses ORM write validation.

A `string_computed` field is read-only and depends on one stored scalar string field.
Projection loads that dependency; model and returning shapes reuse its selected value.
Computation occurs inside the existing native call before materialization, without language callbacks or hidden queries.
NULL dependencies produce NULL.
Native-only computations cannot be used in SQL filters, ordering, grouping, expressions, conflict targets or CTE outputs.
A point-5 hook field is read-only too, and SQL cannot use it.
It depends on the fields that its binding lists (see [Read and expression hooks](#read-and-expression-hooks-points-5-to-7)).
Nested/cyclic computed dependencies require an expanded profile and currently fail at build time.
Relation-dependent computation is not supported by this primitive.

A proxy model takes the spec of its nearest ancestor and adds its own entries to it; an own entry replaces the inherited one of the same name (see [proxy-models.md](../extensions/proxy-models.md)).
Static model methods use scalar string exports with generated Python/TypeScript types and collision checks.
Names must be valid Python identifiers, excluding keywords and `__debug__`.
Generated write types omit computed fields and exclude expressions for hooked fields.
Generation and runtime definition both consume normalized schema requirements and the same compiled adapters.

Profiles fingerprint the normalized model, relevant enums/storage, native configuration, extension composition, resolved dependencies and local source content.
Runtime definitions can add unrelated models but must match compiled behavior shapes and versions.
A stale profile fails before connection or queries with a rebuild instruction.
There is no interpreter fallback.

### Contract fields for derived models and types

An extension pass can derive models and relations.
Four fields of the schema IR carry the result to the host.
Core uses them without knowledge of the extension that sets them.

- `RelationIr.label`: the `@relation("name")` text of a back relation that has no owning relation yet, in a model whose pass derives it (`from` and `to` are empty).
  The pass consumes it. A label that no pass consumes is an error after lowering.
- `TypeModel` (`name`, `fields`, `extended_by`): the fields of a model that has no table.
  Code generation writes a type for it and no runtime model.
  `extended_by` lists the models that extend it directly, in schema order.
- `behavior.type_models`: the `TypeModel` list of a schema.
  `merge_definition` merges the lists of two batches by name; two entries with one name and different content are an error.
- `behavior.model_overrides`: a map from a model to the model that took its table (`{"User": "MyUser"}`).
  Language code reads it with `behavior_value("model_overrides")` (`behaviorValue` in TypeScript).
  Two models that override one model are an error.

## Execution and performance

A core-only build has no composition dependencies or model behavior state.
Every build has the hook plan of a model; a model without a hook has no plan.
Enabled profiles generate direct Rust calls and numeric field positions.
A hook of a static profile is a function pointer; a hook of a packaged extension calls its library through the ABI.
Runtime execution parses no declaration; `define()` reads the manifest of a packaged extension once to open its library.
Dispatch for computation is resolved at the output-shape boundary rather than for each row.
The host retains binding, SQL execution and transaction ownership.

See `bench/build-time-extensions/` for paired baseline comparisons, allocation measurements and enabled-profile controls.
Warm framework cost must be below 1%;
definition, build duration and artifact size are reported separately.
Timing intervals that cross the limit remain inconclusive.

## Installed libraries

A manifest with `abi` (a `"major.minor"` string, currently `"0.2"`) and `library` (the file name; the loader adds the platform prefix and suffix) is a library.
It has no `host_contract`. The loader runs its passes; optional `[python]` and `[typescript]` sections name exports in the packaged runtime modules and retain language-contract validation. The loader resolves the hooks of the manifest with the library.
The crate is a `cdylib` built with `panic = "unwind"` (the default; the exports catch a panic and return a status) that ends with `orm_contracts::extension!`:

```rust
orm_contracts::extension! {
    init = my_init;                          // optional: fn(&serde_json::Value) -> Result<S, String>
    hooks { "my.hook" => my_hook };          // optional: fn(&S, &[Arg], &Context) -> Result<Value, String>
    ("my.pass", my_pass)                     // fn(&mut SchemaIr) -> Result<(), String>
}
```

The macro exports `orm_ext_abi_version`, `orm_ext_run_pass` and `orm_ext_free` (ABI 0.1), and `orm_ext_init`, `orm_ext_drop_state` and `orm_ext_hooks` (ABI 0.2).
`init` gets the JSON of the extension's `extension_data` entry and returns the state `S` (`()` without `init`); the engine calls `orm_ext_drop_state` when it drops the state.
`S` must be `Send + Sync + 'static`, so that a state can cross threads.
Each hook takes the `&S` of the same `init`; a hook with another state type is a compile error.
A hook has one signature for all seven points. `orm_contracts::abi::Value` has the tags null, bool, int, float, decimal, text, bytes, datetime (microseconds since the Unix epoch, UTC), uuid, json and date (days since 1970-01-01).
An `Err` becomes status ERROR with its text, and a panic becomes status PANIC.
`orm_ext_init(json, len, out_state, error)` writes `error` on every status, with the failure text, or an empty buffer on OK (a null `error` is allowed); the host frees it with `orm_ext_free` every time.
A manifest of a library lists its hooks as `[[exports]]` with `id`, `point` (1 to 7), `input` and `output` (a value type). A compiled extension keeps `rust`, `kind` and `input`.
When the manifest has `exports`, the loader needs `orm_ext_hooks` and, when `init` exists, `orm_ext_drop_state`, and the hook table needs a hook of the same name for each export id. A mismatch is an error when the library opens, and it names the export. A manifest without `exports` has no hooks to resolve, so a pass-only library needs none of the three symbols.
`Plan::library(id)` gives a `Library` of a manifest with `exports`; `Library::init(&json)` gives a `State`, and `State::call(hook, &args, &context)` runs one hook. The `State` calls `orm_ext_drop_state` once when it drops.
Schema contract 4.0 holds `Requirements.hooks` (a `HookBinding` per model, field, point and export) and `Requirements.extension_data` (JSON by extension id). A major bump resets the minor. The IR sets `schema_contract_minor` only for a feature that a later minor adds; an engine of an older minor stops at the unknown field with a parse error, and the "newer than this engine" message comes only from an engine that knows the minor.
Schema contract 4.0 holds `Requirements.model_views` and `Requirements.result_checks`, and the `view_source` key of an attribute.
A `ModelView` (`model`, `source`, `root`, `row_filter`, `insert_table`) makes `model` read and write the table of `root`; `source` is the nearest parent. `row_filter` is a `field` and a list of `values`. Each value must be an enum member of `field`, or `define()` fails. The core builds the literal SQL; an extension never gives SQL text. The filter applies to root reads, joins, and update and delete targets, and `without_defaults()` does not remove it.
A `ResultCheck` (`model`, `field`, `non_null`, `allowed`, `code`) makes the engine warn when a result row breaks the check. The warning carries `code`.
An attribute with `view_source = true` declares the source model of a view. The DSL lowering finds views through this key, not through an attribute name.
`define()` checks each binding: the model, the field and the dependency. When the manifest of the source is in the build, it also checks the point, and that the `input` and `output` types match the field type. When the build has no manifest of the source, the source must be in `Requirements.packages`; if it is in neither, the binding is an error that names the source.
A version fits when its major equals the engine major and its minor is at most the engine minor.
This holds for `schema_contract` and for `abi`, and for the `abi` that the library reports.
Another major, or a newer minor, gives an error that names both versions and says what to install, even when the manifest has fields that this release does not know.
The loader runs library passes and compiled passes in one pass order.
The IR records each library that ran in `Requirements.packages`: its version, its passes, its attribute names, supported `languages` and `databases`, `language_members`, `dependencies`, and `manifest_hash` (the lowercase hexadecimal SHA-256 of the raw `orm-extension.toml` bytes).
The generated models embed this lock in their compiled schema.
The hash excludes native library bytes, so the same models work across platforms.

Run `orm check` in CI to compare the lock against installed extension packages.
A changed version or manifest, or a missing package, fails with the locked and installed identities and a run `orm generate` hint.
Unused installed packages add no lock requirement.
Python and npm commands discover packages as they do for generation. The standalone binary takes repeated `--extension ID=MANIFEST` options and, for generation, `--extension-module ID=MODULE` for each selected package (a Python import name or npm package name). The binding commands supply both automatically.
Use `check -o FILE` (or `--out FILE`) for a custom models path; a missing explicit path fails with a regeneration hint.
All hosts check `models.py` and `models.ts` beside the schema when they carry a generated header, and skip other files.
Rust models are outside this lock check.
Python schema facades are checked through their shared `_orm_models.py`.
With no generated models file, the command compiles the schema as usual.
Generated models import every selected package's runtime module, including packages without runtime classes; `load()` and `loads()` discover and import the same modules. Definition merges their runtime members with the native artifact's static members; an extension id present in both is an error. Definition runs no pass. It opens the native library of a package only to resolve the hooks that the schema binds to it; code generation and migrations open no library.

Each runtime module exports `MANIFEST_HASH` (Python) or `manifestHash` (TypeScript), the lowercase SHA-256 of the raw manifest bytes. `define()` compares it with the embedded stamp once per extension per definition. A changed or missing constant warns with the id, both hashes and a run `orm generate` hint, then continues. Python uses `orm.OrmStaleModelsWarning`, a `UserWarning` subclass (subject to Python warning filters); Node and Bun emit `OrmStaleModelsWarning`. This warning does not replace the CI lock check above.

TypeScript runtime packages and their dependencies must be synchronously loadable, without top-level await. Node requires 20.19+ on the 20.x line or 22.12+ on later lines; Bun is also supported. Prefer one runtime format. Synchronous loading resolves the package's import entry before loading it, so a dual package uses the same module and class identities as generated ESM imports.
`tests/outside-extension/` is a library in its own workspace.
It also shows the three optional keys of a field attribute in `orm-extension.toml` (schema contract 2.2).
`field_type = "Money"` makes the attribute own that DSL type: a field of that type lowers to an `Int` placeholder, and the pass replaces it.
`member = false` makes the field no model member: lowering drops the field and keeps its declaration for the pass.
`identities = true` makes a schema that uses the attribute need model identities (`orm identities`).

### Read and expression hooks (points 5 to 7)

Point 5 gives a computed field.
The binding names the field and its `dependencies`, which are stored fields of any type.
The field is a result field of the model: a write cannot set it, and SQL cannot use it.
A read selects the dependencies only when it asks for the computed field, and the host calls the hook for each row.
A write that returns the row gives NULL for the computed field. Read the row again to get the value.

Points 6 and 7 give an expression, not a value.
The hook returns a `json` value that holds a query IR expression.
A point-6 hook serves a write assignment of `AssignValue::ExprHook`: an update assigns the expression to the field.
A point-7 hook serves a field with `ClientDefaultIr::Hook`: an insert plans the expression for each row that omits the value.
`AssignValue::Expr` and `ClientDefaultIr::Expr` store such an expression in the IR, with no hook.
The expression has no parameters, no outer query and no `EXCLUDED` row.
Only a point-6 expression or an `AssignValue::Expr` can name the columns of the row.
An assignment expression reads the row as it was before the update.
An update that sets `text` and has `slug = lower(text)` stores the lower case of the old `text`.

A hook of point 6 or 7 with `reads_context = false` runs once when the schema is generated.
The IR then stores its expression as `Expr`, and `define()` opens no library for it.
A hook that reads the context runs for each statement.

Schema contract 4.0 holds these records. An engine of an older major refuses an IR that has one of them or a hook of point 5, 6 or 7 with the "newer than this engine" message.

## Installable packages

`orm-extension-package package <crate dir> --out <dir>` builds one extension crate for the host platform.
The `library` and `assemble` commands build one package for 3 platforms; see [Extension packages for 3 platforms](optional-packaging.md#extension-packages-for-3-platforms).
The crate has `crate-type = ["cdylib", "rlib"]`, the ABI macro and an `orm-extension.toml` with `abi` and `library`.
The tool writes a `py3-none-<platform>` Python wheel and an npm package directory.
Each package holds the library, the manifest and the runtime module with `MANIFEST_HASH` (`manifestHash`), the SHA-256 of the manifest bytes.
`scripts/install-extension-packages.sh` builds the timestamps, soft-delete, locking, inherit, query-defaults and generic packages and installs them into `.venv` and `languages/js`.
