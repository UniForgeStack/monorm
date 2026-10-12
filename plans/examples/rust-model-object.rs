// Executable naming/typing probe, not the implemented ORM client.
// A model value User coexists with the row type User<S>.
// Its fixture default shape eagerly includes posts solely for this small probe.

use std::ops::Deref;
trait UserShape { type Email; type Posts; }
struct LoadedPosts { cached: Vec<String> }
struct DefaultShape;
impl UserShape for DefaultShape { type Email = String; type Posts = LoadedPosts; }
struct UserFields<S: UserShape> { email: S::Email, posts: S::Posts }
struct User<S: UserShape> { fields: UserFields<S> }
impl<S: UserShape> Deref for User<S> { type Target = UserFields<S>; fn deref(&self) -> &Self::Target { &self.fields } }
struct UserModel { objects: QueryRoot }
struct QueryRoot;
#[allow(non_upper_case_globals)]
const User: UserModel = UserModel { objects: QueryRoot };
impl User<DefaultShape> {
    #[allow(non_upper_case_globals)]
    const objects: QueryRoot = QueryRoot;
}
impl QueryRoot {
    async fn first(self) -> Option<User<DefaultShape>> {
        Some(User { fields: UserFields { email: "a@example.com".into(), posts: LoadedPosts { cached: vec!["A post".into()] } } })
    }
}
fn digest<S: UserShape<Email = String, Posts = LoadedPosts>>(user: &User<S>) -> String {
    format!("{}: {} posts", user.email, user.posts.cached.len())
}
async fn example() {
    let user = User.objects.first().await.unwrap();
    assert_eq!(digest(&user), "a@example.com: 1 posts");
    let user = User::objects.first().await.unwrap();
    assert_eq!(digest(&user), "a@example.com: 1 posts");
}
fn main() {
    let mut f = Box::pin(example());
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match std::future::Future::poll(f.as_mut(), &mut cx) {
        std::task::Poll::Ready(()) => (),
        std::task::Poll::Pending => panic!("fixture suspended"),
    }
}
