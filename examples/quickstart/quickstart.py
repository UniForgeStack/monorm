"""Run from the repo root: .venv/bin/python examples/quickstart/quickstart.py [url]."""

import asyncio
import sys

import monorm

SCHEMA = """
model User {
  id    Int @id @default(autoincrement())
  name  String
  posts Post[]
}
model Post {
  id        Int @id @default(autoincrement())
  title     String
  published Boolean
  author_id Int
  author    User @relation(fields: [author_id], references: [id])
}
"""


async def main():
    url = sys.argv[1] if len(sys.argv) > 1 else "sqlite://:memory:"
    provider = "sqlite" if url.startswith("sqlite:") else "postgresql"
    models = monorm.loads(f'datasource db {{\n  provider = "{provider}"\n}}\n' + SCHEMA)
    User, Post = models["User"], models["Post"]
    db = await monorm.connect(url)
    try:
        await db.create_tables()
        alice = await User.objects.insert(name="Alice")
        await Post.objects.insert_many(
            [
                {"title": "Hello", "published": True, "author": alice},
                {"title": "Draft", "published": False, "author": alice},
            ]
        )
        users = await User.objects.filter(User.posts.published).load(User.posts)
        for user in users:
            print(user.name, [post.title for post in user.posts.cached])
    finally:
        await db.close()


asyncio.run(main())
