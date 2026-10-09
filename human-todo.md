test_update_many: review again
and after that

## Compare newest-comment query plans

Compare both queries with `EXPLAIN (ANALYZE, BUFFERS)` on the same PostgreSQL data and indexes. Check selective and broad post queries before deciding which is faster. Both choose the newest comment per post, breaking timestamp ties by comment ID, and preserve posts without comments.

Correlated LATERAL:

```sql
SELECT p.id, latest.body
FROM posts p
LEFT JOIN LATERAL (
    SELECT c.body
    FROM comments c
    WHERE c.post_id = p.id
    ORDER BY c.created_at DESC, c.id DESC
    LIMIT 1
) latest ON TRUE;
```

Uncorrelated DISTINCT ON:

```sql
SELECT p.id, latest.body
FROM posts p
LEFT JOIN (
    SELECT DISTINCT ON (post_id)
           post_id, body
    FROM comments
    ORDER BY post_id, created_at DESC, id DESC
) latest ON latest.post_id = p.id;
```

## Open decisions (coordinator, 2026-10-09 13:10)
Paths are under `~/projects/worktrees/orm/plans/`. Most urgent first.

| # | File | Decide |
|---|---|---|
| 1 | `merge-reviews/suggest-2.review-triage.md` | json-key-inline: (a) keep JSON keys bound (default now) or (b) write them into the SQL as escaped literals. |
| 2 | `merge-reviews/suggest-3.review-triage.md` | 5 items. Urgent: a caught failure inside `transaction()` loses data with no error. Also `get_or_insert(where=)`, spec F8, COPY with row-level security, upserts skip `version + 1` / `updated_at`. |
| 3 | `merge-reviews/suggest-6.review-triage.md` | 5 items: session locks can deadlock the pool, tenant switch inside a transaction, `refresh()` on replicas, `scope(shop=None)`, the `db.lock` coroutine. |
| 4 | `merge-reviews/suggest-7.review-triage.md` | 5 items: data steps with later models, serial keys after a baseline, rollback of a baselined migration, soft-delete rules in pull, the plan file. |
| 5 | `merge-reviews/triage-1.review-triage.md` | `LIKE` on `char(n)` columns: option A, B or C. |
| 6 | `human-issue2/instances.md`, `writes.md`, `load.md`, `aggregates.md` | Q-questions; defaults applied. Confirm or change. |
| 7 | `merge-reviews/triage-1.md`, `triage-2.md`, `suggestions/[1-7]-*.md` (S-questions), `jury/pm-questions.md` (23) | Older; defaults applied. Low priority. |
| 8 | (coordinator) | Move the run from `/tmp/orm-*` scripts to the skill's `fm-*` scripts? Add `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0` to `~/.claude/settings.json`? |
