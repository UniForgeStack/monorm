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
    let prefix="#![allow(dead_code,unused_variables,unused_mut)]\nmod models;\nuse models::{User,Post,Database};\nuse orm_client::{Loaded,Unloaded};\n";
    let cases=[
        ("associated query root infers default shape", "async fn check(db:&Database) { let _:Option<User>=User::objects.first().await.unwrap(); let _:Option<User>=User::objects.using(db).first().await.unwrap(); let _:Vec<User>=User::objects.filter(User::email.eq(\"reader\".into())).await.unwrap(); }", ""),
        ("direct shape bounds accept partial and full", "fn email<S:models::UserShape<Email=String,Posts=Unloaded>>(row:&S)->&String { row.email() } struct Reader<S:models::UserShape<Email=String,Posts=Unloaded>> { row:S } fn check(a:User,b:User<Unloaded,Loaded,Unloaded,Unloaded>) { let _=email(&a); let _=email(&b); let _=Reader{row:a}; let _=Reader{row:b}; }", ""),
        ("shape bound rejects unloaded relation cache", "struct LoadedPosts; fn posts<S:models::UserShape<Posts=LoadedPosts>>(_:S) {} fn bad(row:User) { posts(row); }", "E0271"),
        ("shape bound rejects unloaded", "fn email<S:models::UserShape<Email=String,Posts=Unloaded>>(_:S) {} fn bad(row:User<Loaded,Unloaded,Loaded,Loaded>) { email(row); }", "E0271"),
        ("shape bound rejects wrong field type", "fn email<S:models::UserShape<Email=i32>>(_:S) {} fn bad(row:User) { email(row); }", "E0271"),
        ("arbitrary composite keys and enum names matching generics", "async fn check(db:&Database,path:models::KeyProbePath<models::UserSource,orm_client::One>) { let _=models::KeyProbe::objects.filter(models::KeyProbe::ST0.eq(models::S0::Value)).using(db).get(models::KeyProbe::A_B_C_D_E_KEY,(1,2,3,4,5)).await; let _:orm_client::Predicate<models::UserSource>=path.root().eq(models::R::Value).and(path.cardinal().eq(models::C::Value)); }", ""),
        ("composite key arity", "async fn bad(db:&Database) { models::KeyProbe::objects.using(db).get(models::KeyProbe::A_B_C_D_E_KEY,(1,2)).await; }", "E0308"),
        ("ordinary borrow/clone, nullable, copy, marker ownership", "fn use_rows(row:User, partial:User<Unloaded,Loaded,Unloaded,Unloaded>) { let text:&String=&row.email; let cloned:String=row.email.clone(); let id:i32=row.id; let nullable:&Option<String>=&row.nickname; let _marker=&partial.id; let moved=partial.id; }", ""),
        ("custom methods preserve query state", "trait Queries: Sized { fn readers(self)->Self; } impl<M:orm_client::Model<Source=models::UserSource>,R,L> Queries for orm_client::Query<M,R,L> { fn readers(self)->Self { self.filter(User::TYPE.eq(\"reader\".into())) } } async fn check(db:&Database) { let a:Vec<User>=User::objects.readers().require().using(db).await.unwrap(); let b:Vec<User>=User::objects.require().readers().using(db).await.unwrap(); }", ""),
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
        ("to-many ordering", "fn bad() { User::objects.order_by(User::posts().views().asc()); }", "E0599"),
        ("many remains many through one", "fn bad() { User::objects.order_by(User::posts().author().email().asc()); }", "E0599"),
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
                root.parent().unwrap().join("target/consumer-checks"),
            )
            .output()
            .unwrap();
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        if category.is_empty() {
            assert!(output.status.success(), "{name}: {diagnostics}");
        } else {
            assert!(
                !output.status.success() && diagnostics.contains(category),
                "{name} expected {category}: {diagnostics}"
            );
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
