# Rust generated models, selection, and loading

`orm-client` executes generated, typed queries directly through `orm-engine`. It has no Python or Node dependency. Generation consumes the prepared schema offline; compilation does not verify deployed constraints or prove that rows exist.

Generate a consumer module with the CLI (the default CLI profile includes `generate-rust`):

```nu
cargo run -p orm-cli -- --schema client/examples/schema.prisma generate rust -o models.rs
```

Applications in this repository can depend on `orm-client` by path. The core, engine, and client packages remain unpublished. Backend features are `postgres` and `sqlite`; both are enabled by default. A generated schema has one dialect and brands its database handle, model sources, and field tokens. A URL with a different dialect is rejected at connection time. Driver availability and selected extension requirements remain runtime artifact checks.

```rust
mod models;
use models::{Database, User};

async fn readers(url: &str) -> orm_client::Result<()> {
    let db = Database::connect(url, 4).await?;
    let query = User::objects
        .filter(User::r#type.eq("reader".into()))
        .order_by(User::id.asc());
    let users = query.clone().using(&db).await?;
    for user in users {
        let email: &String = &user.email;
        let copied = user.email.clone();
        let nullable: &Option<String> = &user.nickname;
        println!("{email} {copied} {nullable:?}");
    }
    let user = query.require().using(&db).get(User::ID_KEY, 1).await?;
    println!("{}", user.email);
    db.close().await;
    Ok(())
}
```

Configure a default for this generated schema to use direct terminals:

```rust
db.set_default();
let user: Option<User> = User::objects.first().await?;
let readers = User::objects.filter(User::email.eq("reader@example.com".into())).await?;
Database::clear_default();
```

Defaults are configured per generated schema identity and resolved when execution starts. Missing configuration returns a compatibility error. Explicit `using(&db)` bypasses the default. Clear the configured default before closing its driver. No transaction is retained as a default.

Builders are lazy, owned transformations. Clone before reusing one. `using` binds a compatible live handle; awaiting the bound builder consumes it and reads a vector. Ordinary field access is synchronous and never performs I/O.

A generated model has one generic definition with a state per scalar field and relation in schema order. The normal shape follows the prepared schema’s default scalar and reference selection; schemas without selection policies load all scalars and leave relation caches unloaded. For the example schema, an explicit email-only query is:

```rust
use orm_client::{Loaded, Unloaded};
type EmailOnly = User<Unloaded, Loaded, Unloaded, Unloaded>;
let row = EmailOnly::query().using(&db).get(User::ID_KEY, 1).await?;
if let Some(row) = row {
    let email: &String = &row.email;
    let marker = row.id; // Moving an unloaded marker is legal.
    println!("{email}");
}
```

Loaded fields are ordinary Rust values, including `Option<T>` for SQL NULL. An immutable model-level `Deref` exposes their field syntax. Borrow or clone non-Copy values; moving a String out of this view, mutating a field, or invoking its mutable methods fails compilation. Unloaded markers have no underlying-value display, dereference, or conversion. Models own private data and identity; callers cannot construct a model or a state implementation. Equality and hashing use logical model source identity and the stored primary key across loaded shapes. Different logical model names remain distinct even when mapped to the same table.

Partial reads ask the shared planner for public fields and private helpers separately. A primary key or unique lookup helper does not become a publicly loaded value. The generic `query()` entry point remains available. The inferred associated root and descriptors are the normal selection API:

```rust
use models::{Post, User};
use orm_client::{BatchLoad, Instance, LoadedMany};

// db.set_default() enables execution without an explicit using(&db).
db.set_default();
let user = User::objects
    .load((User::email, User::posts.load(Post::title)))
    .require().first().await?;
let email: &String = &user.email;
let titles: Vec<&String> = user.posts.cached().iter().map(|post| &post.title).collect();
let user = user.load(User::nickname, &db).await.map_err(|error| error.error)?;
let users = User::objects.load(User::email).using(&db).all().await?;
let users = users.prefetch(User::posts.load(Post::title), &db).await
    .map_err(|error| error.error)?;
```

The first explicit root scalar descriptor replaces default scalars, even after relation-only loads. Later scalar loads add fields. Relation-only loads retain scalars; `all_scalars()` resets the scalar selection and keeps relations, filters, and order. `without_related()` removes default references while retaining explicit references. `without_defaults()` removes default policy and default references, expands an implicit root scalar selection, and preserves explicit selection, filters, order, and scope. Bare targets also widen to all scalars and lose their default references when policy is bypassed, in either method order. Explicit nested target selections retain their scalar selection. Bare target descriptors request their normal shape; `as_prefetch(query)` selects a filtered or limited target shape. Target filters can hide a required physical reference, so `LoadedOne<T>::cached()` always returns `Option<&T>`.

Queryable unloaded relations expose `row.posts.query()` without exposing a cached() method. Loaded relations add typed `cached()` access and retain query(). Reference caches own boxed targets, and collection caches own vectors. Requested recursion is finite; the schema’s cycles do not expand the default Rust type forever. Loading an existing canonical cache unions requested target fields and preserves its existing members, NULLs, and empty state. Use `reload(descriptor, &db)` to replace requested values or caches. `load_all_scalars(&db)` is the distinct consuming instance operation. If requested static data already exists, single/pair/triple descriptors can rebind without a database read.

`load`, `reload`, and batched `prefetch` return `TransitionError<T>` on ordinary errors; `*error.original` recovers the input and `error.error` describes the failure. A consuming future owns its input: dropping it drops that row or collection. Cancellation cannot return ownership. These transitions perform reads; they do not promise rollback of writes after later decoding errors. Mutable runtime loading leaves the original runtime row intact on error.

Generated associated-type shape traits work across minimal and fuller selections:

```rust
use models::{PostShape, UserShape};
fn email<S: UserShape<Email = String>>(row: &S) -> &String { row.email() }
fn titles<'a, S, P>(row: &'a S) -> Vec<&'a String>
where S: UserShape<Posts = LoadedMany<P>>, P: PostShape<Title = String> + 'a {
    row.posts().cached().iter().map(|post| post.title()).collect()
}
```

No additional HasEmail/HasTitle traits are needed. A queryable relation without a cache cannot satisfy the `LoadedMany<P>` requirement. Shape traits expose getters for generic consumers; concrete rows retain ordinary immutable field syntax.

Literal named relation views use reusable macro machinery and keep named views separate from canonical caches:

```rust
let views = orm_client::relation_views!(user, &db;
    recent => User::posts.as_prefetch(Post::objects.load(Post::title)
        .order_by(Post::id.desc()).limit(2)),
    first_post => User::posts.label("first_post")
        .as_prefetch(Post::objects.load(Post::title).order_by(Post::id.desc())).one()
).await.map_err(|error| error.error)?;
let recent = views.recent.cached();
let first = views.first_post.cached(); // Option<&Post<...>>
let original_cache = views.row.posts.cached();
```

These local generic field views have no mutable dereference. Scalar labels, wrong roots, and incompatible targets fail compilation. Repeated literal identifiers fail compilation; schema field, canonical cache, helper, and duplicate label collisions fail validation before execution. Label strings alone never produce Rust fields. The macro’s named-result wrapper is reusable for ticket 03; exported projection and CTE definitions remain that ticket’s work.

Runtime-selected fields always return `RuntimeRow<User>` for this model, regardless of the lists or static shape erased:

```rust
let mut row = User::objects.runtime(&["email", "posts.title"])?
    .filter_runtime("email", "a@example.com".to_owned())?
    .order_runtime("id", false)?.require().using(&db).first().await?;
let email: String = row.get("email")?;
row.load_runtime(&["nickname"], &db, false).await?;
let row = row.try_into_static::<EmailOnly>().map_err(|error| error.error)?;
```

`get<T>` is fallible for unloaded values and codec mismatches. `is_loaded` distinguishes missing data from loaded NULL or an empty cache; `related::<Target>(name)` provides runtime target rows. There is no panicking runtime Deref. `into_runtime()` retains actual public values, private identity/helpers, and loaded flags. Checked conversion validates public loaded flags, codecs, and every nested cache requirement without fetching. Dynamic strings cannot generate named fields or satisfy shape bounds directly. Differing static branches require explicit erasure, a common checked shape, or an enum; erased runtime rows have one fixed type. Arbitrary dynamic schema registries and hybrid static/dynamic field optimization remain separate work.

Generated rows and runtime rows implement serde serialization of public loaded data, preserving SQL NULL and loaded-empty caches. Private helper keys and unloaded markers are omitted. Debug output reports actual loaded data. Codec-view values and owned stored values currently coexist in static rows; this uses additional memory and makes state validation safe without unsafe casts.

`User::objects` is an allocation-free `Query` constant. Custom query extensions begin directly at this root and should implement their methods for `Query<M, R, L, D>` to preserve scalar-selection policy as well as requiredness and slicing. The configured default is a schema-branded pool, not an ambient transaction; `Database::clear_default()` removes it. Explicit `using(&db)` remains available on every query.

| Operation | Contract |
| --- | --- |
| `filter`, `exclude` | Typed root predicates; repeated calls preserve separate relation-filter groups. |
| `Predicate::and`, `or`, `!predicate` | Boolean grouping within a filter call. |
| Field `eq`, `ne`, `lt`, `le`, `gt`, `ge`, `eq_field`, `in_values` | Exact value/source types; nullable equality with None uses IS NULL. |
| `User::posts.path().views()` | Relation-path field tokens retain the root and to-many cardinality through further hops. |
| `order_by`, `then_by` | Replace ordering, then append tie breakers. To-many paths cannot be ordered. |
| `limit`, `offset`, `slice` | Nonnegative u64 counts, checked signed database ranges; slices compose offsets and limits. Reversed ranges are empty. |
| Queryset `or` | Unsliced same-model/same-shape queries, at most one filter call each; the right supplies filters only. The left retains ordering. |
| `all` / bound awaiting | Vec of rows, with fresh execution each time. |
| `get(unique_token, value)` | Optional row; always checks multiple rows even when the deployed constraint differs from the generated schema. Scalar and composite unique tokens are generated; composite tokens carry an exact tuple input type without an arity limit. |
| `first`, `last` | Optional row in the requested order, primary-key order when unspecified. |
| `require` | Row terminals return a row and collections reject emptiness; count/exists still return zero/false. |
| `count`, `exists` | Shared engine operations retain sliced-query behavior. |
| `in_bulk(key, ids, &db)` | Unsliced typed unique-key lookup returning a HashMap, with parameter budgeting and private key helpers. |
| `statement` | A planned, inspectable statement; `sql` and `inspect` do no I/O, `execute` consumes it once. |

The terminal `get` takes a generated key and a matching typed value, e.g. a tuple for a composite constraint. Compilation verifies these types, while execution still verifies cardinality. Partial/expression indexes do not claim global uniqueness and produce no basic-read lookup token. Later write/conflict tokens are a separate surface.

Enums use PascalCase Rust variants and preserve physical labels or integer mappings. Fields use snake_case and raw identifiers such as `r#type`. Generation rejects collisions with normalized fields, tokens, enum variants, and private helpers. Selected behavioral extensions must declare Rust support; custom language codecs require their compiled contributions rather than accepting arbitrary hint strings.

Failures use `orm_client::Error`. Database failures remain `Error::Engine(orm_engine::Error::Db(details))` with inspectable SQLSTATE, constraint, and detail. Empty required queries and multiple-row gets have distinct variants. Shape decoding stays synchronous after execution. Typed query and row states disappear at the operation IR/Params/planning seam; the client reuses the existing executor and drivers.

Consumer tests cover compiler guarantees and SQLite/PostgreSQL behavior. Tests use a disposable PostgreSQL database; an unavailable PostgreSQL connection is a failure rather than silently passing. Example reads expect tables to have already been created through the shared migration tooling. No new migration or raw SQL API is introduced in this ticket.

```nu
$env.ORM_RUST_TEST_POSTGRES_URL = "postgres://postgres:postgres@localhost/orm_test"
cargo test -p orm-client
python3 client/bench/measure.py
```

`bench/measure.py` records generation time (including CLI startup), generated bytes, warm-dependency consumer checking, unchanged and edited incremental checks, release build time, and executable size for a wide schema and a cyclic schema. Each wide model is exercised in two loaded shapes; each cyclic model adds a third shape with a narrowed nested relation. Release timings include consumer code generation and linking; they are not an isolated measurement of monomorphization. Measurements are local observations with no acceptance threshold; see [results.json](bench/results.json) for ticket 01 and [selection-results.json](bench/selection-results.json) for ticket 02.
