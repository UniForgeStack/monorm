<!-- docs-test: setup=blog-lite db=postgresql -->

# Separate PostgreSQL database

```python run
assert user.name == "A"
assert await Post.objects.count() == 3
```

```typescript run
if (user.name !== "A" || await Post.objects.count() !== 3) throw new Error("seed mismatch");
```
