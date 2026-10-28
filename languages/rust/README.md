# Rust generated models, selection, and loading

`monorm-client` executes generated, typed queries directly through `monorm-engine`. It has no Python or Node dependency. Generation consumes the prepared schema offline; compilation does not verify deployed constraints or prove that rows exist.

Generate a consumer module with the CLI (the default CLI profile includes `generate-rust`):

```nu
cargo run -p monorm-cli -- --schema languages/rust/examples/schema.prisma generate rust -o models.rs
```

Applications in this repository can depend on `monorm-client` by path. The core, engine, and client packages remain unpublished. Backend features are `postgres` and `sqlite`; both are enabled by default. A generated schema has one dialect and brands its database handle, model sources, and field tokens. A URL with a different dialect is rejected at connection time. Driver availability and selected extension requirements remain runtime artifact checks.

```rust
mod models;
use models::{Database, User};

async fn readers(url: &str) -> monorm_client::Result<()> {
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
use monorm_client::{Loaded, Unloaded};
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
use monorm_client::{BatchLoad, Instance, LoadedMany};

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
let views = monorm_client::relation_views!(user, &db;
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

Failures use `monorm_client::Error`. Database failures remain `Error::Engine(monorm_engine::Error::Db(details))` with inspectable SQLSTATE, constraint, and detail. Empty required queries and multiple-row gets have distinct variants. Shape decoding stays synchronous after execution. Typed query and row states disappear at the operation IR/Params/planning seam; the client reuses the existing executor and drivers.

Consumer tests cover compiler guarantees and SQLite/PostgreSQL behavior. Tests use a disposable PostgreSQL database; an unavailable PostgreSQL connection is a failure rather than silently passing. Example reads expect tables to have already been created through the shared migration tooling. No new migration or raw SQL API is introduced in this ticket.

```nu
$env.ORM_RUST_TEST_POSTGRES_URL = "postgres://postgres:postgres@localhost/orm_test"
cargo test -p monorm-client
python3 languages/rust/bench/measure.py
```

`bench/measure.py` records generation time (including CLI startup), generated bytes, warm-dependency consumer checking, unchanged and edited incremental checks, release build time, and executable size for a wide schema and a cyclic schema. Each wide model is exercised in two loaded shapes; each cyclic model adds a third shape with a narrowed nested relation. Release timings include consumer code generation and linking; they are not an isolated measurement of monomorphization. Measurements are local observations with no acceptance threshold; see [results.json](bench/results.json) for ticket 01 and [selection-results.json](bench/selection-results.json) for ticket 02.

### Tuple, named and scalar projections

```rust,ignore
let rows: Vec<(String, Option<String>)> = User::objects
    .select((User::email, User::nickname))
    .using(&db)
    .await?;
let contact = monorm_client::select_named!(User::objects;
    email => User::email,
    nickname => User::nickname,
).require().using(&db).first().await?;

// Export the result definition when a function or module needs to name it.
monorm_client::named_result! { pub struct Contact { email, nickname } }
let contact: Contact<String, Option<String>> = monorm_client::select_named!(
    User::objects => Contact; email => User::email, nickname => User::nickname,
).require().using(&db).first().await?;

// Missing row = None; SQL NULL = Some(None).
let nickname: Option<Option<String>> = User::objects
    .select(User::nickname).using(&db).scalar().await?;
// Requiredness rejects missing rows, while preserving SQL NULL.
let nickname: Option<String> = User::objects
    .require().select(User::nickname).using(&db).scalar().await?;
```

Projections accept one item or tuples of up to eight items. `all`/await return
rows as tuples; `scalar`/`scalars` require exactly one logical item. Required
collections reject empty results and retain nullable elements. Both explicitly
bound and configured-default execution are available. Builders are owned and
lazy; clone explicitly for fresh executions. Planned projection statements are
consumed by execution, and support the existing planner's debug SQL and inspection.

`monorm_client::model::<User>()` is a tuple item producing an ordinary model that
satisfies its generated `UserShape` bounds. A scalar item after a model item
still refers to its correct SQL column. Projection result definitions infer the
model's generic shape and accept ordinary direct associated-type bounds.
As in the shared planner, SQL projections cannot combine with relation loading;
model queries remain the way to return loaded relation caches. Named projections
currently name Rust result fields; named SQL CTE columns and reusable prepared
projections are still pending ticket 03 work.

Use `.column().lower()`, `.upper()` or `.length()` on generated scalar descriptors
for SQL text annotations in `select` or `select_named!`. Text functions and
projections through optional to-one paths retain SQL NULL, without nesting
`Option` for a field that was already nullable. To-many paths remain unavailable
as projection items.

Parameter-free model and projection templates support `.prepare()`:

```rust,ignore
let template = User::objects.select(User::nickname).prepare().require();
let first: Option<String> = template.using(&db).scalar().await?;
let again: Vec<Option<String>> = template.using(&db).scalars().await?;
```

Each execution plans again against the supplied database and reads fresh values;
the template stores no result cache. Typed placeholders and binding obligations
remain pending. These templates preserve generated shape bounds, named result
types, and nullable/required result inference. Compiler-cost observations for the
implemented projection forms are in `bench/projection-results.json`, measured on
a loaded machine; `bench/projections.py` reproduces warm-dependency type checks.

### Correlated scalar subqueries

`scalar(|a: Parent<Author>| ...)` builds `(SELECT one_column ...)` as a projection item.
The closure receives a handle for the enclosing query; `a.col(Author::id.column())` is an outer column,
and `Book::author_id.column().eq_outer(..)` compares with it.
Each `scalar` call has its own handle, so nested levels, even over one model, resolve to distinct outer depths.
A predicate that reads an outer column has the `Correlated` type: `Query::filter` and prepared templates reject it,
and a `Subquery` has no terminals. Only `scalar` or `scalar_checked` embeds it.
`scalar` needs a proven single row (`limit_one()` or `unique(Key, outer)`).
`scalar_checked` takes any row count and leaves a second row to the database:
PostgreSQL fails, SQLite reads the first row. The result is `Option<T>`, `None` when no row matches.
A subquery filter cannot hold placeholders.
The `None` stays under a required outer terminal: `require()` removes only absent outer rows.

### `in` and `exists` subqueries

`exists(|u: Parent<User>| Subquery::new(Post::objects).filter(..))` and
`in_subquery(User::id.column(), |u: Parent<User>| Subquery::new(..).select(Post::author_id))` return an ordinary
predicate for the outer model, so `filter`, `and`, `or` and `!` accept it. `not_in_subquery` is `NOT IN`.
The subquery column must have the operand's type; either side may be `Option`.
A subquery may be uncorrelated, or read its parent or an enclosing parent (nested closures).
An outer handle that leaves its closure is a compile error;
a predicate built for one subquery and used in another fails at execution, preparation or `statement()`
with `Error::Compatibility` (unresolved outer source).
