# Explicit async reference loading

Instances accept the same load arguments as querysets:

```python
await post.load(Post.body, Post.author, Post.comments)
await post.load(Post.author.name)
await post.load(Post.comments.objects.filter(Comment.body == "hello").label("matching_comments"))
await post.load(Post.author, reload=True)
```

```typescript
const loaded = await post.load(Post.body, Post.author, Post.comments);
await post.load(Post.author.name);
await post.load(Post.comments.objects.filter(Comment.body.eq("hello")).label("matchingComments"));
await post.load(Post.author, { reload: true });
```

The method returns the same instance. Loaded fields and related objects are
preserved when filling a partial shape. Already-loaded values, including nulls
and empty collections, need no query. `reload=True` (TypeScript: `{ reload: true }`)
refreshes requested values. Calling `.load()` without items fills all scalar
fields. Queryset `.load(...)` configures a query; instance `.load(...)` executes
immediately when awaited and adds to the existing instance.

Synchronous attribute access does no I/O and raises `NotLoaded` for missing
values. TypeScript's returned instance type includes the requested fields and
relations; use the returned value to access its expanded type. There are no
generated `load_author()` / `loadAuthor()` methods.

Relation paths, target columns, filters, ordering, slices, labels and nested loads
use the existing queryset load planner. Collection relations remain relation
querysets: read their prefetched values through `.cached`. Scalar loading reads
the owner row; relation loading uses the owner's keys without refreshing its
other scalar fields. For several owners, `of(instances).load(...items)` batches
relation loading.

The `reference-loading` Cargo feature enables the to-one cache adapter and shared
in-progress queries for simple reference loads. The Python and Node bindings,
the CLI and the tooling profile enable it. Baseline profiles support instance
`.load(...)` through the ordinary relation prefetch planner, but do not import
this adapter. Generated modules still record their required capabilities.

Simple reference loads use ordinary target queries and the database of the owner instance.
When the current task has a transaction, loading uses that transaction. The
reference cache belongs to the owner instance. There is no global identity map.
A valid cached object or a cached optional absence needs no query. An absent
reverse one-to-one target and an absent optional target are cached as `None` in
Python and `null` in TypeScript. A required target that is not in the database raises
`IntegrityError`. A target with a prepared default filter can be absent because
the filter hides it. This applies to required references too, so query-loaded
relation types for these targets are nullable.

Calls for the same reference, key, database, and transaction share one query that
is in progress. Concurrent forced reloads also share it. After a query fails, a
new call starts a new query. In Python, when you cancel one waiter, the shared
query and the other waiters continue. The shared operation completes normally, and
its exception is always observed. Node uses ordinary promises and has no separate
cancellation API.

When an instance update or reload changes a source key, the reference becomes
invalid. A reload also makes reverse one-to-one caches invalid, even when the
source key does not change. Thus a reload after you insert a target replaces a
cached absence. A query that started before such a change does not write its stale result to
the cache.
A forced load of a bare reference replaces the cached reference and does not change the scalar
fields of the owner row. Other instances stay as they are until you reload them.

An adapter reads hidden helper keys: it looks first at the public attribute, then
at the hidden values. Ordinary attribute access does not show the hidden values.
Adapters for generic references can use the internal function that shares
concurrent loads. That function does not route generic references.
