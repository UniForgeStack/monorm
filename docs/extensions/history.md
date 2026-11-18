# History: `@@history.track`

`@@history.track` writes one event for each insert, update and delete of a row.
A database trigger writes the event, so raw SQL and other applications are tracked too.
Each tracker adds an event model, for example `PostEvent` in the table `post_events`.
The table name is the table of the root model plus `_events`.
This page describes track mode, then [versions mode](#versions-mode-historyversions).

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
| `row` | `new` (default) or `old`: the row state that an update event holds. Another value fails with `row must be old or new`. |
| `label` | The event label. Default: the event kind. |
| `when` | A SQL condition on `NEW` and `OLD`. An update writes an event only when it is true. It needs `update` in `events`; without it the schema load fails. |
| `fields`, `exclude` | The fields that the event holds. Default: every stored field. |
| `scope` | Scope keys. Each key becomes a column of the event model. Default: none. |

An update that changes only a tracked-away column (for example `updated_at` from the timestamps extension) writes no event.
A tracker that lists `fields` writes an update event only when one of them changes.

## The event model

Columns: `id`, `row_<key>` (one for each key field of the row), `label`, `data`, `created_at`, `xact_id`, and one column for each scope key.
`data` is a JSON object that holds the tracked fields, keyed by the schema field name.
`xact_id` is the PostgreSQL transaction id. SQLite stores `NULL`.
The event model is write-protected in the application: an insert, update or delete outside `purge` fails with `WriteProtected`.

A composed model has rows in several tables and cannot be tracked. Track each part model.
The PostgreSQL trigger names of a tracker are `h_<suffix>_ins`, `h_<suffix>_upd` and `h_<suffix>_del`, with the functions `<table>_h_<suffix>_ins|upd|del`.
SQLite names them `<table>_h_<suffix>_ins_trigger`, `<table>_h_<suffix>_upd_trigger` and `<table>_h_<suffix>_del_trigger`.
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

`row.history` is the entry point. A model class has no typed `Model.history` member yet; `history(Model)` from `monorm/extensions/history` is the class-level entry, in track mode and in versions mode.
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

## Versions mode: `@@history.versions`

`@@history.versions(of: <field>, scope?: [...])` turns a model into the version table of an aggregate.
The model is a user model, for example `BundleVersion`.
`of` names the field that holds the id of the aggregate (it cannot be the primary key).
The extension adds the columns `version_number`, `snapshot` (JSON), `content_hash`, `effective_at`, `change_type` (`create`, `update`, `delete`, `restore`), `restored_from` (the id of the restored version, a foreign key to the same table with `ON DELETE SET NULL`), `xact_id` and one column for each `scope` key.
A unique index on (`of`, `version_number`) is the backstop of the numbering.
A model cannot have both `@@history.versions` and `@@history.track`.

```prisma
model BundleVersion {
  id        Int @id @default(autoincrement())
  bundle_id Int
  @@history.versions(of: bundle_id, scope: [user])
}
```

You register two functions once for each model: `generate(entity)` gives the snapshot (a JSON object) and `restore(snapshot)` writes it back.
Both can be sync or async.
`restore` gets only the snapshot.
The entity id is the entity itself when it is a plain value, and its `id` when it is an object or a dict.

```python
BundleVersion.history.register(generate=bundle_snapshot, restore=bundle_restore)

await BundleVersion.history.record(bundle, "update")       # the version, or the latest one if nothing changed
await BundleVersion.history.version_at(bundle, when)       # the last version at or before `when`, or None
await BundleVersion.history.timeline(bundle, diff=True)    # [HistoryEntry(event, diff)] by version number
await BundleVersion.history.restore(version)               # calls restore(snapshot), then writes a "restore" version
```

```ts
import { history } from "monorm/extensions/history";

history(BundleVersion).register({ generate: bundleSnapshot, restore: bundleRestore });
await history(BundleVersion).record(bundle, "update");
await history(BundleVersion).versionAt(bundle, when);
await history(BundleVersion).timeline(bundle, { diff: true });
await history(BundleVersion).restore(version);
```

`record` runs in one transaction.
On PostgreSQL it takes a transaction lock for the table and the entity, so concurrent writers get distinct numbers.
It writes the next number only when the content hash of the snapshot differs from the hash of the latest version.
`delete` and `restore` versions are always written.
A version after a `delete` is also always written, because the entity lives again.
A `restore` version stores the restored snapshot and `restored_from`.
If the `restore` function raises, no version is written.
`record` rejects the change type `restore`: only `restore()` writes it.
`at(entity, when)` is `version_at`.
A version rolls back with the outer transaction.
Scope values (`scope(user=...)`) and `xact_id` are stored on PostgreSQL and are `NULL` on SQLite.

## Scope

`@@history.track(scope: [user])` stores the scope value `user` with each event.
Set it in the web middleware:

```python
with monorm.scope(user=request.user.id):
    ...
```

On PostgreSQL the trigger reads the transaction setting `scope.user`; a statement without a value stores `NULL`.
SQLite has no setting, so it stores `NULL`, and `scope(user=...)` is still accepted.
A version model with `scope: [user]` stores the same value.
History publishes every key that it names, so `scope()` accepts it without a `@@scope.by` that uses the key.

## Skip

`skip_triggers("history")` skips the event triggers for a role in `bypass_roles` of the `datasource` block, on PostgreSQL.
A role outside the list still writes events.
SQLite has no skip: `skip_triggers("history")` raises `QueryError`.

## Limits

* SQLite has one trigger for each event kind and takes at most 500 tracked fields.
* A backfill on a large table runs as one statement.
* `pull` reads an event table or a version table as a plain model.
  On PostgreSQL it also writes the triggers as `@@trigger(...)` lines. On SQLite it reports the triggers as a gap.
  It never writes `@@history.track` or `@@history.versions`, so a schema from `pull` loses the write protection and the API.
* SQLite stamps `effective_at` with whole seconds, so `version_at` cannot tell two versions of one second apart.

## Errors

| Error | Cause and fix |
| --- | --- |
| `WriteProtected` | An ORM write to an event model outside `purge`. Use `purge`. Raw SQL is not checked. |
| `QueryError` from `skip_triggers("history")` | SQLite has no skip. Use PostgreSQL with a role in `bypass_roles`. |
| `ValueError` from `record` with `"restore"` | Only `restore()` writes that change type. |
| schema: `when` without `update` in `events` | Add `update` to `events`, or remove `when`. |
| schema: two trackers with one suffix | Give the second one `model: "Other"`. |
| schema: a composed model | Track each part model, or use versions mode. |
| schema: `@@history.versions` on a model with a composite key, a key with no database default, or `of` on the primary key | Use a single-field primary key with a database default, and another field for `of`. |
