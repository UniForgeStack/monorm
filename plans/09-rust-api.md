# Rust client: typed queries, ordinary fields, and build-time guarantees

Status: design draft, not implemented. User choices and engineering defaults are recorded separately.
Updated: 2026-10-10.

## Agreed direction

- Add a high-level Rust client over the existing `orm-core` and `orm-engine`.
- Cover the existing Python/TypeScript operations and preserve their database semantics.
- Prefer schema-generation and Rust compiler checks over runtime checks whenever the necessary information is static.
- Keep ordinary field syntax: `user.email`, not getters such as `user.email()`.
- Generate one generic model definition, rather than a struct declaration for every loaded-field combination.
- Field-level `Deref` is unnecessary for statically loaded fields: their types can be ordinary `String`, `Option<T>`, integers, enums, and relation values.
- Reading fields remains synchronous and never performs I/O. Loading and execution remain async.
- Support both typed consume/rebind loading and mutation with runtime checks for runtime-selected field lists. Distinct method names and row modes must make the guarantees clear.
- Keep model fields read-only; use explicit write operations to change stored values.
- Automate named projection/annotation types with macros rather than requiring users to declare every result struct.
- Function and struct requirements use `UserShape<Email = String, Posts = LoadedPosts>` directly; do not require separate `UserHasEmail`/`UserHasLoadedPosts` capability traits.
- Expose a model query root as `User.objects` or `User::objects`; the user accepts both. Column/path values follow the selected form consistently. The row remains the generic Rust type `User<S>`.
- Resolve routine design choices autonomously. Ask the user only when a material tradeoff cannot be resolved from these preferences or existing Python/TypeScript behavior.

All Rust syntax below is proposed, except the compiler probe described under validation. A proposed signature is not evidence that its complete trait implementation exists. Names, packaging, and undecided ergonomics remain reviewable.

## Evidence and existing surface

The engine is already a directly usable Rust library; the comparison harness constructs typed operations, plans them, executes them, and decodes Rust values without either binding. Both engine/core crates currently use `publish = false`.

Read the checked-out sources, including current uncommitted changes, when implementing this plan. The codebase graph was queried but this repository was not indexed, so this inventory used local documentation, exports, and source reads.

Primary repository references:

- [TypeScript API](../docs/typescript-api.md) and [Python API](../docs/python-api.md).
- [TypeScript exports](../js/src/index.ts) and [Python exports](../python/orm/__init__.py).
- [Query state and terminal behavior](../js/src/query.ts), [projection types](../js/src/select.ts), and [instance loading](../js/src/loading.ts).
- [Public and private selected fields](../docs/selection-and-defaults.md).
- [Instance loading and cache invalidation](../docs/async-reference-loading.md).
- [Behavioral extensions](../docs/build-time-extensions.md), [column roles](../docs/schema-extensions.md), [proxy models](../docs/proxy-models.md), and [generic identity delivery state](../docs/generic-identities.md).
- [Direct Rust execution example](../bench/rust-compare/src/main.rs).

The inventory distinguishes shipped binding operations from planned extension operations. In particular, generic identities/compiler contributions exist, but the documentation says the full binding generic routing/loading surface is still being implemented. Do not call that completed parity work.

## Module placement and execution seam

Proposed modules:

| Module | Responsibility |
| --- | --- |
| `orm-client` | Typed expressions, query builders, execution context, result construction, instance operations, errors |
| Rust generator in `orm-core::codegen` | Schema-specific models, field/path tokens, state transitions, input types, unique-key tokens, codecs, metadata |
| `orm-client-macros` | Automated named projections/CTE columns and ergonomics that require Rust identifiers |
| Existing `orm-engine` | Planning, dialect checks, drivers, execution, extension behavior, migration execution |
| Existing extension-build system | Selected extension compilation and generated Rust capability requirements |

Use the existing schema compilation and typed operation IR. Bind values separately through `Params`; do not serialize queries to JSON or route through Python/Node. Keep the planner and driver implementation shared. Move reusable execution orchestration out of a binding only when Rust needs it; do not duplicate its SQL semantics in the client.

Generated type and query-state information should be erased at the planning seam. Keep large async execution functions nongeneric where practical to control compiler work and binary size. The generator must consume normalized result shapes and extension declarations, including helper columns, rather than rediscovering their rules.

## What “build-time checking” means

1. Schema generation rejects invalid declarations, unsupported configured capabilities, name collisions, impossible insert shapes, and incompatible extensions.
2. Rust type checking rejects invalid field access, input types, expression sources, missing parameters, and invalid statically known builder states.
3. Runtime planning validates dynamic expression identity and capability overrides that cannot be proven statically.
4. Database execution determines existence, cardinality, constraint violations, concurrency conflicts, connectivity, and actual values.

Compilation must not connect to a live database. Successful compilation cannot prove that the deployed database matches the schema, that a row exists, or that a transaction commits.

## Models and ordinary field access

Conceptual definition:

```rust
pub struct UserFields<Email, Name> {
    pub email: Email,
    pub name: Name,
}

// Partial shape: UserFields<String, Unloaded>
// Full shape:    UserFields<String, String>
```

The generator can hide individual parameters behind a generated shape trait with associated field types, e.g. `User<S>` and `S::Email`. Start with ordinary stable Rust generics/associated types. Do not depend on specialization, arbitrary generic const arithmetic, or generating every possible subset.

For statically loaded fields, expose `T`; for a loaded nullable field, expose `Option<T>`; for a definitely unloaded field, expose a marker with no value methods, conversions, `Display`, or `Deref<Target = T>`. Loaded state and SQL NULL remain independent.

`user.email` accesses a real value when loaded. If unloaded, it accesses an `Unloaded` marker. The compiler rejects using that marker as the underlying value, but does not reject merely borrowing or moving the marker. Never describe this as intercepting every field read.

### Read-only model view: accepted requirement, proposed representation

To preserve explicit-write semantics, the proposed `User<S>` owns private `UserFields<S>` and implements only model-level `Deref<Target = UserFields<S>>`, not `DerefMut`. Then `user.email` and `user.email.len()` remain available, while replacing or mutating fields directly is rejected. Use no mutable interior APIs that bypass that rule.

This `Deref` serves field visibility and immutability, not loaded-state checking. No field-level wrapper is needed in the static path. Moving a non-Copy field out through an immutable view is unavailable; callers borrow or explicitly clone an owned value. For owned extraction, offer an explicit `into_data` operation if requested.

Writable public model fields are excluded by the user's read-only decision. Owned data extraction, if offered, must be distinct from a tracked read-only instance.

Keep identity and helper keys private and always available where the engine shape requires them. A fetched private foreign key must not make its corresponding public field statically loaded. Separate model column/path values from instance values: `User.email` builds an expression; `user.email` reads a loaded value. Generate the model value `User` in the value namespace and row type `User<S>` in the type namespace (Q5).

Use schema field names in snake_case by default, handling Rust keywords with raw identifiers when possible. Detect normalization, token, generated helper, and extension collisions during generation. Preserve physical names and mappings.

Equality/hash compare logical model identity and primary key across loaded shapes of that same model. Proxy and concrete models remain distinct logical identities. Instances originate from successful decoding/insertion, not arbitrary public constructors that can forge private keys or loaded states.

### Function requirements: loaded fields as lower bounds

A function must be able to require email and cached posts without fixing the entire user shape. Use the generated shape trait's associated-type constraints directly. These describe minimum requirements; extra loaded fields remain accepted. The user explicitly rejected needing separate per-field capability traits.

```rust
// Proposed generated shape and client syntax.
fn send_digest<S>(user: &User<S>)
where
    S: UserShape<Email = String, Posts = LoadedPosts>,
{
    println!("{}: {} posts", user.email, user.posts.cached.len());
}
```

`Email = String` establishes an ordinary loaded email. `Posts = LoadedPosts` establishes a loaded cached posts collection in this simplified example. Actual related sets should carry their target shape and cache state, so a generic function can accept any compatible selected post shape. Merely having a queryable `posts` relation does not satisfy the loaded-cache requirement. Nested requirements constrain the related post shape directly with `PostShape<Title = String>`.

Generate the shape trait and implementations automatically; callers must not write implementations for every subset. Optional macros may shorten combined/nested requirements, but direct associated-type bounds are the primary interface. Macro expansions use generic/opaque shape bounds, not an exact concrete selection alias. An alias representing only email/posts would incorrectly reject users that also loaded name. Nested associated bounds require a feasibility probe.

Runtime-state rows cannot claim these associated field types merely because load was attempted. Provide explicit checked conversion into a chosen static requirement: validate existing values/loaded flags without I/O, then return a typed view or owned row; otherwise return NotLoaded. A dynamic row with loaded email and posts can then cross the same function interface after that validation.

Initial compiler evidence: a simplified generic shape probe accepted both email+posts and email+posts+name in one function, exposed email as an ordinary String and posts as an ordinary Vec, and rejected missing-email/missing-posts arguments. The probe verifies the associated-type/trait mechanism; actual RelatedSet cached states and nested requirements remain implementation gates.

[Complete executable design example](examples/rust-loaded-requirements.rs): generator-like shape/trait declarations, read-only User fields, fixture loaders for partial/full users, a function requiring email/posts, and a generic DigestJob struct with those same requirements. It compiled and executed with rustc 1.97.0. The loaders use in-memory rows; this does not implement database loading or the proposed client query syntax.

[Model value naming probe](examples/rust-model-object.rs) verifies that a constant model value named `User` can coexist with `User<S>` and expose `User.objects.first().await`. It also verifies `User::objects.first().await` through an associated query root defined on the default shape, without supplying explicit shape parameters. It uses an in-memory fixture and a synthetic eager-posts shape to test naming; ordinary first-result posts are unloaded unless selected by defaults or explicitly requested.

The intended application syntax is `let user = User.objects.first().await?` for optional default-selected rows, and `let user = User.objects.load((User.email, User.posts)).require().first().await?` for a required row with these two selections. Ordinary Rust syntax still requires `let` when declaring a binding and `.await` for async execution. A bound context or configured default database supplies execution; explicit `using`/transaction handles remain available for static context requirements.

## Complete parity map

“Static” below means a goal for the generated typed path. Dynamic forms use checked runtime planning and honestly dynamic result types. Items needing a feasibility probe are not promised merely because the table names a trait.

| Existing family | Rust interface direction | Static guarantees | Runtime work/limits |
| --- | --- | --- | --- |
| Generated models/enums; `define`, schema `load`/`loads`, registries | Generated module with prepared schema; optional `DynamicRegistry` | Field types, enum members, schema/profile compatibility at generation | Loading arbitrary schema cannot create new named Rust types; dynamic mode validates runtime schemas |
| `filter`, `exclude`, boolean `and/or/not` | Typed expressions and condition tuples | Operand compatibility, source visibility, valid relation cardinality | Preserve same-filter to-many grouping versus separate-filter independence |
| Query-set `or` | State-restricted composition of same-model queries | Same model; unsliced/filter-only states; number of filter calls if tracked | Residual dynamic state checks; preserve current restrictions |
| `order_by`, `limit`, `offset`, slicing | Field/expression tokens; typed counts; range builder | Valid source/cardinality, count types | Numeric ranges, overflow, dynamic bounds |
| `without_defaults`, `without_related`, default order | State transforms using generated schema policies | Correct new selection and reference states | Resolve filter scope values; preserve actual engine defaults |
| Query-set `load` | Typed scalar/relation/path/query descriptors | Public selection, nested target shapes, relation roots | Joins/prefetches, filtering, batching, runtime integrity |
| Instance `load` | Consume/rebind to expanded shape | New loaded state after success | Fetch missing values; preserve existing values unless reload requested |
| `prefetch(instances, ...)` | Consume homogeneous collection and return upgraded collection | Loaded relation shape after success | Batched key collection and fetch; dynamic/mixed collections require erased states |
| `label`, `as_prefetch` | Relation-only builders; typed label identifiers if macros selected | Legal relation use; known literal collisions; one/many result shape | Arbitrary runtime label names cannot create Rust fields |
| `all`, query awaiting, async iteration/cache | Explicit terminals; bound `IntoFuture`; explicit reusable cache | Runnable parameters/scopes; result shape | Execute/cache; ownership differs from TS thenables |
| `get`, `first`, `last`, `require` | Requiredness marker changes terminal output | `Option<R>` versus `R`; modifiers preserved | Zero/multiple rows; no compile-time existence proof |
| `count`, query-set `exists`, `in_bulk` | Counts/bool/map; unique-key tokens for bulk lookup | Key/result types; known unique fields | Counts, missing keys, batching, actual uniqueness |
| `batches`, `iterate`, pagination | Fallible streams, typed order/cursor configuration | Supported query states; order types; mutually exclusive forward/backward options | Cursor parsing/fingerprint, row counts, EOF, inter-language encoding |
| `prepare`, `param`, prepared terminals/inspection/SQL | Typed placeholder tokens and generated/macro binding packs | Required parameter set, compatible repeated uses, no unresolved outer references | Build reusable plan; validate values/ranges; bind current scope per execution |
| `select`, annotations, aggregates | Tuple or named projection object | Output types, nullability, scalar arity, expression stages/sources | Grouping expressions requiring semantic identity; actual values |
| `group_by`, `having`, `distinct`, distinct-on | Stage/state constrained projection builders | Known grouping dependency rules and operation eligibility | Complex expression equality/order equivalence; dialect behavior |
| `as_scalar`, `in(subquery)`, `exists(subquery)`, `outer` | Single-column typed subqueries and scoped outer tokens | Arity, source visibility, compatible types, unresolved references disallow execution | Unproven scalar cardinality and database-dependent failure |
| Functions, JSON, arrays, full-text search | Typed function/operator modules | Input/output types, nullability, dialect capability requirements | Dynamic JSON shape, numeric value ranges, DB implementation |
| Window functions and shared windows | Window-ready expressions and frame enums | `.over()` required; allowed clauses; rows/range exclusion | Runtime frame bounds and current shared-window planner restrictions |
| CTEs, recursion, joins, `from` | Typed projected columns and CTE source identity | Base/recursive shape compatibility, model shape for `from`, writes disallowed on joined sources | Runtime name collisions/dynamic identifiers; recursion termination |
| Insert/insert-many/COPY | Generated insert inputs/builders and distinct strategy states | Required inputs, field types, supported strategy/dialect | Defaults, validators, duplicate rows, uniform COPY field sets, constraints |
| Upsert/on-conflict/excluded/get-or-insert | Unique-key/index tokens and separate conflict action types | Constraint member sets, legal update fields, excluded context, return optionality | Conditional index matching, concurrent insert, hidden conflicting row |
| Update/update-many/delete/returning | Typed assignments and inspectable statement builders | Writable fields, expression types, unsliced/unjoined/unlocked mutation states | SQL, duplicate update keys, parameter batches, conflicts |
| Instance update/delete/refresh | Shape-aware operations using private identity/version | Required version/key availability; selection transforms; invalidation states | Missing row, stale version, refresh skip result |
| Related sets and M2M add/remove/clear/set | Relations remain queryable; separate cached data state | Correct target, child/join inputs; owning key auto-filled | Link existence, defaults, atomic set, constraints |
| Transactions/savepoints/on-commit | Explicit borrowed handles | Lock/protection capability and lifetime use | Commit/rollback, callback errors, cancellation, serialization/deadlock |
| Row/advisory/session locks | Transaction-only row/advisory execution; scoped session handle | Required context/dialect; mutually exclusive policies | Contention, timeout, cleanup, skipped rows |
| Replicas, primary view, `.using` | Typed execution context/primary view | Dialect/schema compatibility; transaction identity lifetimes | Replica lag, routing/failure, pool counts |
| Tenants/RLS and `scope` | Typed context values generated from requirements | Required schema scope keys/types where inferable; Pg-only tenant interface | RLS/database policies, dynamic scope dependencies and overrides |
| Protected writes | Explicit permit carried by context | Required permits for known physical write owners | Actual DB permissions; dynamic operation dependencies |
| `sql`, `inspect`, `explain` | Typed diagnostic terminals over existing engine | Runnable state; supported explain options | Planning details and server plan; analyze executes |
| Raw fetch/execute | Dynamic rows or explicit typed decoder | Known decoder type only; typed values | SQL syntax, returned columns/types, schema drift; bypasses ORM checks |
| Query events, debug N+1, OpenTelemetry | Typed callbacks and optional instrumentation | Event/callback types | Runtime counts/timing, callback failures, task-local propagation |
| Package prepare/decode hooks | Typed field codecs and prepared write adapters | Storage/value shape, read-only decoded fields, dependency types | Codec failures, extension validation, IO hooks |
| `describe`, metadata/factories, serialization | Generated immutable metadata and insert inputs; loaded-only serialization | Input types and available static model info | Dynamic inspection; loaded-only shape serialization |
| Migrations, pull, baseline, drift, CLI | Reuse shared migration core/engine; add Rust generation | Schema/source checks; typed migration callbacks | Filesystem and DB drift, SQL execution, migration history |
| Optional extensions and generated methods | Build-selected typed traits and generated members | Selected capabilities and supported operation shapes | Validators, computations, warnings and external services |

## Loading semantics and state transitions

The typed path must reproduce existing selection rules, not silently replace them with “all load calls start empty.”

- A first explicit root-column selection replaces the normal/default scalar selection; later explicit scalar loads add fields. Loading only relations leaves normal scalar selection intact.
- An all-scalars operation resets the scalar selection while retaining applicable references and caller filters.
- Target-column loading narrows that related object; an explicitly loaded bare target path requests its complete normal shape as defined by the existing planner.
- Target default filters can make a physically required reference absent. Generate `Option<Target>` for that view; do not promise a value just because the FK is non-null.
- Public values distinguish unloaded, loaded NULL, and loaded value. Collections distinguish unloaded from loaded empty.
- Relation paths carry finite requested nested shapes. Box to-one values where recursive model layout requires indirection; to-many values use owned collections/queryable relation handles.
- Keep `.cached` behavior on collection relations: cached rows are available only in a loaded-cache state. Querying the relation does not require cached data.
- Filtered and per-parent limited related querysets preserve their existing semantics. A label is a separate result member, not a replacement for the canonical cache.
- Preserve loaded instance fields, references, NULLs, and empty lists during additive load. Reload refreshes requested values.
- Typed collection prefetch should improve on the current binding helper, whose return type does not upgrade individual instances.

### Rust arity and instance ownership: Q1, Q6, Q7

Rust has no TS/Python-style method overloads for zero/one/many arguments. Proposed syntax is `.load(User.email)` or `.load((User.email, User.name))`, with `.load_all()` for all scalar fields. An alternative is `.load(())` for the all-scalars form or a small load macro. Decide before stabilizing examples.

For instance loads, consume and return an upgraded type:

```rust
let user = user.load(User.email, &db).await?;
let user = user.load(User.name, &db).await?;
// Both fields are ordinary values now; no field getter or wrapper is needed.
```

An `&mut self` operation cannot change that variable's static type. The user requested both modes. Proposed names distinguish typed `.load(...)` from runtime `.load_dynamic(...)`, with a runtime-state model representation for the latter. Exact names remain proposals.

```rust
// Proposed runtime path: one fixed Rust type regardless of selected fields.
let mut user = user.into_dynamic();
user.load_dynamic(&fields, &db).await?;
user.email.len(); // Runtime check when accessing the underlying value.
```

The runtime model still exposes read-only fields through an immutable view. Its mutable loading methods change private state; they do not permit caller field assignment. A dynamic field must store either unloaded or a valid value, and check before exposing the value. A static `Unloaded` field cannot simply become a String in place. Converting to runtime mode represents every potentially loaded field with a stable runtime-state type and conservatively erases static field guarantees while retaining actual values and loaded flags. Mixed static/dynamic field guarantees can be a later optimization.

Differently selected query branches cannot share one static local type without unifying shapes, using an enum, or erasing state. Runtime query selections return runtime-state rows. Conversion from runtime to a chosen static shape requires explicit checked validation; it cannot infer a new type from runtime strings.

Consuming a row also means an ordinary error/cancelled future can drop it. Decide whether typed transition errors should return the original instance, and whether preserving ownership across cancellation requires a separate runtime mutation operation. Read-only load results can be staged before publishing; do not promise rollback of already executed database writes when an instance write fails during later decoding.

### Invalidation after update/refresh: Q8

The loaded-state type must remain honest after changes:

- FK updates invalidate the corresponding to-one cache.
- Refresh invalidates reverse one-to-one caches under the existing contract.
- Changes can invalidate computed values and dependent nested caches.
- Versioned instance writes require the version field publicly loaded under current semantics, even when identity is privately available.

A typed operation must return a shape with invalidated relations unloaded, or explicitly reload them before returning a loaded shape. A conservative first implementation unloads all potentially affected references, then refines dependency tracking. Use ordinary `&mut self` only for operations proven not to change guarantees, or explicitly runtime-checked rows.

For refresh with skip-locked, no row may be obtained. Return an enum carrying either the original instance or the upgraded/refreshed one, rather than returning `false` alongside a type that incorrectly promises new fields (Q8). Use `RefreshOutcome<Old, New>` with unchanged/refreshed variants as the design default.

## Reads, require, and nullable scalars

Preserve the current `get` contract: optional unless `.require()` is applied; multiple rows always fail. `first`/`last` are optional and use the existing ordering rules. Do not change `get` to “always required” merely because that is common in other Rust ORMs.

Proposed terminal mapping:

| Terminal | Normal result | Required result |
| --- | --- | --- |
| `get`, `first`, `last` | `Result<Option<R>>` | `Result<R>` |
| `all` | `Result<Vec<R>>` | `Result<Vec<R>>`, error when empty |
| `scalar` selecting non-null T | `Result<Option<T>>` | `Result<T>` |
| `scalar` selecting nullable T | `Result<Option<Option<T>>>` | `Result<Option<T>>` |
| `scalars` | `Result<Vec<T>>` or `Result<Vec<Option<T>>>` | Same element type; error when empty |
| `count`, `exists` | Count / boolean in `Result` | Unchanged zero/false behavior |

The scalar's outer `Option` distinguishes missing row from SQL NULL. Current Python/TS APIs collapse some of these states; Rust should preserve the distinction. `.require()` proves presence after successful execution, never non-nullness. A nonempty collection wrapper is an optional Rust-specific addition, not necessary for parity (Q9).

Builders, prepared queries, diagnostics, and terminals must preserve requiredness. Iteration must check the empty case and yield a typed error. `get` still checks cardinality at runtime even when filters use a generated unique token: deployed schema drift, query joins, and dynamic predicates remain relevant.

## Expressions, scopes, and SQL validity

Use field/path marker identities, source-scope information, value types, and expression-stage markers. Carry nullability through optional joins, arithmetic, aggregates, JSON extraction, and scalar subqueries.

- Use comparison methods such as `.eq`, `.lt`, `.is_null`: Rust's comparison operators return booleans, so Python's overloaded comparison-expression syntax cannot be copied.
- Allow numeric/string/array/JSON operators only on compatible expression types. Arithmetic and SQL concatenation follow existing NULL rules.
- Distinguish root-row, to-one, and to-many paths. To-many paths are legal in relation filters/aggregates, not ordinary projection/order columns that would multiply rows.
- Restrict `excluded` to upsert update contexts.
- Distinguish aggregate/window/set-returning/native-computed expressions and their valid clauses. Window-only functions need `.over()` before selection; `unnest` stays a projection operation.
- Encode frame alternatives and lock wait policies as enums, avoiding contradictory option booleans.
- Use dialect requirement traits for arrays, array aggregation, JSON operations, full-text search, COPY, locks, and explain/analyze. The executor must satisfy the query's requirements. Functions with existing emulations remain available on supported dialects.
- A scalar subquery is one selected column and nullable for no rows. Prefer a statically at-most-one state (limit-one, suitable ungrouped aggregate, or proved unique lookup), with an explicit checked dynamic form for unproven cardinality (Q10).
- Track unresolved parameters and outer sources; no execution, standalone preparation, or diagnostics that require values until obligations are satisfied.

Different aliases of the same model and nested `outer()` references need distinct source identities, not merely model-name types. Prototype macro-generated markers or scoped branded tokens on stable Rust before committing to syntax. CTE joins must introduce their source, and a recursive CTE must type-check its returned shape against the base.

SQL grouping and distinct/order identity are not entirely a type problem. A marker can prove a selected field is in a static grouping list, but two arbitrary expressions of the same Rust type need not be semantically equal. Keep explicit planner validation where proof is unavailable; never claim that `Expr<i64>` equality makes a query valid. Track current shared-window engine limitations until the engine resolves them.

## Select, annotations, named labels, and CTEs: automated named results

Existing annotations use `select(Model, computed...)` or named projection objects; there is no separate established `annotate()` method to reproduce.

Tuples provide a simple strongly typed projection. The user selected automation: named macros should generate one local generic result definition per projection site, e.g. a row with `.user` and `.post_count`, with field types inferred from expression traits. This is separate from generating model structs for loaded combinations.

Prototype a named projection macro before claiming support for all combinations: model-valued selections, nullable expressions, `.require()`, prepared parameters, CTE column reuse, recursion, duplicate labels, and public named result types across function/module boundaries. Explicit result structs remain an alternative; named anonymous Rust types are not a language feature.

Runtime label strings cannot add Rust fields. Choose literal identifier labels/macros, declared label/result structs, or typed key access. Keep canonical relation caches and named prefetched members separate. A typed named relation view can expose base fields through an immutable view, but its construction and collision rules require a proof of concept.

## Writes and input guarantees

Generate insert inputs where required fields are mandatory and optional/defaulted fields have a deliberate omission representation. For updates, distinguish `Unchanged`, `Set(value)`, SQL expressions, and explicit nullable values. `Option<Option<T>>` is possible but a named assignment enum is clearer.

Statically reject stored/default-only/computed fields in invalid inputs, expression assignments for native-validated fields, incompatible relation targets, and specifying a relation object plus its FK. Typed unique/index tokens can cover composite unique constraints even though composite primary keys are not currently a completed engine feature.

Generate conflict actions with distinct types: do-nothing versus update, with returning output reflecting skipped rows. Use schema unique/partial-index tokens instead of arbitrary column vectors for the fully typed path. Dynamic conflict predicates still require planner/database validation.

Separate COPY from ordinary batching/upsert so unsupported combinations have no methods. Runtime sets of omitted fields and duplicate keys still need validation. Copyable schema `Int` values do not prove a supplied integer fits every configured native column width; retain runtime range checks or introduce explicit refined input types.

Restrict query mutations to table-backed, unsliced, unjoined, unlocked states. Preserve returning contracts: queryset update/delete follow public selection; insert and bulk-update returning retain their existing complete-row behavior. Keep defaults, transformations, validation order, atomic batches, and input-order semantics.

Instance writes use private identity and bypass default filters as today. Permit/protection dependencies follow actual physical tables, including composition and M2M through models. Version conflicts remain runtime errors. `attach` exists only on composed child models and accepts only child-local fields.

## Database context, transactions, and scoped capabilities: Q3, Q12

A configured default database supports direct `User.objects` terminals. Explicit typed database/transaction handles remain available; `.using` binds them for later terminals/awaiting. Operations with static transaction requirements must use a compatible bound handle. A configured URL is still a runtime value; typed connection constructors verify its dialect and prepare the expected schema/profile.

Represent a transaction as a borrowed active handle. Row locks and transaction advisory locks execute only through compatible active transaction contexts; session locks use a separate scoped lease. Lifetimes prevent ordinary escaping handles, but runtime lock acquisition and completion/cancellation still need management.

Transactions/savepoints commit on success, roll back on error, preserve outer callback ordering, and do not automatically retry. After-commit callback failure must report that commit already happened. Provide explicit cancellation/drop cleanup guarantees consistent with the current engine, which discards unfinished transaction connections rather than returning them to the pool.

Protected writes and schema scope requirements are typed capabilities on contexts where dependencies are statically known. They are application checks, not database security. A deliberately acquired permit does not create database permissions. Keep runtime checks for dynamic models/physical owners and raw SQL bypass semantics.

Scope values required by target default filters, nested prefetches, mutation returning, and prepared execution must be included. Conflicting uses of the same scope key with incompatible expected types fail generation where inferable; dynamic policy behavior requires a checked path. Tenant/RLS configuration remains a PostgreSQL context feature; actual RLS enforcement is runtime/database behavior.

Replicas, primary views, close, pool limits, raw SQL, and tenant contexts retain current behavior. Transaction reads use primary; replica reads do not provide freshness guarantees. Retain the originating database for compatibility in runtime loading, but never retain an ambient transaction that could outlive its scope. The statically checked transaction path requires an explicit compatible handle (Q3).

## Awaiting, caching, and custom query methods: Q13

Keep query construction lazy and immutable in meaning. Rust builder methods may consume an owned builder; explicit clone preserves reusable queries. Prefer reusable expression templates plus fresh terminal execution unless the user selects cache parity.

`IntoFuture` supports awaiting a bound query builder, but takes ownership. Repeatedly awaiting one variable is not TS/Python thenable caching. Expose an explicit reusable cached query object with shared read-only results, and keep scopes/database/transaction identity in its contract. Explicit fresh `all` remains available. Never silently share results across context changes.

Statements can be inspected before execution and consumed once for execution; this naturally prevents accidental double execution of the same owned statement. Prepared statements/templates intentionally support repeated execution. Rust async streams replace async iteration protocols.

Custom queryset methods use extension traits over compatible generic query states, with associated output types for methods that change state. Named filter methods preserve selection, requiredness, parameters, and relation labels. Test both method ordering directions and relation-query custom methods; do not reproduce the TS custom-subclass loss of loaded type arguments.

## Diagnostics, integration hooks, and tools

- `inspect()` uses existing engine planning and returns typed `QueryInspection`/`StatementInspection` with statement count, batching, join, correlated/uncorrelated, and CTE counts. Counts describe planned templates; they are computed at runtime even though the return structure is known statically.
- `sql()` returns debug SQL; values remain bound during actual execution. Parameter and scope obligations apply to diagnostics as well as queries.
- `explain` is async and contacts the server. PostgreSQL analyze executes; disallow incompatible locked queries by state and keep runtime checks for dynamic forms. SQLite analyze is unavailable on its typed interface.
- Query hooks, typed errors with SQLSTATE/constraint/detail, instrumentation, and N+1 scopes cover each executed statement, including batching/prefetch/raw SQL. Timing, shape counts, hooks, and tracing are runtime behavior.
- Raw SQL offers dynamic cells plus an explicit typed decoder. It cannot gain compile-time SQL checking without a separate parser/schema-validation feature; do not accidentally promise such a feature.
- Serialization includes only public loaded fields, not helper columns, and preserves nulls. Static JSON shape can still depend on the selected model state. Debug output for unloaded markers must not imply a value exists.
- `describe` supplies immutable generated schema metadata; factories use generated insert inputs. Field codecs adapt stored and exposed Rust types without treating native-computed values as SQL expressions.
- Reuse `Migrations`, `Migrator`, pull/baseline/drift, filesystem plans, and the existing CLI. Rust migration data callbacks are compiled functions, not runtime-imported source files. Preserve shared snapshot/history formats and document the packaging decision for compiled migration callbacks (Q14).

## Optional extensions and contracts: Q15

- Query defaults: generate default scalar/reference/order states and scope requirements; bypassing defaults updates those states correctly.
- Timestamps/soft delete: generate enabled operations only for appropriate models; keep engine transforms shared.
- Optimistic locking: instance operations require a loaded version, and the result version is refreshed. No static promise that it is current in the database.
- Native string validators/transforms/record validators: compile operation eligibility and field dependencies; evaluate actual values at runtime. Computed fields are read-only with private dependencies; static native methods are generated from existing declarations.
- Composition: flattened public fields and named parent references use the existing shape/planner; child-only attach inputs and physical-owner protections are generated. Do not substitute Rust inheritance.
- Proxy models: current narrowing is a warning contract and preserves NULL/enum values outside the hint. Rust cannot safely decode that into an ordinary non-null/narrow enum type. Preserve parity with honest physical types (`Option<T>` and parent enum) plus warnings, and offer an explicit checked conversion to a strict logical view. Do not silently introduce strict failure as the default.
- Generic relations: generate a concrete target enum when the allowed set is known, key/discriminator inputs, and exhaustive matching. Keep unknown/retired identities and target existence as runtime integrity errors. Mark routing/loading parity as dependent on the ongoing binding/engine milestone.
- File storage: codecs expose typed references/providers through existing build-selected contracts, retain async signed-url/open/upload behavior, and check operation/protection prerequisites before external work. Storage provider configuration and external I/O errors remain runtime. Distinguish existing interfaces from future lifecycle cleanup work.

Each compiler extension currently declaring Python/TypeScript language support must explicitly declare Rust support and provide a Rust codec/generation contribution before a Rust artifact accepts it. Missing support fails build time; a Rust implementation existing internally does not automatically imply a compatible public client interface.

## Dynamic fallback: Q16

Runtime-selected field lists and runtime mutation are accepted requirements. Use a clearly named opt-in runtime mode returning runtime-state models. Arbitrary runtime schema discovery remains a separate open scope question. No dynamic form claims compile-time loaded fields based on runtime strings.

Support dynamic filters/orderings/columns through validated tokens, not unsafe casts. A static query using a runtime choice can retain a common result shape where both branches agree; differing shapes need an enum, projection unification, or state erasure. Arbitrary dynamic schemas cannot expose newly named model structs.

Only a runtime-state field wrapper needs a loaded-state check during dereference/use. If panicking `Deref` is offered there, document it as a programmer-error check and provide a fallible alternative. The static path is infallible for field access after successful decoding. Never use uninitialized memory as a missing-value sentinel; safe state storage is the default.

## Decision register

User choices are distinguished from engineering defaults. Defaults are concrete design choices for this draft, not claims of user approval and not questions requiring answers. Retain the IDs for references elsewhere in the plan. Confirm feasibility with compiler probes; ask only if a probe exposes a meaningful conflict between field ergonomics, static guarantees, database semantics, or execution cost.

| ID | Area | Decision | Basis |
| --- | --- | --- | --- |
| Q1 | Instance loading | Typed rebinding and explicit runtime mutation mode | User chose both; names are engineering defaults |
| Q2 | Named projections | Automated macros plus tuple support | User chose automation |
| Q3 | Execution context | Default database supports `User.objects.first()`; explicit binding/transaction handles provide statically checked context requirements | User chose direct model queries; static transaction checks remain a goal |
| Q4 | Field mutability | Read-only model view; ordinary values for statically loaded fields | User chose read-only; model-level Deref is the proposed implementation |
| Q5 | Model/query/column values | `User.objects` or `User::objects`; model field/path values follow the chosen form; generic `User<S>` row type | User accepted both query-root forms; both naming arrangements verified |
| Q6 | Load arity | Single descriptor or tuple; `load_all` for all scalar fields | Stable Rust method arity |
| Q7 | Consuming load failure | Recover original row in a transition error where feasible; cancelling/dropping the consuming future drops its owned row | Ownership default; no false cancellation guarantee |
| Q8 | Invalidation/refresh | Downgrade invalidated relations statically; reload explicitly; skip-locked refresh returns an outcome enum | Existing invalidation semantics; no hidden extra fetch |
| Q9 | Required collections | Vec with an empty-result error; preserve nullable scalar distinctions | Existing terminal semantics |
| Q10 | Scalar subqueries | Proven single-column/at-most-one form plus explicit checked form when cardinality cannot be proved | Prefer static checks; retain database-checked expressiveness |
| Q11 | Named prefetch results | Automated literal-identifier labels/views | Follows user's automation choice |
| Q12 | Protection/scope | Typed explicit context capabilities | Prefer compile-time prerequisite checks |
| Q13 | Await/cache | Bound builders may implement IntoFuture; reusable cache is explicit and fresh `all` remains available | Preserve operation meanings within Rust ownership rules |
| Q14 | Data migrations | Compiled registered callbacks, same migration transaction | Reuse existing migration execution |
| Q15 | Proxy narrowing | Honest physical types and existing warnings; explicit strict conversion | Preserve values and Rust type soundness |
| Q16 | Dynamic inputs | Runtime field lists/mutation included; arbitrary dynamic schemas are later optional tooling work | User chose dynamic fields; prioritize generated client |
| Q17 | Dialect capabilities | Validate generation and typed execution requirements; runtime overrides still checked | Prefer early checks without removing runtime validation |
| Q18 | Rust conventions | snake_case terminals and explicit expression methods | Routine language adaptation |

## Feasibility gates before implementation

1. Compile a two-field ordinary-value model with static selection, immutable field view, additive load state, and inaccessible unloaded value methods.
2. Preserve shapes through default selection, all-scalars reset, nested nullable references, and collection `.cached`; test recursive schemas without infinite layouts/trait recursion.
3. Prototype named projection macros and CTE named columns across function boundaries; compare with explicit result structs if inference/diagnostics are poor.
4. Prove repeated prepared-parameter merging, distinct aliases of one model, nested outer scopes, and disallowed standalone execution on stable Rust.
5. Prove typed context capability propagation through prefetches, extension owner writes, transactions, and protected through-table writes.
6. Prove update/refresh invalidation and skip-locked outcomes do not preserve false loaded guarantees.
7. Measure generation size, trait-check time, incremental builds, release monomorphization, and emitted binary size on realistic many-field and cyclic-relation schemas. One source definition does not imply negligible compiler cost.
8. Verify direct shape constraints and optional automation accept supersets, distinguish queryable relations from loaded caches, support nested target requirements, and allow checked conversion from runtime-state rows.

Passing a gate validates that slice only. Failure should trigger an interface decision with the user, not an unsafe cast or a weakened guarantee hidden behind documentation.

## Delivery order

1. Resolve foundational questions and complete the feasibility gates in small temporary prototypes. Record approved syntax and guarantee boundaries here.
2. Add Rust generation, selected capability checks, typed connection/schema preparation, ordinary-value model shapes, metadata/codecs, basic expressions, and get/first/all/require.
3. Add exact defaults/partial selection, nested relation loading, typed instance transitions/prefetch, related sets, cache invalidation, and serialization.
4. Add projections/functions/aggregates, diagnostics, prepared parameters, aliases/subqueries, windows, CTEs, grouping, streams, and cursor pagination.
5. Add generated write inputs, insert/update/delete/bulk/COPY/upsert, returning, instance writes, transaction/lock/scope/protection interfaces, and custom query traits.
6. Add observability, raw SQL, hooks, migrations/CLI support, Rust support in selected extensions, and the agreed runtime-field fallback.

Each increment must include the corresponding valid examples and compile-fail cases. The delivery order does not authorize implementation in this planning task.

## Validation strategy and initial evidence

Use compile-pass/compile-fail consumer tests for invalid field access, read-only fields, wrong model/value, to-many ordering, unresolved parameters/outer scopes, window placement, scalar arity/cardinality, missing scope/transaction/protection, invalid mutation states, invalid inputs, and disabled extension methods. Assert useful diagnostic categories without freezing fragile full compiler messages.

Use shared-behavior database cases against PostgreSQL and SQLite for cardinality, NULL versus absent rows, same-row relation filters, selection helpers, defaults, nested loads, relation invalidation, skip-locked refresh, prepared execution, returning shapes, atomic batches, constraints, lock hashing, cross-language cursors, and extension behavior. Compile-time checks must not replace runtime integrity tests.

Source-level Rust probe completed with local rustc 1.97.0:

- Field-wrapper variant: direct method access and borrowing through `Deref` compiled and ran; consuming a row and returning a new field state compiled.
- The same probe rejected unloaded value method access and direct field assignment through an immutable model view.
- Assigning a field wrapper directly to `String` failed, confirming that wrappers do not provide value identity. This motivated ordinary loaded field types.
- Revised ordinary-value variant: `user.email` was verified as `String`, direct methods and cloning compiled and ran, and consuming/rebinding added a second ordinary `String` field.
- The ordinary-value probe rejected using an unloaded field's value, replacing a field, and calling a mutable String method through the immutable model view.
- The full client, query transforms, named macros, source identities, and invalidation guarantees have not been implemented or validated. These probes establish only the field representation and ownership mechanism.

Rust reference material: [Deref](https://doc.rust-lang.org/stable/std/ops/trait.Deref.html), [DerefMut](https://doc.rust-lang.org/std/ops/trait.DerefMut.html), [IntoFuture](https://doc.rust-lang.org/std/future/trait.IntoFuture.html), [generic monomorphization](https://doc.rust-lang.org/stable/book/ch10-01-syntax.html), and [MaybeUninit initialization requirements](https://doc.rust-lang.org/stable/std/mem/union.MaybeUninit.html).
