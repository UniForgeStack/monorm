<!-- docs-test: setup=blog-lite -->

# Deliberate public API typo

```python run
await User.objects.no_such_method()
```

```typescript run
await User.objects.noSuchMethod();
```
