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

Python installs use `orm[postgres]`, `orm[sqlite]`, `orm[combined]` or `orm[tooling]`.
These extras select complete distributions, not changes to an installed binary.
Each native wheel owns a unique `orm_native_<profile>._native` namespace.
Installing `orm[postgres,sqlite]` installs two engines, not a combined engine. Use
`orm[combined]` for both databases. With exactly one installed profile, initialization
selects it automatically. With multiple profiles, set `ORM_PROFILE` to one profile.
Initialization fails before any query for an unknown or absent selector, an
ambiguous install, an incompatible ABI or version, or an unexpected backend or adapter
set. A change to the environment after initialization does not change the engine.

Node installs use `orm` alongside `@orm/native-postgres`, `@orm/native-sqlite`,
`@orm/native-combined` or `@orm/native-tooling`. Optional peers do not install every
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

`python -m orm` and `npx orm` require the tooling profile.
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

## `orm[full]`

`orm[full]` has no code of its own.
It installs the `tooling` native profile and the Python package of each first-party extension except file storage, each pinned to its manifest version.
Each package is an installable package; see [Installable packages](build-time-extensions.md#installable-packages).
A library loads only when the schema uses its extension.

```bash
python -m pip install 'orm[full]'
```

For Node there is no meta-package: discovery reads only the direct dependencies of the project `package.json`.
Install the packages by name:

```bash
npm install orm @orm/native-tooling @orm/generic @orm/inherit @orm/locking @orm/composition \
  @orm/proxy @orm/query-defaults @orm/soft-delete @orm/timestamps
```

`python languages/python/packaging/check-full-extra.py` keeps the extra equal to the manifests in `extensions/` minus the excluded list (file storage).
Add `orm-file-storage` (Python) or `@orm/file-storage` (Node) for file fields.
CI records the size of every native wheel and package in the job summary.
There is no size limit.

## Exact source builds

Cargo features are additive. Reproducible minimal builds always pass
`--no-default-features`. The default features select both backends and all tooling.
Engine backend features are `postgres` and `sqlite`. Bindings forward selected
backends without enabling dependency defaults. `cli` selects the CLI crate.
`generate-python` and `generate-typescript` select generator modules independently,
including through an optional CLI dependency.

Baseline native profiles do not claim optional reference-loading support.
The tooling profile and the default binding builds add `reference-loading`.
An extension is an installed package ([Build-time extensions](build-time-extensions.md#installable-packages));
a marker package cannot add code to an already compiled engine.
Every artifact embeds its build record in `profile_metadata()["build"]`: the exact Cargo features, the
`rustc -V` toolchain, target triple, Cargo profile and git revision (`git rev-parse
HEAD` of the host source, or `ORM_BUILD_REVISION` when set; `unknown` outside git).

A Node source build is selected once with `ORM_NATIVE`:

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
`smoke-python.py` and `smoke-node.mjs` check that no extension is compiled in.
The `orm[full]` job installs the extra from local wheels and runs a smoke over one attribute of each member.
`languages/js/packaging/check-node-install.mjs` packs and installs one native profile offline,
checks the dependency lock and runs installed-package smoke tests. Python
`languages/python/packaging/smoke-python.py` runs against fresh installed wheels. Set
`ORM_TEST_DATABASE_URL` to an isolated PostgreSQL database for PostgreSQL checks;
SQLite checks use in-memory databases. CI Linux jobs provide an isolated service.
The release workflow keeps the wheels and tarballs as CI artifacts. It does not
publish them.

## Extension packages for 3 platforms

An installable extension is one wheel and one npm package.
Each holds the libraries of `darwin-arm64`, `linux-x64` and `win32-x64` side by side; the loader opens the file of its own platform.
Two commands build them:

```bash
# on each platform
orm-extension-package library <crate-dir> --out <libraries>/<extension>
# once, with the three results in <libraries>/<extension>/<platform>/
orm-extension-package assemble <crate-dir> --libraries <libraries>/<extension> --out <dir> [--tsc <exe>]
```

`library` builds the cdylib and copies it to `<out>/<platform>/`.
It fails on a host with no platform key and lists the 3 keys.
`assemble` writes `<dir>/<module>-<version>-py3-none-any.whl` and the npm package directory `<dir>/npm`.
It fails with the platform key when a library is missing.
The npm `package.json` lists the 3 libraries in `files` and has no `os` or `cpu` field.
`package` stays for local work: it builds the host library only and writes a wheel with a host platform tag.
Do not publish that wheel.

The manifest `library` is a name without a file extension, for example `library = "orm_inherit"`.
The file name is `lib<name>.dylib`, `lib<name>.so` or `<name>.dll`.
`assemble` rejects a name with an extension, because three platforms cannot share it.

The `py3-none-any` tag lets pip install the wheel on a platform with no library; the loader then fails with "cannot load".
The Linux library links against the glibc of the CI runner (`ubuntu-24.04`).
`.github/workflows/extension-packages.yml` builds the libraries (`libraries`), assembles and checks the packages and records their sizes (`assemble`), and installs them on each platform and runs the main seam (`consumers`).
It publishes nothing.
