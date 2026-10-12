// Design example, not an implemented ORM client.
// The shape declarations model generator output; fixture loaders use memory, not SQL.
// Compiled and executed with rustc 1.97.0 during design validation.

use std::marker::PhantomData;
use std::ops::Deref;

// Schema-generated code.
pub struct Unloaded;
pub struct Post { pub title: String }
pub struct LoadedPosts { pub cached: Vec<Post> }
pub trait UserShape { type Email; type Posts; type Name; }
pub struct Shape<E, P, N>(PhantomData<(E, P, N)>);
impl<E, P, N> UserShape for Shape<E, P, N> {
    type Email = E;
    type Posts = P;
    type Name = N;
}
pub struct UserFields<S: UserShape> {
    pub email: S::Email,
    pub posts: S::Posts,
    pub name: S::Name,
}
pub struct User<S: UserShape> { fields: UserFields<S> }
impl<S: UserShape> Deref for User<S> {
    type Target = UserFields<S>;
    fn deref(&self) -> &Self::Target { &self.fields }
}
pub type DigestShape = Shape<String, LoadedPosts, Unloaded>;
pub type FullShape = Shape<String, LoadedPosts, String>;

// Loader internals: memory data stands in for a database query.
struct StoredUser { id: i64, email: String, name: String, posts: Vec<String> }
struct Database { users: Vec<StoredUser> }
#[derive(Debug)]
struct NotFound;
async fn load_digest_user(db: &Database, id: i64) -> Result<User<DigestShape>, NotFound> {
    let row = db.users.iter().find(|r| r.id == id).ok_or(NotFound)?;
    Ok(User { fields: UserFields {
        email: row.email.clone(),
        posts: LoadedPosts { cached: row.posts.iter().map(|t| Post { title: t.clone() }).collect() },
        name: Unloaded,
    } })
}
async fn load_full_user(db: &Database, id: i64) -> Result<User<FullShape>, NotFound> {
    let row = db.users.iter().find(|r| r.id == id).ok_or(NotFound)?;
    Ok(User { fields: UserFields {
        email: row.email.clone(),
        posts: LoadedPosts { cached: row.posts.iter().map(|t| Post { title: t.clone() }).collect() },
        name: row.name.clone(),
    } })
}

// Application code.
struct DigestJob<S: UserShape<Email = String, Posts = LoadedPosts>> {
    user: User<S>,
}
impl<S: UserShape<Email = String, Posts = LoadedPosts>> DigestJob<S> {
    fn render(&self) -> String { build_digest(&self.user) }
}
fn build_digest<S: UserShape<Email = String, Posts = LoadedPosts>>(user: &User<S>) -> String {
    let titles: Vec<&str> = user.posts.cached.iter().map(|p| p.title.as_str()).collect();
    format!("To: {}\nPosts: {}", user.email, titles.join(", "))
}
async fn example(db: &Database) -> Result<(), NotFound> {
    let partial = load_digest_user(db, 1).await?;
    assert_eq!(build_digest(&partial), "To: a@example.com\nPosts: First post");
    let full = load_full_user(db, 1).await?;
    assert_eq!(build_digest(&full), build_digest(&partial));
    assert_eq!(full.name, "Alice");
    let partial_job = DigestJob { user: partial };
    let full_job = DigestJob { user: full };
    assert_eq!(partial_job.render(), full_job.render());
    Ok(())
}
fn main() {
    // The fixture loaders complete immediately; one poll runs this example.
    let db = Database { users: vec![StoredUser { id: 1, email: "a@example.com".into(), name: "Alice".into(), posts: vec!["First post".into()] }] };
    let mut future = Box::pin(example(&db));
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    match std::future::Future::poll(future.as_mut(), &mut cx) {
        std::task::Poll::Ready(result) => result.unwrap(),
        std::task::Poll::Pending => panic!("fixture unexpectedly suspended"),
    }
}
