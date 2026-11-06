import monorm

models = monorm.loads(docs_schema)
User, Post, Comment = (models[name] for name in ("User", "Post", "Comment"))
db = await monorm.connect(docs_database_url)


async def docs_test_cleanup(database=db):
    try:
        await database.drop_tables()
    finally:
        await database.close()


await db.drop_tables()  # a killed earlier run can leave its tables behind
await db.create_tables()
await User.objects.insert_many([
    {"name": "A", "email": "a@example.com"},
    {"name": "B", "email": "b@example.com"},
])
await Post.objects.insert_many([
    {"author_id": 1, "title": "One", "body": "First"},
    {"author_id": 1, "title": "Two", "body": "Second"},
    {"author_id": 2, "title": "Three", "body": "Third"},
])
await Comment.objects.insert_many([
    {"post_id": 1, "author_id": 2, "body": "hello"},
    {"post_id": 1, "author_id": 1, "body": "thanks"},
])
user = await User.objects.get(User.id == 1)
post = await Post.objects.get(Post.id == 1)
