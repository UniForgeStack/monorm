# Fence parser cases

````markdown
```python run
not_a_real_block()
```
````

```python run fail=ValueError
raise ValueError("expected")
```

```typescript run fail=TypeError
throw new TypeError("expected");
```

```prisma file=schema.prisma
model Item {
  id Int @id
}
```

```python
# Prisma Client Python
this_must_not_run()
```

````python run
text = "```"
```
assert text
````
