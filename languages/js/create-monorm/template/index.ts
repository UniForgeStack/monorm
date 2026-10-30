import { connect, load } from "monorm";

const { User, Post } = load("schema.prisma");
const db = await connect("sqlite://dev.db");
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
await db.close();
