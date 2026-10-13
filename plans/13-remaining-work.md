# Remaining work from the suggestion groups, human-issue2 and the merge reviews

Status: open work on `main` at 2026-10-13.
Every feature branch of suggestion groups 1-7 and of human-issue2 is merged.
This file lists only what is not built, and the reviews that are not done.

The specs, plans and review files live outside the repo, in `~/projects/worktrees/orm/plans/`.
Each spec file ends with a section `## Status on main (2026-10-13)` that gives the evidence for each item.

| Spec | Spec file | Items done / partly / not done |
|---|---|---|
| 1 Observability | `suggestions/1-observability.md` | 4 / 0 / 0 |
| 2 Expressions | `suggestions/2-expressions.md` | 4 / 0 / 0 |
| 3 Writes | `suggestions/3-writes.md` | 5 / 0 / 0 |
| 4 Query API | `suggestions/4-query-api.md` | 4 / 1 / 0 |
| 5 Schema extensions | `suggestions/5-schema-extensions.md` | 2 / 1 / 0 |
| 6 Connections | `suggestions/6-connections.md` | 4 / 0 / 0 |
| 7 Migrations | `suggestions/7-migrations.md` | 2 / 0 / 0 |
| human-issue2 | `human-issue2/ISSUES.md` | 6 / 1 / 1 |

## Build

1. **Review fixes for suggest-2, FILTER level** (blockers 5-6 of `merge-reviews/suggest-2.review-triage.md`): `filter-relation-local-agg`, `filter-outer-only`, `filter-to-one-under-rel`.
   Follow Django: a filter on a relation gives a per-row aggregate over that relation, with no error.
2. **Review fixes for suggest-2, other Agree items** (7-27 of the same file): should-fix items, spec drift, docs and nits.
3. **`group_by(case)` and JSON paths in `group_by`, `having`, `distinct`** (fx-case-group-by): Postgres gives error 42803.
   Use one bind for each copy of the expression.
   Test HAVING, ORDER BY, DISTINCT and JSON paths.
4. **Batch the existing test setup** (human-issue2 item 3): `human-issue2/tests.prompt.md`.
   Do it after the other branches merge.
5. **Soft-delete cascade in application mode** (spec 5, S5.3): several statements in one transaction.
6. **Native code generation with a query-set argument** (spec 4, F4): `generate_python` and `generateTypescript` in the bindings.
7. **Docs that name removed APIs** (human-issue2 item 5): `docs/model-composition.md:42` (`select_related`), `docs/proxy-models.md:99` and `docs/selection-and-defaults.md:28` (`only()`).
8. **Optional:** aggregates in `select` and `filter()` share one LATERAL join (human-issue2 item 8, Q2.8.2).
9. **Declarative partitioning** ([plan 11](11-partitioning-and-composite-keys.md), tickets 3 and 4; "PR B" in `composite-keys/plan.md`): `@@partition` and `partition` blocks, then `@@partition.values` on proxies.
   Composite primary and foreign keys (tickets 1 and 2) are done: `82f8a4c`, `671f567`.
   Open question first: identity columns on a partitioned table need Postgres 17 by the spec; check Postgres 16, then set the floor or emit a sequence default.

## Review

Each review uses `merge-reviews/rt-<name>.prompt.md` (review and triage only, no code).
Then `merge-reviews/fx-<name>.prompt.md` applies the Agree group, each fix with a regression test that fails without the fix.

| Review | State |
|---|---|
| suggest-1 | Review file has 16 items; no triage, no fixes. Blocker `hooks-in-finally`: `python/orm/db.py:152` runs the hooks in `finally`. |
| suggest-4 | Not started. |
| suggest-5 | Not finished: pipeline map only. Two seams to check: `on_conflict` updates skip `maintained()`; composed-model writes skip soft delete and `maintained()`. |
| h2-writes, h2-instances, h2-load, h2-aggregates | Not started. |

## Decisions for the owner

The list with files is in `human-todo.md` at the repo root.
The most urgent: a caught failure inside `transaction()` commits nothing and gives no error (`merge-reviews/suggest-3.review-triage.md`).

## Deferred by decision

These stay out until the owner asks for them; each spec's status section names them.
- Spec 2: a stored `tsvector` column type (S2.4).
- Spec 4: typing of Python relation sets (S4.1), a path-aware TypeScript row type (S4.5), prefetch statements in `orm.debug.n_plus_one` (S4.6), a `TypedDict` for Python `describe`.
- Spec 6: `robust=` (S6.1), health check and failover (S6.4), `setting=` (S6.7), insert from scope (S6.9).
- Spec 7: `pull` does not reproduce rules, views, policies, composite keys and partitions; it reports them as gaps. `data.py` has no down step (S7.5).
