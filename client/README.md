# Rust generated models and basic reads

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
        .filter(User::TYPE.eq("reader".into()))
        .order_by(User::ID.asc());
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

`User::objects` is an associated lazy query constant with the default scalar shape; it needs no parentheses or generic arguments. Each access creates an independent query. Snake_case associated columns, such as `User::email`, preserve instance syntax `user.email`; the uppercase column aliases remain available. Relation path builders remain `User::posts()` until the loading surface is added by ticket 02.

Defaults are configured per generated schema identity, checked at connection time, and resolved when execution starts. Missing configuration returns a compatibility error. Explicit `using(&db)` bypasses the default. Changing the default affects subsequent executions; an executing query holds its own compatible handle. Clear the configured default before closing its driver. No transaction is retained as a default.

Generated associated-type shape traits let consumers accept minimal and fuller loaded shapes through the same interface:

```rust
use models::UserShape;
fn email<S: UserShape<Email = String>>(row: &S) -> &String {
    row.email()
}
struct Reader<S: UserShape<Email = String>> { row: S }
```

Each instantiated generic model implements its shape trait. Scalar associated types are their actual exposed types, including `Option<T>` and `Unloaded`. Relation associated types are `Unloaded` in this ticket; ticket 02 adds queryable relation values, loaded caches, and nested target shape bounds. Concrete model fields retain ordinary field syntax; generic shape-bound consumers use immutable accessors. Separate per-field capability traits are unnecessary.

Builders are lazy, owned transformations. Clone before reusing one. `using` binds a compatible live handle; awaiting the bound builder consumes it and reads a vector. Ordinary field access is synchronous and never performs I/O.

A generated model has one generic definition with one `Loaded`/`Unloaded` state per scalar field in schema order. The default shape loads all scalar fields. For the example schema, an explicit email-only query is:

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

Partial reads ask the shared planner for public fields and private helpers separately. A primary key or unique lookup helper does not become a publicly loaded value. The explicit generic shape entry point is a foundation for ticket 02's selection/loading API; default selection policies, nested reference values, and runtime field mode are delivered there.

| Operation | Contract |
| --- | --- |
| `filter`, `exclude` | Typed root predicates; repeated calls preserve separate relation-filter groups. |
| `Predicate::and`, `or`, `!predicate` | Boolean grouping within a filter call. |
| Field `eq`, `ne`, `lt`, `le`, `gt`, `ge`, `eq_field`, `in_values` | Exact value/source types; nullable equality with None uses IS NULL. |
| `User::posts().views()` | Relation-path field tokens retain the root and to-many cardinality through further hops. |
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

`bench/measure.py` records generation time (including CLI startup), generated bytes, warm-dependency consumer checking, unchanged and edited incremental checks, release build time, and executable size for a wide schema and a cyclic schema. Each model is exercised in two loaded shapes. Release timings include consumer code generation and linking; they are not an isolated measurement of monomorphization. Measurements are local observations with no acceptance threshold; see [results.json](bench/results.json).
