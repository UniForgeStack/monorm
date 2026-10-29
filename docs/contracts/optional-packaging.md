# Packaging: one package per registry

Each registry holds one package, `monorm`.
It has the engine, the nine built-in extensions, the command line and the auth app.
There are no native profiles, no extras and no per-extension packages.

| Registry | Package | What it holds |
| --- | --- | --- |
| PyPI | `monorm` | 12 platform wheels (3 platforms × CPython 3.11 to 3.14). No sdist. |
| npm | `monorm` | One tarball with the addon and the extension libraries of 3 platforms. |
| crates.io | `monorm` | The contracts library for authors of outside extensions. No binary. |

The 3 platforms are `darwin-arm64`, `linux-x64` (glibc 2.28 or newer) and `win32-x64`.
Another platform needs a source build.
The engine has both PostgreSQL and SQLite, the migration and schema APIs, and both code generators.
The wheel and the tarball hold the command line: `python -m monorm`, `npx monorm` and the `monorm` command.
The crate `monorm` (the directory `crates/contracts`) is not on crates.io yet. An outside extension takes it as a git or path dependency until then.

```bash
pip install monorm
npm install monorm
```

## The built-in extensions

The built-in extensions are `monorm.timestamps`, `monorm.soft-delete`, `monorm.locking`, `monorm.inherit`, `monorm.query-defaults`, `monorm.generic`, `monorm.proxy`, `monorm.composition` and `monorm.file-storage`.
Each one has a native library, a manifest and a runtime module.
A library loads only when the schema uses its extension, so a schema with no extension attribute opens no library.

- Python: `stage.py` writes the nine libraries of the platform into the package, and `[project.entry-points."monorm.extensions"]` in the root `pyproject.toml` lists them. The owner distribution `monorm` counts as built-in.
- npm: `npm run assemble` in `languages/js` writes `extensions/<name>/` and `prebuilt/<platform>/`. The package exports `monorm/extensions/<name>`. Node finds the built-in extensions from `bundledExtensions()`.
- Auth: Python has the module `monorm_auth` and the app name `monorm-auth`. Node has `monorm/auth` and the app name `auth`. The two names differ, so one schema file does not serve both languages.

## Outside extensions

An outside extension is a Python wheel, an npm package, or both, built with `monorm-extension-package` (see [Extension packages for 3 platforms](#extension-packages-for-3-platforms)).
It installs next to `monorm`:

- Python finds it through the `monorm.extensions` entry point.
- Node finds it when the package is a direct dependency of the project `package.json` and has `"monorm": {"extension": "<id>"}`.

An outside package that declares the id of a built-in extension stops the load with the error "installed twice".
The error names the package to uninstall.

## Upgrade from the old packages

Before `v0.1.0` the project had a thin `monorm` package, a native package for each profile and one package for each extension.
Uninstall them and install `monorm` (the commands are in the [README](../../README.md#upgrade-from-the-split-packages)).
The `MONORM_PROFILE` selector, the `monorm[postgres]`, `monorm[sqlite]`, `monorm[combined]`, `monorm[tooling]` and `monorm[full]` extras, and the `@ufstack/*` packages no longer exist.

## Exact source builds

Cargo features are additive.
The default features select both backends and all tooling.
Engine backend features are `postgres` and `sqlite`.
Bindings forward selected backends without enabling dependency defaults.
`cli` selects the CLI crate.
`generate-python` and `generate-typescript` select generator modules independently, including through an optional CLI dependency.
`reference-loading` selects the to-one cache adapter ([reference-loading.md](reference-loading.md)).
The released wheel and tarball use the default features.
The `profile-*` features of the bindings stay for audits of a narrower build (`languages/python/packaging/check-dependencies.py`); they are not published.

Every artifact embeds its build record in `profile_metadata()["build"]`: the exact Cargo features, the `rustc -V` toolchain, target triple, Cargo profile and git revision (`git rev-parse HEAD` of the host source, or `MONORM_BUILD_REVISION` when set; `unknown` outside git).

A source checkout builds like this:

```bash
python languages/python/packaging/stage.py && maturin develop        # Python: the extension libraries, then the wheel
cd languages/js && npm install && npm run build:native && npm run assemble   # Node: the addon, then the extension files
```

`MONORM_NATIVE=<file>` makes Node load another addon file.
`MONORM_CARGO_TARGET=<triple>[.<glibc>]` builds with `cargo zigbuild` for that target (the Linux release uses `x86_64-unknown-linux-gnu.2.28`).
`stage.py --libraries <dir>` stages libraries that another job built, and `assemble-bundle.mjs --collect <dir>` gathers the libraries of 3 platforms for the tarball.
`MONORM_PACK_PLATFORMS=all` makes a missing platform an error when you pack.

## Immutable adapter contract

Native `profile_metadata()` / `profileMetadata()` returns JSON ABI 1 with `version`, `language`, `profile`, `backends`, boolean `capabilities`, selected `adapters`, and the `build` record (`features`, `rustc`, `target`, `profile`, `revision`).
The loaders reject an artifact without a well-formed build record.
This metadata is separate from schema and extension manifests.
An adapter name must have a true capability with the same canonical name.
Python `monorm._capabilities.ADAPTERS` is a frozen set initialized at module import; TypeScript `nativeAdapters()` returns a cached frozen array.
Selected adapter modules and specialized model methods are bound at module or definition initialization.
Disabled paths retain ordinary decoding and writes; they do not import providers, copy rows, scan fields or test flags on every operation.
There is no query-time plugin discovery.

## Verification

`scripts/check-extension-packages.sh` is the main seam.
It builds the one wheel and the one tarball of the checkout, packages the outside extension `tests/outside-extension`, and installs all of them in fresh Python, Node and Bun consumers, on PostgreSQL and SQLite.
`scripts/check-assembled-consumers.py` runs the cases:

- one attribute of each built-in extension, generate and one query, and a file field;
- the auth app in both languages;
- a schema with no extension attribute, with the extension libraries deleted;
- the outside extension next to `monorm`;
- an outside package with a built-in id, which must give the "installed twice" error.

`languages/python/packaging/check-wheel.py` checks the wheel content, `smoke-python.py` and `smoke-node.mjs` are the smoke scripts, and `languages/js/packaging/check-node-install.mjs` packs and installs the tarball offline.
`scripts/check-glibc.py` fails when a Linux library needs a symbol version above glibc 2.28.
Set `ORM_TEST_DATABASE_URL` to an isolated PostgreSQL database for the PostgreSQL checks; SQLite checks use in-memory databases.
`.github/workflows/package-builds.yml` builds the wheels and the tarball on each platform, and `extension-packages.yml` runs the seam on Linux.
Both workflows publish nothing; `release.yml` publishes (see [RELEASING.md](../../RELEASING.md)).
CI records the size of every wheel and of the tarball in the job summary.
There is no size limit.

## Extension packages for 3 platforms

An outside extension is one wheel and one npm package.
Each holds the libraries of `darwin-arm64`, `linux-x64` and `win32-x64` side by side; the loader opens the file of its own platform.
Two commands build them:

```bash
# on each platform
monorm-extension-package library <crate-dir> --out <libraries>/<extension> [--zigbuild <target>]
# once, with the three results in <libraries>/<extension>/<platform>/
monorm-extension-package assemble <crate-dir> --libraries <libraries>/<extension> --out <dir> [--tsc <exe>]
```

`library` builds the cdylib and copies it to `<out>/<platform>/`.
It fails on a host with no platform key and lists the 3 keys.
`assemble` writes `<dir>/<module>-<version>-py3-none-macosx_11_0_arm64.manylinux_2_28_x86_64.win_amd64.whl` and the npm package directory `<dir>/npm`.
It fails with the platform key when a library is missing.
The npm `package.json` lists the 3 libraries in `files` and has no `os` or `cpu` field.
`package` stays for local work: it builds the host library only and writes a wheel with a host platform tag.
Do not publish that wheel.

The manifest `library` is a name without a file extension, for example `library = "acme_outside"`.
The file name is `lib<name>.dylib`, `lib<name>.so` or `<name>.dll`.
`assemble` rejects a name with an extension, because three platforms cannot share it.

The compressed platform tag lets pip install the wheel on macOS arm64, Linux x86_64 with glibc 2.28 or newer, and Windows x64 only.
The Linux library builds with `cargo zigbuild` for glibc 2.28 (`library --zigbuild x86_64-unknown-linux-gnu.2.28`).

## Extension contracts

An extension is an installed package ([Installable packages](#installable-packages)).
The manifest `monorm-extension.toml` sits at the root of the package.

| Field | Meaning |
|---|---|
| `id`, `version` | The extension id and its version. |
| `schema_contract` | `"major.minor"`: the majors must be equal, and the minor must not be above the engine minor. |
| `abi`, `library` | Required. `abi` is `"major.minor"`; `library` is the file name without the platform prefix and suffix. A manifest without `abi` fails with "a manifest needs `abi`". |
| `languages` | `python`, `typescript`. |
| `databases` | `postgres`, `sqlite`. |
| `dependencies` | Semver ranges of other extensions, by id. |
| `capabilities` | Host capabilities that the extension adds. |
| `attributes`, `passes`, `exports` | Source attributes, schema passes and hooks (below). |
| `[python]`, `[typescript]` | Exports of the packaged runtime modules (`language_contract`). |

`host_contract`, `requires`, `kind` and the other fields of the removed static path are unknown fields and fail.
Unknown manifest fields are errors.

### Contracts and preparation

The `monorm` crate (`crates/contracts`) contains schema IR, dialects and versioned extension contracts.
It depends on neither bindings nor extension implementations.

Python `define(schema, registry=...)` and TypeScript `define(schema, registry)` prepare a complete candidate before publishing models.
Invalid declarations, missing exports, unsupported language/database combinations and member collisions fail during definition.
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

### The `ext` modules

`monorm.ext` (Python) and `monorm/ext` (TypeScript) are the host surface for extension code.
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

### Source syntax, attributes and passes

An attribute declares its `name`, `target` (`model` or `field`), and typed `arguments` (`string`, `integer`, `boolean`, `list`, `value`).
Only a `value` argument takes a bare name (as a string), `null` and objects; the other kinds take strict literals, also inside a list.
The extension pass checks the shape of a `value` argument.
Source declarations use namespaced attributes, for example `name String @app.rename(to: "public_name")` or `@@app.audit(level: 2)`.
Names, arguments and source locations are preserved in schema requirements.
Unknown namespaces request a rebuild; `@db.*` keeps its existing database meaning.

A pass declares `id`, `phase`, `after`, and owned `effects` paths.
Phases run declaration, logical, storage, behavior, validation, generation.
Ordering is stable;
cycles, missing pass dependencies, duplicate IDs and overlapping effect ownership fail.
The library function of a pass takes `&mut monorm::ir::SchemaIr` and returns `Result<(), String>`.
Namespaced model declarations may derive their primary key during selected passes.
Ordinary models require `@id` before lowering; annotated models must have one after preparation.
Feature-derived relations should be contributed by the pass.

Completed passes and prepared model names (`lowered_models`) are recorded so a generated or serialized normalized schema is not lowered twice.
Additional definition batches lower their new declarations against the complete existing definition context.
Attribute-independent passes use `lowered_models` to distinguish previously prepared models from a new batch.

### Logical fields and physical storage

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

Writes across owner tables belong to `monorm_engine::multi_table`.
Any extension can write a two-table model with these records; `tests/outside-extension` does so.
It plans creates, attaches, filtered updates and deletes, and each runs in one transaction or savepoint.
`update_many` on a multi-table model is rejected.
Ordinary model writes reject inherited storage instead of writing nonexistent columns.
Code generation and both runtimes derive the child models, their own fields and the key default from `behavior.owner_links` and `behavior.field_storage`; a pass writes no hint for them.
The language APIs of these features are in [model-composition.md](../extensions/model-composition.md), [proxy-models.md](../extensions/proxy-models.md) and [selection-and-defaults.md](../schema/selection-and-defaults.md).

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

### Installed libraries

A manifest with `abi` (a `"major.minor"` string, currently `"0.2"`) and `library` (the file name; the loader adds the platform prefix and suffix) is a library.
A manifest with `host_contract` fails and names the field. The loader runs its passes; optional `[python]` and `[typescript]` sections name exports in the packaged runtime modules and retain language-contract validation. The loader resolves the hooks of the manifest with the library.
The crate is a `cdylib` built with `panic = "unwind"` (the default; the exports catch a panic and return a status) that ends with `monorm::extension!`:

```rust
monorm::extension! {
    init = my_init;                          // optional: fn(&serde_json::Value) -> Result<S, String>
    hooks { "my.hook" => my_hook };          // optional: fn(&S, &[Arg], &Context) -> Result<Value, String>
    ("my.pass", my_pass)                     // fn(&mut SchemaIr) -> Result<(), String>
}
```

The macro exports `monorm_ext_abi_version`, `monorm_ext_run_pass` and `monorm_ext_free` (ABI 0.1), and `monorm_ext_init`, `monorm_ext_drop_state` and `monorm_ext_hooks` (ABI 0.2).
`init` gets the JSON of the extension's `extension_data` entry and returns the state `S` (`()` without `init`); the engine calls `monorm_ext_drop_state` when it drops the state.
`S` must be `Send + Sync + 'static`, so that a state can cross threads.
Each hook takes the `&S` of the same `init`; a hook with another state type is a compile error.
A hook has one signature for all seven points. `monorm::abi::Value` has the tags null, bool, int, float, decimal, text, bytes, datetime (microseconds since the Unix epoch, UTC), uuid, json and date (days since 1970-01-01).
An `Err` becomes status ERROR with its text, and a panic becomes status PANIC.
`monorm_ext_init(json, len, out_state, error)` writes `error` on every status, with the failure text, or an empty buffer on OK (a null `error` is allowed); the host frees it with `monorm_ext_free` every time.
A manifest of a library lists its hooks as `[[exports]]` with `id`, `point` (1 to 7), `input` and `output` (a value type).
When the manifest has `exports`, the loader needs `monorm_ext_hooks` and, when `init` exists, `monorm_ext_drop_state`, and the hook table needs a hook of the same name for each export id. A mismatch is an error when the library opens, and it names the export. A manifest without `exports` has no hooks to resolve, so a pass-only library needs none of the three symbols.
`Plan::library(id)` gives a `Library` of a manifest with `exports`; `Library::init(&json)` gives a `State`, and `State::call(hook, &args, &context)` runs one hook. The `State` calls `monorm_ext_drop_state` once when it drops.
Schema contract 4.0 holds `Requirements.hooks` (a `HookBinding` per model, field, point and export) and `Requirements.extension_data` (JSON by extension id). A major bump resets the minor. The IR sets `schema_contract_minor` only for a feature that a later minor adds; an engine of an older minor stops at the unknown field with a parse error, and the "newer than this engine" message comes only from an engine that knows the minor.
Schema contract 4.0 holds `Requirements.model_views` and `Requirements.result_checks`, and the `view_source` key of an attribute.
Schema contract 5.0 removes `Requirements.file_fields` and the `file-storage` host capability: the `monorm.file-storage` extension uses `field_adapters`.
Contract 5.0 also has no `Requirements.extensions`, `Requirements.completed_passes` or `Requirements.methods`. The IR crosses the ABI as JSON, so an extension library that was built before this removal fails to parse the IR with an unknown-field error: rebuild it. After the first publish of the `monorm` crate, such a removal needs a major bump.
Models of contract 4 fail `define` with a "schema contract 4 is older" error; regenerate them.
A `ModelView` (`model`, `source`, `root`, `row_filter`, `insert_table`) makes `model` read and write the table of `root`; `source` is the nearest parent. `row_filter` is a `field` and a list of `values`. Each value must be an enum member of `field`, or `define()` fails. The core builds the literal SQL; an extension never gives SQL text. The filter applies to root reads, joins, and update and delete targets, and `without_defaults()` does not remove it.
A `ResultCheck` (`model`, `field`, `non_null`, `allowed`, `code`) makes the engine warn when a result row breaks the check. The warning carries `code`.
An attribute with `view_source = true` declares the source model of a view. The DSL lowering finds views through this key, not through an attribute name.
`define()` checks each binding: the model, the field and the dependency. When the manifest of the source is in the build, it also checks the point, and that the `input` and `output` types match the field type. When the build has no manifest of the source, the source must be in `Requirements.packages`; if it is in neither, the binding is an error that names the source.
A version fits when its major equals the engine major and its minor is at most the engine minor.
This holds for `schema_contract` and for `abi`, and for the `abi` that the library reports.
Another major, or a newer minor, gives an error that names both versions and says what to install, even when the manifest has fields that this release does not know.
The loader runs the passes of all libraries in one pass order.
The IR records each library that ran in `Requirements.packages`: its version, its passes, its attribute names, supported `languages` and `databases`, `language_members`, `dependencies`, and `manifest_hash` (the lowercase hexadecimal SHA-256 of the raw `monorm-extension.toml` bytes).
The generated models embed this lock in their compiled schema.
The hash excludes native library bytes, so the same models work across platforms.

Run `monorm check` in CI to compare the lock against installed extension packages.
A changed version or manifest, or a missing package, fails with the locked and installed identities and a run `monorm generate` hint.
Unused installed packages add no lock requirement.
Python and npm commands discover packages as they do for generation. The standalone binary takes repeated `--extension ID=MANIFEST` options and, for generation, `--extension-module ID=MODULE` for each selected package (a Python import name or npm package name). The binding commands supply both automatically.
Use `check -o FILE` (or `--out FILE`) for a custom models path; a missing explicit path fails with a regeneration hint.
All hosts check `models.py` and `models.ts` beside the schema when they carry a generated header, and skip other files.
Rust models are outside this lock check.
Python schema facades are checked through their shared `_orm_models.py`.
With no generated models file, the command compiles the schema as usual.
Generated models import every selected package's runtime module, including packages without runtime classes; `load()` and `loads()` discover and import the same modules. Definition reads their runtime members from the package lock. Definition runs no pass. It opens the native library of a package only to resolve the hooks that the schema binds to it; code generation and migrations open no library.

Each runtime module exports `MANIFEST_HASH` (Python) or `manifestHash` (TypeScript), the lowercase SHA-256 of the raw manifest bytes. `define()` compares it with the embedded stamp once per extension per definition. A changed or missing constant warns with the id, both hashes and a run `monorm generate` hint, then continues. Python uses `monorm.OrmStaleModelsWarning`, a `UserWarning` subclass (subject to Python warning filters); Node and Bun emit `OrmStaleModelsWarning`. This warning does not replace the CI lock check above.

TypeScript runtime packages and their dependencies must be synchronously loadable, without top-level await. Node requires 20.19+ on the 20.x line or 22.12+ on later lines; Bun is also supported. Prefer one runtime format. Synchronous loading resolves the package's import entry before loading it, so a dual package uses the same module and class identities as generated ESM imports.
`tests/outside-extension/` is a library in its own workspace.
It also shows the three optional keys of a field attribute in `monorm-extension.toml` (schema contract 2.2).
`field_type = "Money"` makes the attribute own that DSL type: a field of that type lowers to an `Int` placeholder, and the pass replaces it.
`member = false` makes the field no model member: lowering drops the field and keeps its declaration for the pass.
`identities = true` makes a schema that uses the attribute need model identities (`monorm identities`).

#### Read and expression hooks (points 5 to 7)

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

### Installable packages

`monorm-extension-package package <crate dir> --out <dir>` builds one extension crate for the host platform.
The `library` and `assemble` commands build one package for 3 platforms; see [Extension packages for 3 platforms](optional-packaging.md#extension-packages-for-3-platforms).
The crate has `crate-type = ["cdylib", "rlib"]`, the ABI macro and a `monorm-extension.toml` with `abi` and `library`.
The tool writes a `py3-none-<platform>` Python wheel and an npm package directory.
Each package holds the library, the manifest and the runtime module with `MANIFEST_HASH` (`manifestHash`), the SHA-256 of the manifest bytes.
`languages/python/packaging/stage.py` and `npm run assemble` in `languages/js` build the built-in extensions into the one `monorm` wheel and the one `monorm` npm package; there are no separate packages for them.
