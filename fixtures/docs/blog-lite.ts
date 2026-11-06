import { connect, loads, registry } from "@ufstack/monorm";

const models = loads(docsSchema, { registry });
const { User, Post, Comment } = models;
const db = await connect(docsDatabaseUrl);
docsTestCleanup = async () => {
  try { await db.dropTables(); } finally { await db.close(); }
};
await db.dropTables(); // a killed earlier run can leave its tables behind
await db.createTables();
await User.objects.insertMany([
  { name: "A", email: "a@example.com" },
  { name: "B", email: "b@example.com" },
]);
await Post.objects.insertMany([
  { authorId: 1, title: "One", body: "First" },
  { authorId: 1, title: "Two", body: "Second" },
  { authorId: 2, title: "Three", body: "Third" },
]);
await Comment.objects.insertMany([
  { postId: 1, authorId: 2, body: "hello" },
  { postId: 1, authorId: 1, body: "thanks" },
]);
const user = await User.objects.get(User.id.eq(1));
const post = await Post.objects.get(Post.id.eq(1));
