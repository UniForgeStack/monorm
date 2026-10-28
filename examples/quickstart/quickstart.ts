// Build languages/js first; then npm install and node quickstart.ts (Node 23.6+).
import { connect, loads } from "@ufstack/monorm";

const url = process.argv[2] ?? "sqlite://:memory:";
const provider = url.startsWith("sqlite:") ? "sqlite" : "postgresql";
const { User, Post } = loads(`datasource db {
  provider = "${provider}"
}
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
`);
const db = await connect(url);
try {
  await db.createTables();
  const alice = await User.objects.insert({ name: "Alice" });
  await Post.objects.insertMany([
    { title: "Hello", published: true, author: alice },
    { title: "Draft", published: false, author: alice },
  ]);
  const users = await User.objects.filter(User.posts.published).load(User.posts);
  for (const user of users) {
    console.log(user.name, user.posts.cached.map((post) => post.title));
  }
} finally {
  await db.close();
}
