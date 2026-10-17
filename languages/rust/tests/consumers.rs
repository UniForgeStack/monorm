//! Compile real downstream programs, checking categories rather than full diagnostics.
use std::{path::PathBuf, process::Command};
#[test]
fn generated_consumer_contracts() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::temp_dir().join(format!("orm-rust-consumers-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"),format!("[package]\nname='orm-consumer-probe'\nversion='0.0.0'\nedition='2021'\n[workspace]\n[dependencies]\norm-client={{path={:?}}}\n",root)).unwrap();
    std::fs::write(
        dir.join("src/models.rs"),
        include_str!(concat!(env!("OUT_DIR"), "/models.rs")),
    )
    .unwrap();
    let prefix="#![allow(dead_code,unused_variables,unused_mut)]\nmod models;\nuse models::{User,Post,Database};\nuse orm_client::{Loaded,Unloaded,Instance};\n";
    let cases=[
        ("reverse to one projection is nullable","async fn check(db:&Database) {let _:Vec<Option<String>>=models::ProjectionPerson::objects.select(models::ProjectionPerson::account_path().code()).require().using(db).scalars().await.unwrap();}",""),
        ("prepared scalar retains requiredness and repeats","async fn check(db:&Database) {let prepared=User::objects.select(User::nickname).require().prepare();let _:Option<String>=prepared.using(db).scalar().await.unwrap();let _:Vec<Option<String>>=prepared.using(db).scalars().await.unwrap();let _=prepared.using(db).statement().unwrap().inspect();}",""),
        ("prepared exported model result preserves shape bounds","orm_client::named_result! {pub struct Contact {person}} fn email<S:models::UserShape<Email=String>>(row:&S)->&String {row.email()} async fn check(db:&Database) {let prepared=orm_client::select_named!(User::objects=>Contact;person=>orm_client::model::<User>()).prepare().require();let row:Contact<User>=prepared.using(db).first().await.unwrap();email(&row.person);let row=prepared.using(db).first().await.unwrap();email(&row.person);}",""),
        ("prepared scalar preserves NULL","async fn bad(db:&Database) {let prepared=User::objects.select(User::nickname).require().prepare();let _:String=prepared.using(db).scalar().await.unwrap();}","E0308"),
        ("prepared placeholders merge repeated slots and bind a pack","orm_client::params! {mod r {lo: i32, hi: i32}} async fn check(db:&Database) {let t=User::objects.prepare().filter(User::id.column().ge_param(r::lo()).and(User::id.column().ne_param(r::lo()))).filter(User::id.column().le_param(r::hi()));let a=t.bind(&r::Args{lo:1,hi:3}).unwrap();let b=t.bind(&r::Args{lo:2,hi:4}).unwrap();let _:Vec<User>=a.using(db).all().await.unwrap();let _:Vec<User>=b.using(db).all().await.unwrap();}",""),
        ("projected placeholders keep requiredness","orm_client::params! {mod r {lo: i32}} async fn check(db:&Database) {let t=User::objects.select(User::email).require().prepare().filter(User::id.column().gt_param(r::lo()));let _:String=t.bind(&r::Args{lo:1}).unwrap().using(db).scalar().await.unwrap();}",""),
        ("missing binding is rejected","orm_client::params! {mod a {lo: i32}} orm_client::params! {mod b {hi: i32}} fn bad() {let t=User::objects.prepare().filter(User::id.column().ge_param(a::lo()).and(User::id.column().le_param(b::hi())));let _=t.bind(&a::Args{lo:1});}","E0277"),
        ("same name with another type is a missing binding","orm_client::params! {mod a {id: i32}} orm_client::params! {mod b {id: String}} fn bad() {let t=User::objects.prepare().filter(User::id.column().eq_param(a::id()).and(User::email.column().eq_param(b::id())));let _=t.bind(&a::Args{id:1});}","E0277"),
        ("placeholder needs the column value type","orm_client::params! {mod a {id: i32}} fn bad() {User::objects.prepare().filter(User::email.column().eq_param(a::id()));}","E0271"),
        ("unbound template cannot execute","orm_client::params! {mod a {id: i32}} async fn bad(db:&Database) {let _=User::objects.prepare().filter(User::id.column().eq_param(a::id())).using(db);}","E0599"),
        ("placeholder cannot enter an unprepared query","orm_client::params! {mod a {id: i32}} fn bad() {User::objects.filter(User::id.column().eq_param(a::id()));}","E0308"),
        ("optional joins nullable values and text annotations","async fn check(db:&Database) {let _:Vec<Option<String>>=models::PolicyBook::objects.without_related().select(models::PolicyBook::editor_path().name()).require().using(db).scalars().await.unwrap();let _:Option<String>=models::PolicyBook::objects.without_related().select(models::PolicyBook::editor_path().note()).require().using(db).scalar().await.unwrap();let _:Option<i32>=models::PolicyBook::objects.without_related().select(models::PolicyBook::editor_path().name().upper().length()).require().using(db).scalar().await.unwrap();}",""),
        ("requiredness retains optional join nullability","async fn bad(db:&Database) {let _:String=models::PolicyBook::objects.without_related().select(models::PolicyBook::editor_path().name()).require().using(db).scalar().await.unwrap();}","E0308"),
        ("optional path through many still rejects projection","fn bad() {models::PolicyBook::objects.select(models::PolicyBook::editor_path().books().author().name());}","E0277"),
        ("named results accept explicit alternatives","enum Alternative {Email(String),Nickname(Option<String>)} async fn check(db:&Database,flag:bool)->orm_client::Result<Vec<Alternative>> {if flag {User::objects.select(User::email).map(|(v,)|Alternative::Email(v)).using(db).all().await} else {User::objects.select(User::nickname).map(|(v,)|Alternative::Nickname(v)).using(db).all().await}}",""),
        ("typed annotations retain nullability","async fn check(db:&Database) {let named=orm_client::select_named!(User::objects;size=>User::nickname.column().length(),upper=>User::email.column().upper()).require().using(db).first().await.unwrap();let _:Option<i32>=named.size;let _:String=named.upper;}",""),
        ("numeric text function rejected","fn bad() {User::id.column().lower();}","E0599"),
        ("exported model projection preserves superset bounds","orm_client::named_result! {pub struct Person {person,alias}} fn email<S:models::UserShape<Email=String>>(row:&S)->&String {row.email()} async fn check(db:&Database) {let full:Person<User,Option<String>>=orm_client::select_named!(User::objects => Person; person=>orm_client::model::<User>(),alias=>User::nickname).require().using(db).first().await.unwrap();email(&full.person);let partial=orm_client::select_named!(User::objects; person=>orm_client::model::<User<Unloaded,Loaded,Unloaded,Unloaded>>()).require().using(db).first().await.unwrap();email(&partial.person);}",""),
        ("model projection rejects missing shape field","fn email<S:models::UserShape<Email=String>>(_:S) {} async fn bad(db:&Database) {let (row,)=User::objects.select((orm_client::model::<User<Loaded,Unloaded,Loaded,Loaded>>(),)).require().using(db).first().await.unwrap();email(row);}","E0271"),
        ("tuple and scalar inferred requiredness","async fn check(db:&Database) {let _:Vec<(String,Option<String>)>=User::objects.select((User::email,User::nickname)).require().using(db).await.unwrap();let _:Option<Option<String>>=User::objects.select(User::nickname).using(db).scalar().await.unwrap();let _:Option<String>=User::objects.require().select(User::nickname).using(db).scalar().await.unwrap();}",""),
        ("named export inferred fields","mod exported {orm_client::named_result! {pub struct Contact {email,nickname}}} async fn check(db:&Database)->orm_client::Result<exported::Contact<String,Option<String>>> {orm_client::select_named!(User::objects => exported::Contact; email=>User::email,nickname=>User::nickname).require().using(db).first().await}",""),
        ("duplicate projection labels","fn bad() {let query=orm_client::select_named!(User::objects; email=>User::email,email=>User::nickname);}","E0124|E0403|E0415"),
        ("wrong projection source","fn bad() {User::objects.select(Post::title);}","E0277"),
        ("many path projection","fn bad() {User::objects.select(User::posts_path().title());}","E0277"),
        ("tuple scalar arity","async fn bad(db:&Database) {User::objects.select((User::email,User::nickname)).using(db).scalar().await;}","E0599"),
        ("required scalar retains SQL nullability","async fn bad(db:&Database) {let _:String=User::objects.select(User::nickname).require().using(db).scalar().await.unwrap();}","E0308"),
        ("projection statement executes once","async fn bad(db:&Database) {let s=User::objects.select(User::email).using(db).statement().unwrap();s.execute().await;s.execute().await;}","E0382"),
        ("custom methods before and after projection","trait Readers:Sized {fn readers(self)->Self;} impl<M:orm_client::Model<Source=models::UserSource>,R,L,D> Readers for orm_client::Query<M,R,L,D> {fn readers(self)->Self {self.filter(User::TYPE.eq(\"reader\".into()))}} impl<M:orm_client::Model<Source=models::UserSource>,P:orm_client::Projection<models::UserSource>,R,L,D> Readers for orm_client::ProjectedQuery<M,P,R,L,D> {fn readers(self)->Self {self.filter(User::TYPE.eq(\"reader\".into()))}} async fn check(db:&Database) {let _:String=User::objects.readers().require().select(User::email).using(db).scalar().await.unwrap();let _:String=User::objects.select(User::email).require().readers().using(db).scalar().await.unwrap();}",""),
        ("associated root is a const Query", "const ROOT:orm_client::Query<User>=User::objects; const NEW:orm_client::Query<User>=orm_client::Query::new(); fn check() {let _:orm_client::Query<User>=ROOT;let _:orm_client::Query<User>=NEW;}", ""),
        ("repeated runtime selections retain a fixed row type", "async fn check(db:&Database) {let row:orm_client::RuntimeRow<User>=User::objects.runtime(&[\"email\"]).unwrap().runtime(&[\"nickname\"]).unwrap().require().using(db).first().await.unwrap();let erased:orm_client::RuntimeRow<User>=row.into_runtime();}", ""),
        ("dropping consuming future consumes row ownership", "fn bad(row:User,db:&Database) {let future=row.load(User::email,db);drop(future);println!(\"{}\",row.email);}", "E0382"),

        ("snake case comparison and relation paths", "fn check() {use orm_client::PathDescriptor;let query=User::objects.load(User::email).filter(User::email.eq(\"a\".to_owned())).filter(User::posts.path().title().eq(\"one\".to_owned())).order_by(User::id.desc());}", ""),
        ("consuming transitions have Send futures", "fn send<T:Send>(_:T) {} fn check(row:User,db:&Database) {send(row.load(User::posts,db));}", ""),

        ("nested shape structs accept partial full and checked rows", "struct Reader<S,P> where S:models::UserShape<Email=String,Posts=orm_client::LoadedMany<P>>,P:models::PostShape<Title=String> {row:S,target:std::marker::PhantomData<fn()->P>} fn reader<S,P>(row:S)->Reader<S,P> where S:models::UserShape<Email=String,Posts=orm_client::LoadedMany<P>>,P:models::PostShape<Title=String> {for post in row.posts().cached() {let _: &String=post.title();} Reader{row,target:std::marker::PhantomData}} async fn check(db:&Database,runtime:orm_client::RuntimeRow<User>) {let partial=User::objects.load((User::email,User::posts.load(Post::title))).require().using(db).first().await.unwrap();let _=reader(partial);let full=User::objects.load(User::posts).require().using(db).first().await.unwrap();let _=reader(full);type Requested=User<Unloaded,Loaded,Unloaded,Unloaded,orm_client::Cached<Post<Unloaded,Unloaded,Loaded,Unloaded>>>;let checked:Requested=runtime.try_into_static().unwrap();let _=reader(checked);}", ""),
        ("nested shape structs reject missing titles", "struct Reader<S,P> where S:models::UserShape<Posts=orm_client::LoadedMany<P>>,P:models::PostShape<Title=String> {row:S,target:std::marker::PhantomData<fn()->P>} async fn bad(db:&Database) {let row=User::objects.load(User::posts.load(Post::views)).require().using(db).first().await.unwrap();let _=Reader{row,target:std::marker::PhantomData};}", "E0271"),
        ("root required loaded collection", "async fn check() {let row=User::objects.load((User::email,User::posts)).require().first().await.unwrap();let _: &String=&row.email;let _: &orm_client::LoadedMany<Post>=&row.posts;}", ""),
        ("shape structs accept superset", "struct View<S:models::UserShape<Email=String>> {row:S} async fn check(db:&Database) {let a=View{row:User::objects.load(User::email).require().using(db).first().await.unwrap()};let b=View{row:User::objects.require().using(db).first().await.unwrap()};}", ""),
        ("same runtime row for static branches", "fn check(a:User,b:User<Unloaded,Loaded,Unloaded,Unloaded>,flag:bool)->orm_client::RuntimeRow<User> {if flag {a.into_runtime()} else {b.into_runtime()}}", ""),
        ("runtime checked nested requirements", "fn consume<S,P>(row:&S) where S:models::UserShape<Email=String,Posts=orm_client::LoadedMany<P>>,P:models::PostShape<Title=String> {} fn check(row:orm_client::RuntimeRow<User>) {type Requested=User<Unloaded,Loaded,Unloaded,Unloaded,orm_client::Cached<Post<Unloaded,Unloaded,Loaded,Unloaded>>>;let requested:Requested=row.try_into_static().unwrap();consume(&requested);}", ""),
        ("queryable cache is not loaded", "fn consume<S:models::UserShape<Posts=orm_client::LoadedMany<Post>>>(_:&S) {} fn bad(row:User) {consume(&row);}", "E0271"),
        ("missing nested title", "fn consume<S,P>(_:&S) where S:models::UserShape<Posts=orm_client::LoadedMany<P>>,P:models::PostShape<Title=String> {} async fn bad(db:&Database) {let row=User::objects.load(User::posts.load(Post::views)).require().using(db).first().await.unwrap();consume(&row);}", "E0271"),
        ("missing root shape requirement", "fn consume<S:models::UserShape<Email=String>>(_:S) {} async fn bad(db:&Database) {let row=User::objects.load(User::nickname).require().using(db).first().await.unwrap();consume(row);}", "E0271"),
        ("runtime row does not satisfy static requirements", "fn consume<S:models::UserShape<Email=String>>(_:S) {} async fn bad(db:&Database) {let row=User::objects.runtime(&[\"email\"]).unwrap().require().using(db).first().await.unwrap();consume(row);}", "E0277"),
        ("runtime strings create no named fields", "fn bad(row:orm_client::RuntimeRow<User>) {let _=&row.email;}", "E0609"),
        ("scalar labels forbidden", "fn bad() {User::email.label(\"alias\");}", "E0599"),
        ("wrong selection root", "fn bad() {User::objects.load(Post::title);}", "E0277"),
        ("wrong relation target", "fn bad() {User::posts.as_prefetch(User::objects.query());}", "E0271"),
        ("unloaded caches cannot be read", "fn bad(row:User) {row.posts.cached();}", "E0599"),
        ("mixed static rows need explicit erasure", "fn bad(a:User,b:User<Unloaded,Loaded,Unloaded,Unloaded>,flag:bool) {let row=if flag {a} else {b};}", "E0308"),
        ("duplicate literal views", "async fn bad(row:User,db:&Database) {orm_client::relation_views!(row,db; recent=>User::posts,recent=>User::posts).await;}", "E0124|E0403|E0415"),
        ("named views are immutable", "async fn bad(row:User,db:&Database) {let mut view=orm_client::relation_views!(row,db; recent=>User::posts).await.unwrap();view.row.email.clear();}", "E0596"),

        ("direct shape requirements", "fn email<S:models::UserShape<Email=String>>(row:&S)->&String {row.email()} async fn check(db:&Database) {let partial=User::objects.load(User::email).require().using(db).first().await.unwrap(); email(&partial);let full=User::objects.require().using(db).first().await.unwrap();email(&full);}", ""),
        ("nested shape requirements", "fn titles<S,P>(row:&S) where S:models::UserShape<Posts=orm_client::LoadedMany<P>>, P:models::PostShape<Title=String> {for post in row.posts().cached() {let _: &String=post.title();}} async fn check(db:&Database) {let row=User::objects.load(User::posts.load(Post::title)).require().using(db).first().await.unwrap();titles(&row);let full=User::objects.load(User::posts).require().using(db).first().await.unwrap();titles(&full);}", ""),

        ("associated query root infers default shape", "async fn check(db:&Database) { let _:Option<User>=User::objects.first().await.unwrap(); let _:Option<User>=User::objects.using(db).first().await.unwrap(); let _:Vec<User>=User::objects.filter(User::email.eq(\"reader\".into())).await.unwrap(); }", ""),
        ("direct shape bounds accept partial and full", "fn email<S:models::UserShape<Email=String,Posts=orm_client::RelatedSet<Post>>>(row:&S)->&String { row.email() } struct Reader<S:models::UserShape<Email=String,Posts=orm_client::RelatedSet<Post>>> { row:S } fn check(a:User,b:User<Unloaded,Loaded,Unloaded,Unloaded>) { let _=email(&a); let _=email(&b); let _=Reader{row:a}; let _=Reader{row:b}; }", ""),
        ("shape bound rejects unloaded relation cache", "struct LoadedPosts; fn posts<S:models::UserShape<Posts=LoadedPosts>>(_:S) {} fn bad(row:User) { posts(row); }", "E0271"),
        ("shape bound rejects unloaded", "fn email<S:models::UserShape<Email=String,Posts=orm_client::RelatedSet<Post>>>(_:S) {} fn bad(row:User<Loaded,Unloaded,Loaded,Loaded>) { email(row); }", "E0271"),
        ("shape bound rejects wrong field type", "fn email<S:models::UserShape<Email=i32>>(_:S) {} fn bad(row:User) { email(row); }", "E0271"),
        ("arbitrary composite keys and enum names matching generics", "async fn check(db:&Database,path:models::KeyProbePath<models::UserSource,orm_client::One>) { let _=models::KeyProbe::objects.filter(models::KeyProbe::ST0.eq(models::S0::Value)).using(db).get(models::KeyProbe::A_B_C_D_E_KEY,(1,2,3,4,5)).await; let _:orm_client::Predicate<models::UserSource>=path.root().eq(models::R::Value).and(path.cardinal().eq(models::C::Value)); }", ""),
        ("composite key arity", "async fn bad(db:&Database) { models::KeyProbe::objects.using(db).get(models::KeyProbe::A_B_C_D_E_KEY,(1,2)).await; }", "E0308"),
        ("ordinary borrow/clone, nullable, copy, marker ownership", "fn use_rows(row:User, partial:User<Unloaded,Loaded,Unloaded,Unloaded>) { let text:&String=&row.email; let cloned:String=row.email.clone(); let id:i32=row.id; let nullable:&Option<String>=&row.nickname; let _marker=&partial.id; let moved=partial.id; }", ""),
        ("custom methods preserve query state", "trait Queries: Sized { fn readers(self)->Self; } impl<M:orm_client::Model<Source=models::UserSource>,R,L,D> Queries for orm_client::Query<M,R,L,D> { fn readers(self)->Self { self.filter(User::TYPE.eq(\"reader\".into())) } } async fn check(db:&Database) { let a:Vec<User>=User::objects.readers().require().using(db).await.unwrap(); let b:Vec<User>=User::objects.require().readers().using(db).await.unwrap(); let c=User::objects.readers().load((User::email,User::posts.load(Post::title))).require().using(db).first().await.unwrap(); let d=User::objects.load((User::email,User::posts.load(Post::title))).readers().require().using(db).first().await.unwrap(); }", ""),
        ("unloaded value method", "fn bad(row:User<Unloaded,Loaded,Unloaded,Unloaded>) { row.id.to_string(); }", "E0599"),
        ("unloaded display", "fn bad(row:User<Unloaded,Loaded,Unloaded,Unloaded>) { println!(\"{}\",row.id); }", "E0277"),
        ("unloaded deref", "fn bad(row:User<Unloaded,Loaded,Unloaded,Unloaded>) { let _=*row.id; }", "E0614"),
        ("unloaded conversion", "fn bad(row:User<Unloaded,Loaded,Unloaded,Unloaded>) { let _:i32=row.id.into(); }", "E0277"),
        ("direct field mutation", "fn bad(mut row:User) { row.id=7; }", "E0594"),
        ("mutable ordinary method", "fn bad(mut row:User) { row.email.clear(); }", "E0596"),
        ("mutable field borrow", "fn bad(mut row:User) { let _=&mut row.email; }", "E0596"),
        ("noncopy move", "fn bad(row:User) { let moved:String=row.email; drop(moved); }", "E0507"),
        ("forged identity", "fn bad(row:User) { let _=row.__orm_identity; }", "E0616"),
        ("forged state", "struct Fake; impl orm_client::State<i32> for Fake {type Value=i32; const LOADED:bool=true; fn read(_: &orm_client::DecodedRow<'_>,_:&str)->orm_client::Result<i32> {Ok(7)} }", "E0277"),
        ("wrong filter model", "fn bad() { User::objects.filter(Post::ID.eq(1)); }", "E0308"),
        ("wrong filter value", "fn bad() { User::ID.eq(\"1\".to_owned()); }", "E0308"),
        ("wrong field source", "fn bad() { User::ID.eq_field(Post::ID); }", "E0308"),
        ("to-many ordering", "fn bad() { User::objects.order_by(User::posts_path().views().asc()); }", "E0599"),
        ("many remains many through one", "fn bad() { User::objects.order_by(User::posts_path().author().email().asc()); }", "E0599"),
        ("sliced union", "fn bad() { User::objects.limit(1).or(User::objects); }", "E0599"),
        ("sliced bulk", "async fn bad(db:&Database) { User::objects.limit(1).in_bulk(User::ID_KEY,[1],db).await; }", "E0599"),
        ("wrong unique type", "async fn bad(db:&Database) { User::objects.using(db).get(User::ID_KEY,\"1\".to_owned()).await; }", "E0308"),
        ("wrong unique model", "async fn bad(db:&Database) { User::objects.using(db).get(Post::ID_KEY,1).await; }", "E0308"),
        ("unsupported dialect/schema handle", "enum Other {} impl orm_client::SchemaSpec for Other {type Dialect=orm_client::Postgres; const JSON:&'static str=\"\";} fn bad(db:&orm_client::Database<Other>) { User::objects.using(db); }", "E0308"),
        ("required optionality", "async fn bad(db:&Database) { let _:Option<User>=User::objects.require().using(db).first().await.unwrap(); }", "E0308"),
        ("statement executes once", "async fn bad(db:&Database) { let s=User::objects.using(db).statement().unwrap(); s.execute().await; s.execute().await; }", "E0382"),
    ];
    for (name, body, category) in cases {
        std::fs::write(dir.join("src/lib.rs"), format!("{prefix}{body}\n")).unwrap();
        let output = Command::new("cargo")
            .args(["check", "--offline", "--quiet", "--manifest-path"])
            .arg(dir.join("Cargo.toml"))
            .env(
                "CARGO_TARGET_DIR",
                root.parent().unwrap().parent().unwrap().join("target/consumer-checks"),
            )
            .output()
            .unwrap();
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        // `A|B` accepts any listed code: rustc reports resolution errors (E0403, E0415) before E0124 for one duplicate label.
        if category.is_empty() {
            assert!(output.status.success(), "{name}: {diagnostics}");
        } else {
            assert!(
                !output.status.success()
                    && category.split('|').any(|code| diagnostics.contains(code)),
                "{name} expected {category}: {diagnostics}"
            );
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
