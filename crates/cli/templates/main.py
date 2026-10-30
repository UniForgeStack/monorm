import asyncio
import os
from pathlib import Path

import monorm

models = monorm.load(Path(__file__).with_name("schema.prisma"))
User, Post = models["User"], models["Post"]


async def main():
    db = await monorm.connect(os.environ.get("MONORM_DATABASE_URL", "{{URL}}"))
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
