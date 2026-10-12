#![allow(dead_code)]
mod sqlite {
    include!(concat!(env!("OUT_DIR"), "/models.rs"));
}
mod postgres {
    include!(concat!(env!("OUT_DIR"), "/postgres.rs"));
}
use orm_client::{BatchLoad, Error, Instance, Loaded, Unloaded};
use std::hash::{Hash, Hasher};
fn hash<T: Hash>(v: &T) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}
const FIXTURE:&str="DROP TABLE IF EXISTS rust_read_posts; DROP TABLE IF EXISTS rust_read_users; CREATE TABLE rust_read_users(id INTEGER PRIMARY KEY,email TEXT NOT NULL,nickname TEXT,kind TEXT NOT NULL); CREATE TABLE rust_read_posts(id INTEGER PRIMARY KEY,author_id INTEGER NOT NULL,title TEXT NOT NULL,views INTEGER NOT NULL); INSERT INTO rust_read_users VALUES (1,'a@example.com',NULL,'reader'),(2,'b@example.com','Bee','writer'),(3,'a@example.com','Duplicate','reader'); INSERT INTO rust_read_posts VALUES(1,1,'alpha',1),(2,1,'beta',20),(3,2,'alpha',20);";
macro_rules! exercise {
    ($module:ident,$url:expr) => {{
        use $module::{Database, Post, User};
        // Fixture setup is outside the read surface; all assertions use public reads.
        let fixture = orm_engine::db::connect($url, 1).await.unwrap();
        fixture.batch(FIXTURE.into()).await.unwrap();
        fixture.close().await;
        let db = Database::connect($url, 1).await.unwrap();
        let selected = User::objects.load(User::email).load(User::nickname).require().using(&db).get(User::ID_KEY, 1).await.unwrap();
        let _: &String = &selected.email;
        let _: &Option<String> = &selected.nickname;
        let _: &Unloaded = &selected.id;
        assert_eq!(selected.email, "a@example.com");
        let nested = User::objects.load((User::email, User::posts.load(Post::title))).require().using(&db).get(User::ID_KEY, 1).await.unwrap();
        assert_eq!(nested.posts.cached()[0].title, "alpha");
        assert_eq!(nested.posts.cached().len(), 2);
        let _: &Unloaded = &nested.posts.cached()[0].views;
        let upgraded = selected.load(User::posts.load(Post::title), &db).await.unwrap();
        assert_eq!(upgraded.posts.cached().len(), 2);
        assert_eq!(upgraded.nickname, None);
        let runtime = upgraded.into_runtime();
        assert!(!runtime.is_loaded("id"));
        assert_eq!(runtime.get::<String>("email").unwrap(), "a@example.com");
        assert!(matches!(runtime.get::<i32>("id"), Err(Error::Unloaded(_))));
        type Requested=User<Unloaded,Loaded,Unloaded,Unloaded,orm_client::Cached<Post<Unloaded,Unloaded,Loaded,Unloaded>>>;
        let restored:Requested=runtime.try_into_static().unwrap();
        assert_eq!(restored.posts.cached()[0].title, "alpha");
        let empty=Vec::<User>::new().prefetch(User::posts,&db).await.unwrap();
        assert!(empty.is_empty());
        let batch=User::objects.load(User::email).order_by(User::ID.asc()).using(&db).await.unwrap().prefetch(User::posts.load(Post::title),&db).await.unwrap();
        assert_eq!(batch[0].posts.cached().len(),2);
        assert!(batch[2].posts.cached().is_empty());
        let mut runtime=User::objects.runtime(&["nickname"]).unwrap().require().using(&db).get(User::ID_KEY,1).await.unwrap();
        assert_eq!(runtime.get::<Option<String>>("nickname").unwrap(),None);
        let rejected=runtime.try_into_static::<Requested>().unwrap_err();
        runtime=*rejected.original;
        runtime.load_runtime(&["email","posts.title"],&db,false).await.unwrap();
        let accepted:Requested=runtime.try_into_static().unwrap();
        assert_eq!(accepted.posts.cached().len(),2);
        assert_eq!(accepted.public_json().unwrap(),orm_client::serde_json::json!({"email":"a@example.com","nickname":null,"posts":[{"title":"alpha"},{"title":"beta"}]}));
        let all = User::objects.using(&db).await.unwrap();
        assert_eq!(all.len(), 3);
        let user = User::objects
            .using(&db)
            .get(User::ID_KEY, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(user.email, "a@example.com");
        assert_eq!(user.nickname, None);
        assert_eq!(user.r#type, "reader");
        assert_eq!(User::objects.filter(User::NICKNAME.eq(None)).using(&db).count().await.unwrap(),1);
        assert_eq!(User::objects.filter(User::NICKNAME.ne(None)).using(&db).count().await.unwrap(),2);
        let borrowed: &String = &user.email;
        assert_eq!(borrowed.clone(), "a@example.com");
        let partial = User::<Unloaded, Loaded, Unloaded, Unloaded>::query()
            .using(&db)
            .get(User::ID_KEY, 1)
            .await
            .unwrap()
            .unwrap();
        let hidden=User::<Unloaded,Unloaded,Unloaded,Unloaded>::query().using(&db).get(User::ID_KEY,1).await.unwrap().unwrap();
        assert!(user==hidden); assert_eq!(hash(&user),hash(&hidden));
        let helpers=User::<Loaded,Unloaded,Unloaded,Unloaded>::query().in_bulk(User::EMAIL_KEY,["b@example.com".to_owned()],&db).await.unwrap();
        assert_eq!(helpers["b@example.com"].id,2); let _unloaded=&helpers["b@example.com"].email;
        let _marker = &partial.id;
        assert!(user == partial);
        assert_eq!(hash(&user), hash(&partial));
        assert_eq!(
            User::objects
                .using(&db)
                .first()
                .await
                .unwrap()
                .unwrap()
                .id,
            1
        );
        assert_eq!(
            User::objects.using(&db).last().await.unwrap().unwrap().id,
            3
        );
        assert_eq!(
            User::objects
                .order_by(User::EMAIL.desc())
                .using(&db)
                .first()
                .await
                .unwrap()
                .unwrap()
                .id,
            2
        );
        assert!(User::objects
            .using(&db)
            .get(User::ID_KEY, 99)
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            User::objects
                .require()
                .using(&db)
                .get(User::ID_KEY, 99)
                .await,
            Err(Error::NotFound)
        ));
        // Generated unique tokens do not trust deployed database constraints.
        assert!(matches!(
            User::objects
                .using(&db)
                .get(User::EMAIL_KEY, "a@example.com".into())
                .await,
            Err(Error::MultipleRows)
        ));
        assert!(matches!(
            User::objects
                .limit(1)
                .using(&db)
                .get(User::EMAIL_KEY, "a@example.com".into())
                .await,
            Err(Error::MultipleRows)
        ));
        let missing = User::objects.filter(User::ID.eq(99));
        assert!(matches!(
            missing.clone().require().using(&db).all().await,
            Err(Error::NotFound)
        ));
        assert_eq!(
            missing.clone().require().using(&db).count().await.unwrap(),
            0
        );
        assert!(!missing.require().using(&db).exists().await.unwrap());
        assert_eq!(
            User::objects
                .slice(1, 3)
                .unwrap()
                .order_by(User::ID.asc())
                .using(&db)
                .all()
                .await
                .unwrap()
                .iter()
                .map(|u| u.id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        let bulk = User::<Unloaded, Loaded, Unloaded, Unloaded>::query()
            .in_bulk(User::ID_KEY, [1, 2, 99], &db)
            .await
            .unwrap();
        assert_eq!(bulk.len(), 2);
        assert_eq!(bulk[&2].email, "b@example.com");
        let same = User::posts_path()
            .title()
            .eq("alpha".into())
            .and(User::posts_path().views().gt(10));
        assert_eq!(
            User::objects
                .filter(same)
                .using(&db)
                .all()
                .await
                .unwrap()
                .iter()
                .map(|u| u.id)
                .collect::<Vec<_>>(),
            vec![2]
        );
        let separate = User::objects
            .filter(User::posts_path().title().eq("alpha".into()))
            .filter(User::posts_path().views().gt(10));
        assert_eq!(separate.clone().using(&db).count().await.unwrap(), 2);
        assert!(separate.or(User::objects).is_err());
        let either = User::objects
            .order_by(User::ID.desc())
            .filter(User::ID.eq(1))
            .or(User::objects.filter(User::ID.eq(2)))
            .unwrap();
        assert_eq!(
            either
                .using(&db)
                .all()
                .await
                .unwrap()
                .iter()
                .map(|u| u.id)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(
            User::objects
                .exclude(User::ID.eq(1).or(User::ID.eq(3)))
                .using(&db)
                .count()
                .await
                .unwrap(),
            1
        );
        let stmt = Post::objects
            .filter(Post::author_path().email().eq("b@example.com".into()))
            .using(&db)
            .statement()
            .unwrap();
        assert_eq!(stmt.inspect().statement_count, 1);
        assert!(stmt.sql().contains("rust_read_posts"));
        assert_eq!(stmt.execute().await.unwrap().len(), 1);
        assert_eq!(User::METADATA.fields[3].column, "kind");
        // Scalar codecs and composite unique tokens use the same public read boundary.
        let fixture=orm_engine::db::connect($url,1).await.unwrap();
        let pg=fixture.dialect().is_postgres();
        fixture.batch("DROP TABLE IF EXISTS rust_read_scalars".into()).await.unwrap();
        let failure=$module::Scalar::objects.using(&db).all().await;
        match failure {Err(Error::Engine(orm_engine::Error::Db(e)))=>{assert!(!e.message.is_empty());if pg {assert_eq!(e.sqlstate.as_deref(),Some("42P01"));}},_=>panic!("expected structured database failure")}
        let types=if pg {"flag boolean,big bigint,ratio double precision,moment timestamptz,day date,key uuid,payload jsonb"}else{"flag boolean,big bigint,ratio double precision,moment text,day text,key text,payload text"};
        fixture.batch(format!("CREATE TABLE rust_read_scalars(id integer PRIMARY KEY,status text,priority integer,{types}); INSERT INTO rust_read_scalars VALUES(1,'ACTIVE',10,{},9007199254740993,1.5,'{}','2026-10-10','a03807af-cc91-4a3e-a65d-f573c7b8f31a','{{\"ok\":true}}')",if pg {"true"}else{"1"},if pg {"2026-10-10 12:00:00+00"}else{"2026-10-10 12:00:00"})).await.unwrap();
        fixture.close().await;
        let scalar=$module::Scalar::objects.filter($module::Scalar::STATUS.eq($module::Status::Active)).using(&db).get($module::Scalar::STATUS_PRIORITY_KEY,($module::Status::Active,$module::Priority::High)).await.unwrap().unwrap();
        assert_eq!(scalar.big,9007199254740993);assert_eq!(scalar.priority,$module::Priority::High);assert!(scalar.flag);assert_eq!(scalar.ratio,1.5);assert_eq!(scalar.payload,orm_client::serde_json::json!({"ok":true}));assert_eq!(scalar.day.to_string(),"2026-10-10");assert_eq!(scalar.moment.timestamp(),1791633600);assert_eq!(scalar.key.to_string(),"a03807af-cc91-4a3e-a65d-f573c7b8f31a");
        assert!(matches!(User::objects.limit(u64::MAX).using(&db).all().await,Err(Error::Engine(orm_engine::Error::Value(_)))));
        assert_eq!(User::objects.order_by(User::ID.asc()).slice(0,2).unwrap().slice(1,3).unwrap().using(&db).all().await.unwrap().iter().map(|u|u.id).collect::<Vec<_>>(),vec![2]);
        assert_eq!(User::objects.slice(2,1).unwrap().using(&db).count().await.unwrap(),0);
        db.close().await;
    }};
}
#[tokio::test]
async fn sqlite_public_reads() {
    let path = std::env::temp_dir().join(format!("orm-rust-reads-{}.db", std::process::id()));
    let url = format!("sqlite://{}", path.display());
    exercise!(sqlite, &url);
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn postgres_public_reads() {
    let url = std::env::var("ORM_RUST_TEST_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost/orm_test".into());
    exercise!(postgres, &url);
    let fixture = orm_engine::db::connect(&url, 1).await.unwrap();
    fixture.batch("DROP TABLE IF EXISTS rust_read_extra; CREATE TABLE rust_read_extra(id integer PRIMARY KEY,amount numeric,tags text[],priorities integer[]); INSERT INTO rust_read_extra VALUES(1,12.50,ARRAY['a','b'],ARRAY[1,10]);".into()).await.unwrap();
    fixture.close().await;
    let db = postgres::Database::connect(&url, 1).await.unwrap();
    let row = postgres::Extra::objects
        .filter(postgres::Extra::TAGS.eq(vec!["a".into(), "b".into()]))
        .using(&db)
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.amount.to_string(), "12.50");
    assert_eq!(
        row.priorities,
        vec![postgres::Priority::Low, postgres::Priority::High]
    );
    assert_eq!(
        postgres::Extra::objects
            .filter(postgres::Extra::TAGS.eq(vec![]))
            .using(&db)
            .count()
            .await
            .unwrap(),
        0
    );
    db.close().await;
}
#[tokio::test]
async fn incompatible_url_is_rejected() {
    assert!(matches!(
        postgres::Database::connect("sqlite://:memory:", 1).await,
        Err(Error::Compatibility(_))
    ));
}
