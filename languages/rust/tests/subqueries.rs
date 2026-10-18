#![allow(dead_code)]
mod sqlite {
    include!(concat!(env!("OUT_DIR"), "/models.rs"));
}
mod postgres {
    include!(concat!(env!("OUT_DIR"), "/postgres.rs"));
}

const FIXTURE:&str="DROP TABLE IF EXISTS rust_read_posts; DROP TABLE IF EXISTS rust_read_users; CREATE TABLE rust_read_users(id INTEGER PRIMARY KEY,email TEXT NOT NULL,nickname TEXT,kind TEXT NOT NULL); CREATE TABLE rust_read_posts(id INTEGER PRIMARY KEY,author_id INTEGER NOT NULL,title TEXT NOT NULL,views INTEGER NOT NULL); INSERT INTO rust_read_users VALUES (1,'a@example.com',NULL,'reader'),(2,'b@example.com','Bee','writer'),(3,'a@example.com','Duplicate','reader'); INSERT INTO rust_read_posts VALUES(1,1,'alpha',1),(2,1,'beta',20),(3,2,'alpha',20);";

// `$checked_fails` is whether the database rejects a second row in a scalar subquery.
macro_rules! exercise {
    ($module:ident,$url:expr,$checked_fails:expr) => {{
        use $module::{Database,Post,User};
        use orm_client::{scalar,scalar_checked,Error,Parent,Subquery};
        let raw=orm_engine::db::connect($url,1).await.unwrap();
        raw.batch(FIXTURE.into()).await.unwrap();raw.close().await;
        let db=Database::connect($url,1).await.unwrap();
        let s=|v:&str|Some(v.to_owned());

        // A correlated scalar reads the enclosing row and is NULL when no row matches.
        let best_title=scalar(|u:Parent<User>|Subquery::new(Post::objects)
            .filter(Post::author_id.column().eq_outer(u.col(User::id.column())))
            .order_by(Post::views.desc()).then_by(Post::id.asc())
            .limit_one().select(Post::title));
        let rows:Vec<(i32,Option<String>)>=User::objects.order_by(User::id.asc()).select((User::id,best_title)).using(&db).all().await.unwrap();
        assert_eq!(rows,vec![(1,s("beta")),(2,s("alpha")),(3,None)]);

        // A unique key lookup proves at most one row, and a to-one outer column compares in either direction.
        let owner=scalar(|p:Parent<Post>|Subquery::new(User::objects).unique(User::ID_KEY,p.col(Post::author_id.column())).select(User::email));
        let rows:Vec<(i32,Option<String>)>=Post::objects.order_by(Post::id.asc()).select((Post::id,owner)).using(&db).all().await.unwrap();
        assert_eq!(rows,vec![(1,s("a@example.com")),(2,s("a@example.com")),(3,s("b@example.com"))]);
        let newer=scalar(|u:Parent<User>|Subquery::new(User::objects)
            .filter(User::id.column().gt_outer(u.col(User::id.column())))
            .order_by(User::id.asc()).limit_one().select(User::id));
        let rows:Vec<Option<i32>>=User::objects.order_by(User::id.asc()).select(newer).using(&db).scalars().await.unwrap();
        assert_eq!(rows,vec![Some(2),Some(3),None]);

        // Nested scopes: the innermost query reads its parent (depth 1) and the root (depth 2).
        // Root and innermost read the same model, so only distinct aliases keep them apart.
        let nested=scalar(|root:Parent<User>|Subquery::new(Post::objects).order_by(Post::id.asc()).limit_one().select(
            scalar(|post:Parent<Post>|Subquery::new(User::objects)
                .filter(User::id.column().eq_outer(post.col(Post::author_id.column())))
                .filter(User::email.column().eq_outer(root.col(User::email.column())))
                .limit_one().select(User::email))));
        let rows:Vec<(i32,Option<String>)>=User::objects.order_by(User::id.asc()).select((User::id,nested)).using(&db).all().await.unwrap();
        assert_eq!(rows,vec![(1,s("a@example.com")),(2,None),(3,s("a@example.com"))]);

        // The checked form accepts an unproven row count and leaves a second row to the database.
        let titles=||scalar_checked(|u:Parent<User>|Subquery::new(Post::objects)
            .filter(Post::author_id.column().eq_outer(u.col(User::id.column())))
            .select(Post::title));
        let single:Vec<(i32,Option<String>)>=User::objects.filter(User::id.eq(2)).select((User::id,titles())).using(&db).all().await.unwrap();
        assert_eq!(single,vec![(2,s("alpha"))]);
        let many=User::objects.order_by(User::id.asc()).select((User::id,titles())).using(&db).all().await;
        assert_eq!(many.is_err(),$checked_fails);

        // A predicate that leaves its closure has no enclosing query at erasure: an error, not a wrong depth.
        let mut leaked=None;
        let _=scalar(|u:Parent<User>|{
            let p=Post::author_id.column().eq_outer(u.col(User::id.column()));
            leaked=Some(p.clone());
            Subquery::new(Post::objects).filter(p).limit_one().select(Post::title)
        });
        let stray=scalar(|_:Parent<User>|Subquery::new(Post::objects).filter(leaked.take().unwrap()).limit_one().select(Post::title));
        let err=User::objects.select((User::id,stray)).using(&db).all().await;
        assert!(matches!(&err,Err(Error::Compatibility(m)) if m.contains("unresolved outer source")),"{err:?}");
        db.close().await;
    }};
}
#[tokio::test]
async fn sqlite_subqueries() {
    let path = std::env::temp_dir().join(format!("orm-rust-subqueries-{}.db", std::process::id()));
    let url = format!("sqlite://{}", path.display());
    exercise!(sqlite, &url, false);
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn postgres_subqueries() {
    let url =
        std::env::var("ORM_RUST_TEST_POSTGRES_URL").expect("isolated PostgreSQL URL required");
    exercise!(postgres, &url, true);
}
