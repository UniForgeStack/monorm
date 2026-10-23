# Static native profiles

The `orm` Python wheel and npm package contain language runtime code only. They do
not install drivers, TLS, CLI or generators. A native profile supplies one engine
for the entire process. Profile selection is fixed during initialization; existing
schemas, objects, pools and transactions cannot switch engines.

| Profile | Databases | CLI | Generators | Feature adapters | Extensions |
| --- | --- | --- | --- | --- | --- |
| postgres | PostgreSQL | no | no | none | none |
| sqlite | SQLite | no | no | none | none |
| combined | PostgreSQL + SQLite | no | no | none | none |
| tooling | PostgreSQL + SQLite | yes | Python + TypeScript | none | none |
| full | PostgreSQL + SQLite | yes | Python + TypeScript | none | all nine first-party |

Python installs use `orm[postgres]`, `orm[sqlite]`, `orm[combined]`, `orm[tooling]` or `orm[full]`.
These extras select complete distributions, not changes to an installed binary.
Each native wheel owns a unique `orm_native_<profile>._native` namespace.
Installing `orm[postgres,sqlite]` installs two engines, not a combined engine. Use
`orm[combined]` for both databases. With exactly one installed profile, initialization
selects it automatically. With multiple profiles, set `ORM_PROFILE` to one profile.
Initialization fails before any query for an unknown or absent selector, an
ambiguous install, an incompatible ABI or version, or an unexpected backend or adapter
set. A change to the environment after initialization does not change the engine.

Node installs use `orm` alongside `@orm/native-postgres`, `@orm/native-sqlite`,
`@orm/native-combined`, `@orm/native-tooling` or `@orm/native-full`. Optional peers do not install every
profile. Each selected profile's optional dependencies are **platform artifacts**,
not additional database features. Supported prebuilt targets are darwin-arm64,
linux-x64 (glibc) and win32-x64. Unsupported targets require a source build. The
release workflow builds CPython 3.11–3.14 wheels and N-API 8 Node artifacts. CI keeps
the artifacts. No job publishes them.

After the distributions are published:

```bash
python -m pip install 'orm[sqlite]'
export ORM_PROFILE=sqlite
npm install orm @orm/native-sqlite
```

`python -m orm` and `npx orm` require the tooling or the full profile.
The standalone `orm` binary does not use a profile.
All runtime profiles contain the migration and schema APIs.
Each binding exposes a generator only when the build selects it.
The tooling profile contains both generators, so each language CLI generates both languages.

```bash
python -m pip install 'orm[tooling]'
export ORM_PROFILE=tooling
python -m orm generate
npm install orm @orm/native-tooling
npx orm generate typescript
```

## The full profile

The `full` profile is the `tooling` profile plus every first-party extension:
`orm.proxy`, `orm.query-defaults`, the model-composition extension (`composition`), `file-storage`, `orm.generic`, `orm.timestamps`, `orm.soft-delete`, `orm.locking` and `orm.inherit`.
It has the generators because only the artifact that holds the extensions can write their `orm_ext/` files during `orm generate`.

```bash
python -m pip install 'orm[full]'     # orm-native-full
npm install orm @orm/native-full
export ORM_PROFILE=full               # only when more than one profile is installed
```

The build goes through `orm-extension-build`, never with hand-set host features.
The inputs are `build.json` in `languages/python/packaging/full/` and in `languages/js/packaging/full/`.
`python languages/python/packaging/build-full-wheel.py [wheel-dir]` builds the wheel.
`node languages/js/packaging/build-node-profile.mjs full --release` builds the Node package.
Both commands need `cargo`, `mypy` and `pyright` on `PATH`, and the wheel build needs `maturin`.
Run `npm install` in `languages/js` first: the frontend type-checks the embedded TypeScript with its `tsc`.
`profile_metadata()["build"]["features"]` lists the host features only (with `profile-full`).
`native_artifact()["extensions"]` (`nativeArtifact().extensions` in Node) lists the compiled extensions.
`profile_metadata()["capabilities"]["composition"]` is `true` for `full` only.
The file-storage language packages stay separate.
Add `orm-file-storage` (Python) or `@orm/file-storage` (Node) for file fields.
CI records the size of every native wheel and package in the job summary.
There is no size limit.

## Exact source builds

Cargo features are additive. Reproducible minimal builds always pass
`--no-default-features`. The default features select both backends and all tooling.
Engine backend features are `postgres` and `sqlite`. Bindings forward selected
backends without enabling dependency defaults. `cli` selects the CLI crate.
`generate-python` and `generate-typescript` select generator modules independently,
including through an optional CLI dependency. `composition` selects the build-time
static extension host; extension contract/compiler logic belongs to the build-time
extension system. It is distinct from model composition.

Baseline native profiles do not claim optional query-defaults, reference-loading,
proxy-models, model-composition, file-storage or generic-relations support.
The tooling profile and the default binding builds add `reference-loading`.
The five feature extensions come only from extension crates selected in
`orm-extension-build`; do not pass their host features by hand (for example
`maturin build --features proxy-models`). A marker package cannot add code to an
already compiled engine. [Build-time extensions](build-time-extensions.md#select-feature-extensions)
has the configuration, the build commands and the packaging step: the Python library
goes into an `orm_native_custom` package (`ORM_PROFILE=custom`), and the Node library
is selected with `ORM_NATIVE`.

Build a custom Python artifact using the `custom` template. Every artifact embeds
its build record in `profile_metadata()["build"]`: the exact Cargo features, the
`rustc -V` toolchain, target triple, Cargo profile and git revision (`git rev-parse
HEAD` of the host source, or `ORM_BUILD_REVISION` when set; `unknown` outside git).
Keep the Python version and the extension build configuration (`build-input.json`
of `orm-extension-build`) with the artifact; the record does not hold them. This template has no
backend by default; at least one backend must be explicitly selected. The
features on the command line replace the `features` list of the template pyproject.

```bash
cd languages/python/packaging/custom
maturin build --features postgres,generate-python --out ../../../target/custom-wheels
cd ../../..
python -m pip install .
python -m pip install --no-index --find-links target/custom-wheels orm-native-custom
export ORM_PROFILE=custom
```

The custom loader validates the ABI, version and language, unique supported backend
names, boolean capability metadata, and adapter and capability consistency. It does
not know the exact feature set of a custom artifact. The application build record
must specify it. Install one custom wheel per environment. For Node, an explicit
source-built path is selected once with `ORM_NATIVE`:

```bash
cd languages/js
node scripts/build-native.mjs --features postgres,generate-typescript
export ORM_NATIVE="$PWD/orm.node"
```

For named Node release profiles, `node languages/js/packaging/build-node-profile.mjs sqlite
--release` stages a platform distribution under `target/node-profiles/`. Python
profile pyprojects under `languages/python/packaging/<profile>/` select only their named Cargo
profile with defaults disabled. Never enable multiple named profile features in one
build; exact metadata validation rejects broadened named artifacts.

## Immutable adapter contract

Native `profile_metadata()` / `profileMetadata()` returns JSON ABI 1 with `version`,
`language`, `profile`, `backends`, boolean `capabilities`, selected `adapters`, and
the `build` record (`features`, `rustc`, `target`, `profile`, `revision`). The loaders
reject an artifact without a well-formed build record.
This metadata is separate from schema/extension manifests. An adapter name must
have a true capability with the same canonical name. Python
`orm._capabilities.ADAPTERS` is a frozen set initialized at module import;
TypeScript `nativeAdapters()` returns a cached frozen array. Selected adapter modules
and specialized model methods are bound at module/definition initialization.
Disabled paths retain ordinary decoding/writes; they do not import providers,
copy rows, scan fields or test flags on every operation. Profile builds must include
exactly their selected adapters and dependencies. No query-time plugin discovery.

## Verification

`languages/python/packaging/check-dependencies.py` audits isolated graphs for every binding/profile,
including backend SQL-builder unification and absence of CLI/TLS/unused drivers.
`languages/python/packaging/check-wheel.py` checks thin/native wheel payload separation.
`smoke-python.py` and `smoke-node.mjs` check the compiled extension set: none, or all nine for `full`.
The `full` run also defines a schema with `@@proxy.of`.
`languages/js/packaging/check-node-install.mjs` packs and installs one native profile offline,
checks the dependency lock and runs installed-package smoke tests. Python
`languages/python/packaging/smoke-python.py` runs against fresh installed wheels. Set
`ORM_TEST_DATABASE_URL` to an isolated PostgreSQL database for PostgreSQL checks;
SQLite checks use in-memory databases. CI Linux jobs provide an isolated service.
The release workflow keeps the wheels and tarballs as CI artifacts. It does not
publish them.
