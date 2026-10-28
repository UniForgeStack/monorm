#![allow(dead_code)]
mod sqlite {
    include!(concat!(env!("OUT_DIR"), "/models.rs"));
}
mod postgres {
    include!(concat!(env!("OUT_DIR"), "/postgres.rs"));
}
mod exported {
    monorm_client::named_result! {pub struct Contact {email, nickname}}
}

macro_rules! exercise {
    ($module:ident,$url:expr) => {{
        use $module::{Database,User};
        use monorm_client::Error;
        let raw=monorm_engine::db::connect($url,1).await.unwrap();
        raw.batch("DROP TABLE IF EXISTS rust_read_posts; DROP TABLE IF EXISTS rust_read_users; CREATE TABLE rust_read_users(id INTEGER PRIMARY KEY,email TEXT NOT NULL,nickname TEXT,kind TEXT NOT NULL); INSERT INTO rust_read_users VALUES(1,'a',NULL,'reader'),(2,'b','Bee','writer');".into()).await.unwrap();
        raw.close().await;
        let db=Database::connect($url,1).await.unwrap();
        fn email<S:$module::UserShape<Email=String>>(row:&S)->&String {row.email()}
        let model=monorm_client::select_named!(User::objects.filter(User::id.eq(1)).require(); before=>User::id,person=>monorm_client::model::<User>(),after=>User::nickname).using(&db).first().await.unwrap();
        assert_eq!(model.before,1);assert_eq!(email(&model.person),"a");assert_eq!(model.after,None);
        let model=User::objects.filter(User::id.eq(1)).select((monorm_client::model::<User<monorm_client::Unloaded,monorm_client::Loaded,monorm_client::Unloaded,monorm_client::Unloaded>>(),)).require().using(&db).first().await.unwrap();
        assert_eq!(email(&model.0),"a");
        let annotation=monorm_client::select_named!(User::objects.filter(User::id.eq(1)); upper=>User::email.column().upper(),size=>User::nickname.column().length()).require().using(&db).first().await.unwrap();
        assert_eq!(annotation.upper,"A");assert_eq!(annotation.size,None);
        let length:i32=User::objects.filter(User::id.eq(2)).select(User::nickname.column().upper().length()).require().using(&db).scalar().await.unwrap().unwrap();assert_eq!(length,3);
        let query=User::objects.order_by(User::id.asc()).select((User::email,User::nickname));
        let rows:Vec<(String,Option<String>)>=query.clone().using(&db).await.unwrap();
        assert_eq!(rows,vec![("a".into(),None),("b".into(),Some("Bee".into()))]);
        let statement=query.require().using(&db).statement().unwrap();
        assert!(statement.sql().contains("SELECT"));
        assert!(!format!("{:?}",statement.inspect()).is_empty());
        assert_eq!(statement.execute().await.unwrap(),rows);
        let query=User::objects.filter(User::id.eq(1)).select(User::nickname);
        let null:Option<Option<String>>=query.clone().using(&db).scalar().await.unwrap();
        assert_eq!(null,Some(None));
        let null:Option<String>=query.clone().require().using(&db).scalar().await.unwrap();
        assert_eq!(null,None);
        let absent:Option<Option<String>>=query.clone().filter(User::id.eq(99)).using(&db).scalar().await.unwrap();
        assert_eq!(absent,None);
        assert!(matches!(query.clone().filter(User::id.eq(99)).require().using(&db).scalar().await,Err(Error::NotFound)));
        assert!(matches!(query.clone().filter(User::id.eq(99)).require().using(&db).scalars().await,Err(Error::NotFound)));
        assert!(matches!(query.clone().limit(0).require().using(&db).first().await,Err(Error::NotFound)));
        assert!(matches!(query.clone().filter(User::id.eq(99)).require().using(&db).statement().unwrap().execute().await,Err(Error::NotFound)));
        assert_eq!(query.limit(0).using(&db).scalar().await.unwrap(),None);
        let values:Vec<Option<String>>=User::objects.require().select(User::nickname).order_by(User::id.asc()).using(&db).scalars().await.unwrap();
        assert_eq!(values,vec![None,Some("Bee".into())]);
        let values=User::objects.order_by(User::id.asc()).select((User::nickname,)).using(&db).scalars().await.unwrap();
        assert_eq!(values,vec![None,Some("Bee".into())]);
        let one=monorm_client::select_named!(User::objects.filter(User::id.eq(1)).require(); address=>User::email, alias=>User::nickname).using(&db).first().await.unwrap();
        assert_eq!(one.address,"a");assert_eq!(one.alias,None);
        let one:exported::Contact<String,Option<String>>=monorm_client::select_named!(User::objects.filter(User::id.eq(2)) => exported::Contact;email=>User::email,nickname=>User::nickname).require().using(&db).first().await.unwrap();
        assert_eq!(one.email,"b");assert_eq!(one.nickname.as_deref(),Some("Bee"));
        let prepared=monorm_client::select_named!(User::objects.filter(User::id.eq(1));person=>monorm_client::model::<User>(),nickname=>User::nickname).prepare().require();
        let row=prepared.using(&db).first().await.unwrap();assert_eq!(email(&row.person),"a");assert_eq!(row.nickname,None);
        let nullable=User::objects.filter(User::id.eq(1)).select(User::nickname).require().prepare();
        let before:Option<String>=nullable.using(&db).scalar().await.unwrap();assert_eq!(before,None);
        assert_eq!(nullable.using(&db).scalars().await.unwrap(),vec![None]);
        assert_eq!(nullable.using(&db).statement().unwrap().inspect().statement_count,1);
        let empty=User::objects.filter(User::id.eq(99)).select(User::nickname).prepare().require();
        assert!(matches!(empty.using(&db).scalar().await,Err(Error::NotFound)));
        assert!(matches!(empty.using(&db).statement().unwrap().execute().await,Err(Error::NotFound)));
        // Fresh terminals read again, and values remain bound through the planner.
        let fresh=User::objects.filter(User::email.eq("a' OR 1=1 --".into())).select(User::id);
        assert!(fresh.using(&db).all().await.unwrap().is_empty());
        let raw=monorm_engine::db::connect($url,1).await.unwrap();
        raw.batch("UPDATE rust_read_users SET nickname='new' WHERE id=1".into()).await.unwrap();raw.close().await;
        assert_eq!(User::objects.filter(User::id.eq(1)).select(User::nickname).require().using(&db).scalar().await.unwrap(),Some("new".into()));
        let raw=monorm_engine::db::connect($url,1).await.unwrap();
        raw.batch("DROP TABLE IF EXISTS rust_load_books; DROP TABLE IF EXISTS rust_load_authors; CREATE TABLE rust_load_authors(id INTEGER PRIMARY KEY,name TEXT NOT NULL,bio TEXT NOT NULL,note TEXT,visible BOOLEAN NOT NULL); CREATE TABLE rust_load_books(id INTEGER PRIMARY KEY,author_id INTEGER NOT NULL,editor_id INTEGER,title TEXT NOT NULL); INSERT INTO rust_load_authors VALUES(1,'Alice','bio',NULL,true); INSERT INTO rust_load_books VALUES(1,1,NULL,'first'),(2,1,1,'second');".into()).await.unwrap();raw.close().await;
        use $module::PolicyBook;
        let optional:Vec<Option<String>>=PolicyBook::objects.without_related().order_by(PolicyBook::id.asc()).select(PolicyBook::editor_path().name()).require().using(&db).scalars().await.unwrap();
        assert_eq!(optional,vec![None,Some("Alice".into())]);
        let nullable_values:Vec<Option<String>>=PolicyBook::objects.without_related().order_by(PolicyBook::id.asc()).select(PolicyBook::editor_path().note()).require().using(&db).scalars().await.unwrap();assert_eq!(nullable_values,vec![None,None]);
        let lengths:Vec<Option<i32>>=PolicyBook::objects.without_related().order_by(PolicyBook::id.asc()).select(PolicyBook::editor_path().name().upper().length()).require().using(&db).scalars().await.unwrap();assert_eq!(lengths,vec![None,Some(5)]);
        let filtered:Vec<(String,)>=PolicyBook::objects.without_related().filter(PolicyBook::editor_path().name().eq("Alice".into())).select(PolicyBook::title).using(&db).all().await.unwrap();assert_eq!(filtered,vec![("second".into(),)]);

        let raw=monorm_engine::db::connect($url,1).await.unwrap();
        raw.batch("DROP TABLE IF EXISTS rust_projection_accounts; DROP TABLE IF EXISTS rust_projection_people; CREATE TABLE rust_projection_people(id INTEGER PRIMARY KEY); CREATE TABLE rust_projection_accounts(id INTEGER PRIMARY KEY,person_id INTEGER NOT NULL UNIQUE,code TEXT NOT NULL); INSERT INTO rust_projection_people VALUES(1),(2); INSERT INTO rust_projection_accounts VALUES(1,2,'present');".into()).await.unwrap();raw.close().await;
        use $module::ProjectionPerson;
        let reverse:Vec<Option<String>>=ProjectionPerson::objects.order_by(ProjectionPerson::id.asc()).select(ProjectionPerson::account_path().code()).require().using(&db).scalars().await.unwrap();assert_eq!(reverse,vec![None,Some("present".into())]);
        let raw=monorm_engine::db::connect($url,1).await.unwrap();raw.batch("UPDATE rust_load_authors SET visible=false".into()).await.unwrap();raw.close().await;
        let hidden:Vec<Option<String>>=PolicyBook::objects.without_related().select(PolicyBook::author_path().name()).require().using(&db).scalars().await.unwrap();assert_eq!(hidden,vec![None,None]);
        // The existing planner rejects projection + loaded relations consistently.
        assert!(User::objects.load(User::posts).select(User::email).using(&db).all().await.is_err());
        let after=prepared.using(&db).first().await.unwrap();assert_eq!(after.nickname.as_deref(),Some("new"));assert_eq!(email(&after.person),"a");
        assert_eq!(nullable.using(&db).scalar().await.unwrap().as_deref(),Some("new"));
        let model_template=User::objects.filter(User::id.eq(1)).prepare().require();
        assert_eq!(model_template.using(&db).first().await.unwrap().email,"a");
        assert_eq!(model_template.using(&db).all().await.unwrap().len(),1);
        db.set_default();
        let _:Vec<(i32,)>=User::objects.select(User::id).await.unwrap();
        assert_eq!(User::objects.filter(User::id.eq(1)).select(User::nickname).require().scalar().await.unwrap(),Some("new".into()));
        Database::clear_default();db.close().await;
    }};
}
#[tokio::test]
async fn sqlite_projections() {
    let path = std::env::temp_dir().join(format!("monorm-rust-projections-{}.db", std::process::id()));
    let url = format!("sqlite://{}", path.display());
    exercise!(sqlite, &url);
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn postgres_projections() {
    let url =
        std::env::var("ORM_RUST_TEST_POSTGRES_URL").expect("isolated PostgreSQL URL required");
    exercise!(postgres, &url);
}
