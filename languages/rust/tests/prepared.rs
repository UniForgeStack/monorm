#![allow(dead_code)]
mod sqlite {
    include!(concat!(env!("OUT_DIR"), "/models.rs"));
}
mod postgres {
    include!(concat!(env!("OUT_DIR"), "/postgres.rs"));
}
orm_client::params! {mod range {lo: i32, hi: i32}}
orm_client::params! {mod pick {who: String}}

/// Hand-written packs reach the runtime guards that `params!` packs satisfy statically.
struct Raw(Vec<(&'static str, std::any::TypeId, orm_client::BindValue)>);
impl orm_client::Has<range::lo> for Raw {}
impl orm_client::Has<range::hi> for Raw {}
impl orm_client::Pack for Raw {
    fn values(&self) -> Vec<(&'static str, std::any::TypeId, orm_client::BindValue)> {
        self.0.clone()
    }
}

macro_rules! exercise {
    ($module:ident,$url:expr) => {{
        use $module::{Database,ScopedNote,User};
        use orm_client::Error;
        use std::any::TypeId;
        let raw=orm_engine::db::connect($url,1).await.unwrap();
        raw.batch("DROP TABLE IF EXISTS rust_read_posts; DROP TABLE IF EXISTS rust_read_users; DROP TABLE IF EXISTS rust_scoped_notes; CREATE TABLE rust_read_users(id INTEGER PRIMARY KEY,email TEXT NOT NULL,nickname TEXT,kind TEXT NOT NULL); INSERT INTO rust_read_users VALUES(1,'a',NULL,'r'),(2,'b','B','w'),(3,'c',NULL,'r'),(4,'d','D','w'); CREATE TABLE rust_scoped_notes(id INTEGER PRIMARY KEY,shop INTEGER NOT NULL,title TEXT NOT NULL); INSERT INTO rust_scoped_notes VALUES(1,1,'a'),(2,1,'b'),(3,2,'c');".into()).await.unwrap();
        raw.close().await;
        let db=Database::connect($url,1).await.unwrap();
        let ids=|rows:Vec<User>|rows.iter().map(|u|u.id).collect::<Vec<_>>();
        // A slot used twice takes one value; the template repeats with new values.
        let template=User::objects.order_by(User::id.asc()).prepare().filter(User::id.column().ge_param(range::lo()).and(User::id.column().ne_param(range::lo()))).filter(User::id.column().le_param(range::hi()));
        assert_eq!(ids(template.bind(&range::Args{lo:1,hi:3}).unwrap().using(&db).all().await.unwrap()),vec![2,3]);
        assert_eq!(ids(template.bind(&range::Args{lo:2,hi:4}).unwrap().using(&db).all().await.unwrap()),vec![3,4]);
        // Projected templates bind the same way and keep requiredness.
        let projected=User::objects.order_by(User::id.asc()).select((User::id,User::email)).require().prepare().filter(User::id.column().gt_param(range::lo()).or(User::id.column().lt_param(range::hi())));
        let rows:Vec<(i32,String)>=projected.bind(&range::Args{lo:3,hi:2}).unwrap().using(&db).await.unwrap();
        assert_eq!(rows,vec![(1,"a".into()),(4,"d".into())]);
        let by_name=User::objects.prepare().filter(User::email.column().eq_param(pick::who()));
        assert_eq!(ids(by_name.bind(&pick::Args{who:"c".into()}).unwrap().using(&db).all().await.unwrap()),vec![3]);
        // Values stay bound: the text matches no row.
        let statement=by_name.bind(&pick::Args{who:"x' OR 1=1".into()}).unwrap().using(&db).statement().unwrap();
        assert_eq!(statement.execute().await.unwrap().len(),0);
        // Runtime guards: conflicting types, missing and NULL values.
        let lo=orm_client::BindValue::Int(Some(1));
        let conflict=Raw(vec![("lo",TypeId::of::<String>(),lo.clone()),("hi",TypeId::of::<i32>(),lo.clone())]);
        assert!(matches!(template.bind(&conflict),Err(Error::Compatibility(m)) if m.contains("conflicting types for parameter lo")));
        let twice=Raw(vec![("lo",TypeId::of::<i32>(),lo.clone()),("lo",TypeId::of::<i32>(),lo.clone()),("hi",TypeId::of::<i32>(),lo.clone())]);
        assert!(matches!(template.bind(&twice),Err(Error::Compatibility(_))));
        let missing=Raw(vec![("lo",TypeId::of::<i32>(),lo.clone())]);
        assert!(matches!(template.bind(&missing),Err(Error::Compatibility(m)) if m.contains("missing value for parameter hi")));
        let null=Raw(vec![("lo",TypeId::of::<i32>(),orm_client::BindValue::Int(None)),("hi",TypeId::of::<i32>(),lo.clone())]);
        assert!(matches!(template.bind(&null),Err(Error::Engine(e)) if e.to_string().contains("can't be None")));
        // A scope value lives in the template; `bind(..)?.scope(..)` replaces it for one execution.
        let notes=ScopedNote::objects.order_by(ScopedNote::id.asc()).prepare();
        let titles=|rows:Vec<ScopedNote>|rows.iter().map(|n|n.title.clone()).collect::<Vec<_>>();
        assert_eq!(titles(notes.clone().scope("shop",1).using(&db).all().await.unwrap()),vec!["a","b"]);
        assert_eq!(titles(notes.clone().scope("shop",2).using(&db).all().await.unwrap()),vec!["c"]);
        assert_eq!(titles(notes.clone().scope("shop",1).scope("shop",2).using(&db).all().await.unwrap()),vec!["c"]);
        assert!(matches!(notes.using(&db).all().await,Err(Error::Engine(_))));
        assert!(matches!(notes.clone().scope("shop",None::<i32>).using(&db).all().await,Err(Error::Engine(_))));
        let projected=ScopedNote::objects.order_by(ScopedNote::id.asc()).select(ScopedNote::title).prepare();
        let titles:Vec<String>=projected.clone().scope("shop",2).using(&db).scalars().await.unwrap();
        assert_eq!(titles,vec!["c"]);
        assert!(projected.using(&db).scalars().await.is_err());
        let bound=ScopedNote::objects.order_by(ScopedNote::id.asc()).prepare().filter(ScopedNote::title.column().eq_param(pick::who()));
        assert_eq!(bound.bind(&pick::Args{who:"b".into()}).unwrap().scope("shop",1).using(&db).all().await.unwrap().len(),1);
        assert_eq!(bound.bind(&pick::Args{who:"b".into()}).unwrap().scope("shop",2).using(&db).all().await.unwrap().len(),0);
        // A scope entry stays at the root of an `or`: it is no filter, and a right-side scope moves to the root.
        let note_titles=|rows:Vec<ScopedNote>|rows.iter().map(|n|n.title.clone()).collect::<Vec<_>>();
        let scoped=||ScopedNote::objects.order_by(ScopedNote::id.asc()).filter(ScopedNote::title.eq("a".to_string())).scope("shop",1);
        let either=scoped().or(ScopedNote::objects.filter(ScopedNote::title.eq("b".to_string()))).unwrap();
        assert_eq!(note_titles(either.using(&db).all().await.unwrap()),vec!["a","b"]);
        let right=ScopedNote::objects.order_by(ScopedNote::id.asc()).filter(ScopedNote::title.eq("a".to_string())).or(ScopedNote::objects.filter(ScopedNote::title.eq("b".to_string())).scope("shop",1)).unwrap();
        assert_eq!(note_titles(right.using(&db).all().await.unwrap()),vec!["a","b"]);
        let both=scoped().or(ScopedNote::objects.filter(ScopedNote::title.eq("b".to_string())).scope("shop",2));
        assert!(matches!(&both,Err(Error::Compatibility(m)) if m.contains("both sides")));
        // A scope on a subquery is an error, not a dropped value.
        let inner=orm_client::scalar(|_:orm_client::Parent<User>|orm_client::Subquery::new(ScopedNote::objects.scope("shop",1)).limit_one().select(ScopedNote::title));
        let err=User::objects.select((User::id,inner)).using(&db).all().await;
        assert!(matches!(&err,Err(Error::Compatibility(m)) if m.contains("scope shop on a subquery")),"{err:?}");
        db.close().await;
    }};
}
#[tokio::test]
async fn sqlite_prepared_parameters() {
    let path = std::env::temp_dir().join(format!("orm-rust-prepared-{}.db", std::process::id()));
    let url = format!("sqlite://{}", path.display());
    exercise!(sqlite, &url);
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn postgres_prepared_parameters() {
    let url =
        std::env::var("ORM_RUST_TEST_POSTGRES_URL").expect("isolated PostgreSQL URL required");
    exercise!(postgres, &url);
}
