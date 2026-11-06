# Runner contract

```text file=nested/message.txt
hello
```

```python
# Prisma Client Python
raise RuntimeError("unmarked block ran")
```

```python run
import asyncio
from pathlib import Path
message = Path("nested/message.txt").read_text()
await asyncio.sleep(0)
```

```python run
assert message == "hello\n"
```

```python run fail=ValueError
raise ValueError(message)
```

```typescript
throw new Error("unmarked block ran");
```

```typescript run
import { readFile } from "node:fs/promises";
const message = await readFile("nested/message.txt", "utf8");
```

```typescript run
if (message !== "hello\n") throw new Error(message);
```

```typescript run fail=TypeError
throw new TypeError(message);
```

```typescript run
import { LANGUAGE_CONTRACT } from "@ufstack/monorm/ext";
if (typeof LANGUAGE_CONTRACT !== "number") throw new Error("subpath import did not resolve");
```
