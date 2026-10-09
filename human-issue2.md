# 1. Merge conflict handling into one call
- Replace the Python chain with `on_conflict(*columns, update=True, updates=[User.name])` or `on_conflict(*columns, update=False)`; require keyword-only `update`.
- Columns identify one unique constraint; support composite targets, unpacked lists and partial-index predicates. Omitting `.on_conflict()` retains raise behavior.
- Allow `updates` only with `update=True`; omitting it keeps existing default update behavior. Challenge empty list/mapping semantics before implementing.
- Proposal (needs `grilling` before implementation): fold `update_fields` and `update_values` into one `updates` parameter accepting a column list or a field-name mapping. `updates=[Post.title, Post.body]` copies incoming values for those columns; a mapping accepts literals and expressions. Either mode limits updates to the specified fields.
- In a mapping, use the uncalled `orm.excluded` function as shorthand for the incoming value of the key's column: `updates={"title": orm.excluded, "published": True, "views": Post.views + orm.excluded(Post.views)}`. Thus `{"title": orm.excluded}` is equivalent to `{"title": orm.excluded(Post.title)}`; inside expressions, keep the explicit column argument. Settle typing, validation and the TypeScript equivalent before implementation.

# 2. Explicit insert results
- Match `update()`: inserts return affected-row counts; `.returning()` requests inserted/updated rows, excluding skipped conflicts.
- Single-row `.returning()` yields `Model | None` for `update=False`, otherwise `Model`; bulk yields `list[Model]`. Type literal booleans precisely and dynamic booleans conservatively.
- Preserve returned-row ordering; migrate existing callers, examples and runtime/typing tests in Python and TypeScript.

# 3. Batch existing test setup
- Prefer bulk setup/cleanup; retain individual operations when behavior under test or dependencies require them.
- Use TypeScript `await Promise.all(...)` for independent setup when faster; respect transaction/connection constraints and verify isolation with affected tests.

# 4. Aggregate options and ArrayAgg
- Add typed `func.array_agg()` / `func.arrayAgg()` with per-aggregate filtering, input ordering and distinct options; extend existing aggregates where applicable.
- Preserve current distinct support; document/test NULL and empty results, relation aggregates and database capability restrictions.

# 5. Model-returning `load()`
- Rename/extend `only()` to `load()`: accept columns, nested relation paths and relation querysets with filtering, ordering and per-parent slicing; relation-only loads select all model fields.
- Relation-queryset `.label("top_posts")` sets a separate attribute; unlabeled results fill the relation cache. `Post.author.name` loads a partial author accessible through `post.author.name`.
- Join to-one and prefetch to-many automatically; `.as_prefetch()` only changes strategy. Keep `select()` returning Rows; preserve identity/link keys and reject alias collisions.
- Implementer: use `grilling` before coding to settle partial-field behavior, nested alias ownership, typing and migration from existing loading APIs; document/test the agreed contract.

# 6. Lock during instance refresh
- Add `qs.lock()` options (`exclusive`, `nowait`, `skip_locked`) to `refresh()`, retaining field selection and unlocked refresh by default; locking requires a transaction.
- Implementer: use `grilling` before coding to settle lock activation, skipped/missing rows and partial refresh; keep queryset validation/errors consistent and test contention.

# 7. Compare relations with instances
- Make `filter(Post.author == alice)` equivalent to `filter(Post.author_id == alice.id)` without an extra join; add the typed TypeScript equivalent.

# 8. Combine aggregates over the same relation
- `tests/test_queries.py::test_select_model_with_relation_aggregates` computes post count and views sum in separate correlated subqueries; consider one grouped subquery join or lateral aggregate join to avoid repeated scans.
- Combine compatible aggregates only; preserve independent-relation results without row multiplication, empty counts (`0`), empty sums (`None`) and per-aggregate filter/distinct semantics.
- Benchmark against the current plan for selective and broad parent queries before choosing a strategy; add SQL-shape and correctness coverage with database capability checks.
- Nice-to-have if hard or risky: reuse an aggregate appearing in both `select()` and `filter()` through a derived table, CTE or lateral binding instead of emitting duplicate subqueries; preserve filter semantics and benchmark before enabling.

# 9. Scoped query hooks for tests
- Keep persistent hook registration: `db.on_query(hook)` currently supports long-lived logging and backs `orm.otel.instrument(db)`, whose returned function stops instrumentation.
- Add a context-manager form for temporary observation in tests, unregistering on exit even when the block raises. Settle whether this is a separate scoped method or a subscription handle supporting both explicit removal and context-manager use; preserve persistent instrumentation behavior.

# 10. ORM query inspection for test assertions
- Implementation: `f81f4cefc1d6b0f20e8a1853aa9b5c4b7f1ece59` (Python `inspect()` API, shared Rust planner inspection, and updated runtime/typing tests). Status: implemented; independent review and performance testing requested.
- Review requested: review this commit against item 10, especially enclosing-scope correlation in implicit filters/aggregates, LATERAL/CTE/UNION nesting, planned templates versus executed statements, batching, PostgreSQL/SQLite differences, and composed writes. Confirm ordinary execution collects no inspection metadata and existing SQL-rendering, correctness, and N+1 coverage remains intact.
- Performance testing requested: compare this commit with its parent `fa668ccbd5c887419c0fb0c395d08aa25ccf662b` using the same data and build profile. Measure ordinary planning/execution without `inspect()` for regressions; measure `inspect()` latency and allocations separately for simple queries, nested relation filters, shared aggregates, prefetches, and bulk writes on PostgreSQL and SQLite. Report benchmark setup and results; use statement events for actual execution counts and `explain()` for physical database strategy. Performance results are pending.
- Validation completed: default Python suite 407 passed / 38 skipped; composed-profile Python suite 410 passed / 35 skipped; all-features Python suite 441 passed / 4 skipped. Rust workspace tests, Node/Bun tests, type checks, lint, and model-composition/all-features artifact checks passed.
- TypeScript implementation: synchronous `inspect()` on query sets, selected/prepared queries, inserts/upserts and query-set mutations; deeply frozen camelCase result interfaces; Node binding reuses the shared Rust inspector. Query-set mutations now execute once when awaited, preserving their creation-time async context. Runtime/typing coverage includes PostgreSQL/SQLite, skipped/repeated prefetch templates, bulk writes and composed writes. Default Node suite: 287 passed / 31 skipped; all-features Node: 315 passed / 3 skipped; default Bun: 283 passed / 32 skipped; all-features Bun: 311 passed / 4 skipped. Type checks, Rust engine/Node tests and Node-binding Clippy passed. Performance measurements remain pending.
- Add an on-demand `query.inspect()` returning a typed planner result, separate from the existing database `explain()`. Keep collection disabled during normal execution; reuse existing statement events for actual execution observation rather than requiring OpenTelemetry.
- Keep the result small: `statement_count`, `has_batching`, and `statements`; each statement exposes `has_batching`, `joins_count`, and `subqueries.correlated_count` / `subqueries.uncorrelated_count`. No read/write subdivision or subquery counters by SELECT/filter/order-by location.
- Determine correlation from references to enclosing query columns in the generated query structure, including implicit relation aggregates/filters, not just explicit `outer()` calls. LATERAL subqueries can be correlated; JOIN/FROM/CTE placement alone does not determine correlation. Count each nested subquery once by its own enclosing-scope dependencies.
- `statement_count` counts planned statement templates; actual execution counts must come from observation because batching and parent results can change the number sent. N+1 reports deliberately collapse some intentional batching and must not supply raw execution counts. Correlation describes dependency, not a guarantee of per-row database execution; use `explain()` for physical strategy.
- Example assertions: `assert result.statement_count == 1`; `assert not result.has_batching`; `assert result.statements[0].joins_count == 1`; `assert result.statements[0].subqueries.correlated_count == 1`; `assert result.statements[0].subqueries.uncorrelated_count == 0`.
- Update existing tests to use `inspect()` where query structure is part of the behavior: relation loading (joins versus prefetch statements), same-relation aggregate sharing, correlated versus uncorrelated subqueries, and read/write batching. Replace brittle SQL substring/count assertions with inspection assertions where they express the same contract; retain exact SQL tests for SQL rendering and result assertions for correctness. Respect PostgreSQL/SQLite planner differences. Use execution observation separately when the test must verify actual statements sent, including empty-parent and multiple-batch cases; retain N+1 tests for repeated-query detection.

# 11. Capture executed queries for test assertions
- Add `with orm.capture_queries() as captured:` around task execution. Expose `captured.queries` in execution order, one entry per actual database statement, including each prefetch query and batch; do not collapse intentional batching as the N+1 finder does.
- Each entry provides existing event details (`sql`, `start`, `duration`, `rows`, `error`) and `.inspect()` for that executed statement's planner metadata, using the same statement inspection type as `query.inspect().statements`. Capture metadata only while enabled and reuse the existing statement-event infrastructure; no OpenTelemetry dependency.
- Capture executed-statement records, not original query objects: one ORM operation can produce several statements. For raw SQL without planner metadata, report inspection as unavailable rather than fabricated zero counts. Settle nested scopes, concurrent task isolation and ordering before implementation.
- Example:

```python
with orm.capture_queries() as captured:
    await do_the_task()

assert len(captured.queries) == 2
assert captured.queries[0].inspect().joins_count == 2
assert captured.queries[0].inspect().subqueries.correlated_count == 1
```
