# History: `@@history.track`

`@@history.track` writes one event for each insert, update and delete of a row.
A database trigger writes the event, so raw SQL and other applications are tracked too.
Each tracker adds an event model, for example `PostEvent` in the table `posts_events`.
This page describes track mode.

```prisma
model Post {
  id    Int    @id @default(autoincrement())
  title String
  body  String?
  @@history.track
}
```

## Arguments

| Argument | Meaning |
| --- | --- |
| `model` | The event model name. Default: `<Model>Event`. A second tracker on the same model needs its own name. |
| `events` | `[insert, update, delete]` (default: all three). |
| `row` | `new` (default) or `old`: the row state that an update event holds. |
| `label` | The event label. Default: the event kind. |
| `when` | A SQL condition on `NEW` and `OLD`. An update writes an event only when it is true. It needs `update` in `events`; without it the schema load fails. |
| `fields`, `exclude` | The fields that the event holds. Default: every stored field. |
| `scope` | Scope keys. Each key becomes a column of the event model. |

An update that changes only a tracked-away column (for example `updated_at` from the timestamps extension) writes no event.
A tracker that lists `fields` writes an update event only when one of them changes.

## The event model

Columns: `id`, `row_<key>` (one for each key field of the row), `label`, `data`, `created_at`, `xact_id`, and one column for each scope key.
`data` is a JSON object that holds the tracked fields, keyed by the schema field name.
`xact_id` is the PostgreSQL transaction id. SQLite stores `NULL`.
The event model is write-protected in the application: an insert, update or delete outside `purge` fails with `WriteProtected`.

A composed model has rows in several tables and cannot be tracked. Track each part model.
The trigger names of a tracker are `h_<suffix>_*`.
The suffix is `events` for the default event model, and the snake-case name of a `model:` argument.
Two trackers of one table with the same suffix fail the schema load; for example `model: "Events"` clashes with the default tracker.

## Python

```python
post = await Post.objects.insert(title="a")
await Post.objects.filter(Post.id == post.id).update(title="b")

timeline = await Post.history.timeline(post, diff=True)   # [HistoryEntry(event, diff)]
timeline[1].diff                                         # {"title": {"old": "a", "new": "b"}}
await Post.history.at(post, when)                        # the last event at or before `when`, or None; its `data` is the row then
await Post.history.event(post, "reviewed")               # a manual event
await Post.history.backfill("snapshot")                  # one event for each row, in one statement
await Post.history.restore(timeline[0].event)            # write the row back; label "restore"
await Post.history.purge(before=cutoff)                  # the one delete path of the events
```

`row.history` is the same API for the row.
`timeline` always gives `HistoryEntry` objects; `diff` is `None` unless `diff=True` and the event has a previous one.
With several trackers, pass `tracker="<event model name>"`; `purge` without it covers every tracker.

## TypeScript

```ts
const timeline = await post.history.timeline(post, { diff: true });
await post.history.restore(timeline[0].event);
await post.history.purge({ before: cutoff });
```

`row.history` is the entry point. A model class has no typed `Model.history` member yet.
A `BigInt` above 2^53 loses precision in `data`, because the event holds JSON that `JSON.parse` reads.
`restore` refuses such an event with an error and does not write the wrong value.

## Restore

`restore(event)` runs in one transaction and leaves one event with the label `restore`.
On PostgreSQL, `restore` sets the transaction-local setting `monorm.history_restore` to the event table, and the trigger of that tracker writes the label `restore`.
A tracker of the same model with another event table keeps its own label.
On SQLite a trigger cannot read a setting, so `restore` changes the label of the events that it wrote, after the write. A label that the tracker sets with `label:` is replaced for those events.
If the row exists, `restore` updates it with the fields of the event.
If the row is gone, `restore` inserts it again, only when the tracker holds every field and the event has every required field.
`row: old` trackers store the old row in an update event, so `restore` of such an event writes the old state, as the event says.
It decodes dates, times, decimals, big integers and UUIDs from JSON by the field type, and ignores event fields that the model no longer has.

## Scope

`@@history.track(scope: [user])` stores the scope value `user` with each event.
Set it in the web middleware:

```python
with monorm.scope(user=request.user.id):
    ...
```

On PostgreSQL the trigger reads the transaction setting `scope.user`; a statement without a value stores `NULL`.
SQLite has no setting, so it stores `NULL`, and `scope(user=...)` is still accepted.
History publishes every key that it names, so `scope()` accepts it without a `@@scope.by` that uses the key.

## Skip

`skip_triggers("history")` skips the event triggers for a role in `bypass_roles` of the `datasource` block, on PostgreSQL.
A role outside the list still writes events.
SQLite has no skip: `skip_triggers("history")` raises `QueryError`.

## Limits

* SQLite has one trigger for each event kind and takes at most 500 tracked fields.
* A backfill on a large table runs as one statement.
* Versions mode (`@@history.versions`) is not in this release.
