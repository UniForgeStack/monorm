# Loading values onto an instance

An instance can load more values after a query.
It takes the same load arguments as a query set.
This guide is for both languages.
The internals are in [reference-loading.md](../contracts/reference-loading.md).

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

The method returns the same instance.
It keeps the loaded fields and related objects when it fills a partial shape.
A value that is already loaded needs no query, also a null or an empty collection.
`reload=True` (TypeScript: `{ reload: true }`) refreshes the requested values.
`.load()` without items fills all scalar fields that are missing.
A query set `.load(...)` configures a query.
An instance `.load(...)` runs when you await it, and adds to the instance.

Attribute access does no I/O.
A missing value raises `NotLoaded`.
In TypeScript, the type of the returned instance has the requested fields and relations: use the returned value.
There are no generated `load_author()` / `loadAuthor()` methods.

Relation paths, target columns, filters, order, slices, labels and nested loads use the load planner of query sets ("Loading related objects" in [python-api.md](../api/python-api.md#loading-related-objects) and [typescript-api.md](../api/typescript-api.md#loading-related-objects)).
A collection relation stays a relation query set: read its loaded values with `.cached`.
Scalar loading reads the owner row.
Relation loading uses the keys of the owner and does not refresh its other scalar fields.
For several owners, `of(instances).load(...items)` loads the relations in a batch.

## Reference cache and shared queries

A simple load of a to-one reference uses the database of the owner instance.
In a transaction, it uses that transaction.
The cache belongs to the owner instance.
There is no global identity map.

- A cached object, or a cached absence of an optional target, needs no query.
- An absent optional target, and an absent reverse one-to-one target, are cached as `None` (TypeScript: `null`).
- A required target that is not in the database raises `IntegrityError`.
- A default filter on the target can hide a target. This applies to required references too, so the loaded type of such a relation is nullable.

Calls for the same reference, key, database and transaction share one query that is in progress.
Concurrent forced reloads share it too.
After a query fails, the next call starts a new query.
In Python, when you cancel one waiter, the shared query and the other waiters continue.
Node uses ordinary promises and has no cancel API.

An update or a reload that changes a source key makes the reference invalid.
A reload also makes reverse one-to-one caches invalid, so a reload after you insert a target replaces a cached absence.
A query that started before such a change does not write its stale result to the cache.
A forced load of a bare reference replaces the cached reference and leaves the scalar fields of the owner row.
Other instances stay as they are until you reload them.
