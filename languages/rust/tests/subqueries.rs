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
        use monorm_client::{exists,in_subquery,not_in_subquery,scalar,scalar_checked,Error,Parent,Subquery};
        let raw=monorm_engine::db::connect($url,1).await.unwrap();
        raw.batch(FIXTURE.into()).await.unwrap();raw.close().await;
        let db=Database::connect($url,1).await.unwrap();
        let s=|v:&str|Some(v.to_owned());

        // A correlated scalar reads the enclosing row and is NULL when no row matches.
        let best_title=scalar(|u:Parent<User>|Subquery::new(Post::objects)
            .filter(Post::author_id.column().eq_outer(u.col(User::id.column())))
            .order_by(Post::views.desc()).then_by(Post::id.asc())
            .limit_one().select(Post::title));
        let rows:Vec<(i32,Option<String>)>=User::objects.order_by(User::id.asc()).select((User::id,best_title.clone())).using(&db).all().await.unwrap();
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

        // EXISTS reads the enclosing row; `!` negates it; an uncorrelated one is all or nothing.
        let ids=|rows:Vec<i32>|rows;
        let has_big_post=||exists(|u:Parent<User>|Subquery::new(Post::objects)
            .filter(Post::author_id.column().eq_outer(u.col(User::id.column())))
            .filter(Post::views.ge(20)));
        let rows:Vec<i32>=User::objects.filter(has_big_post()).order_by(User::id.asc()).select(User::id).using(&db).scalars().await.unwrap();
        assert_eq!(ids(rows),vec![1,2]);
        let rows:Vec<i32>=User::objects.filter(!has_big_post()).order_by(User::id.asc()).select(User::id).using(&db).scalars().await.unwrap();
        assert_eq!(rows,vec![3]);
        let any_post=|min:i32|exists(|_:Parent<User>|Subquery::new(Post::objects).filter(Post::views.gt(min)));
        let rows:Vec<i32>=User::objects.filter(any_post(10)).order_by(User::id.asc()).select(User::id).using(&db).scalars().await.unwrap();
        assert_eq!(rows,vec![1,2,3]);
        let rows:Vec<i32>=User::objects.filter(any_post(100)).select(User::id).using(&db).scalars().await.unwrap();
        assert!(rows.is_empty());

        // IN and NOT IN take one column of the subquery, correlated or not.
        let big_authors=||in_subquery(User::id.column(),|_:Parent<User>|Subquery::new(Post::objects).filter(Post::views.ge(20)).select(Post::author_id));
        let rows:Vec<i32>=User::objects.filter(big_authors()).order_by(User::id.asc()).select(User::id).using(&db).scalars().await.unwrap();
        assert_eq!(rows,vec![1,2]);
        let rows:Vec<i32>=User::objects.filter(not_in_subquery(User::id.column(),|_:Parent<User>|Subquery::new(Post::objects).select(Post::author_id))).select(User::id).using(&db).scalars().await.unwrap();
        assert_eq!(rows,vec![3]);
        let twin=in_subquery(User::email.column(),|u:Parent<User>|Subquery::new(User::objects)
            .filter(User::id.column().ne_outer(u.col(User::id.column()))).select(User::email));
        let rows:Vec<i32>=User::objects.filter(twin).order_by(User::id.asc()).select(User::id).using(&db).scalars().await.unwrap();
        assert_eq!(rows,vec![1,3]);
        // A nullable operand against a nullable column: NULL is never IN, so user 1 drops out.
        let named=in_subquery(User::nickname.column(),|_:Parent<User>|Subquery::new(User::objects).select(User::nickname));
        let rows:Vec<i32>=User::objects.filter(named).order_by(User::id.asc()).select(User::id).using(&db).scalars().await.unwrap();
        assert_eq!(rows,vec![2,3]);
        // A required operand against a nullable subquery column compares by value.
        let rows:Vec<i32>=User::objects.filter(in_subquery(User::email.column(),|_:Parent<User>|Subquery::new(User::objects).filter(User::nickname.is_null()).select(User::email))).order_by(User::id.asc()).select(User::id).using(&db).scalars().await.unwrap();
        assert_eq!(rows,vec![1,3]);

        // Predicates nest, and the innermost one reads the root and its parent.
        let nested_exists=exists(|u:Parent<User>|Subquery::new(Post::objects)
            .filter(Post::author_id.column().eq_outer(u.col(User::id.column())))
            .filter(exists(|p:Parent<Post>|Subquery::new(User::objects)
                .filter(User::id.column().eq_outer(p.col(Post::author_id.column())))
                .filter(User::email.column().eq_outer(u.col(User::email.column()))))));
        let rows:Vec<i32>=User::objects.filter(nested_exists).order_by(User::id.asc()).select(User::id).using(&db).scalars().await.unwrap();
        assert_eq!(rows,vec![1,2]);
        let inside_scalar=scalar(|u:Parent<User>|Subquery::new(Post::objects)
            .filter(exists(|p:Parent<Post>|Subquery::new(User::objects).filter(User::id.column().eq_outer(p.col(Post::author_id.column()))).filter(User::id.column().eq_outer(u.col(User::id.column())))))
            .order_by(Post::id.asc()).limit_one().select(Post::title));
        let rows:Vec<Option<String>>=User::objects.order_by(User::id.asc()).select(inside_scalar).using(&db).scalars().await.unwrap();
        assert_eq!(rows,vec![s("alpha"),s("alpha"),None]);

        // A scalar subquery stays nullable under a required outer terminal; only an absent outer row is an error.
        let v:Option<String>=User::objects.filter(User::id.eq(3)).require().select(best_title.clone()).using(&db).scalar().await.unwrap();
        assert_eq!(v,None);
        let absent=User::objects.filter(User::id.eq(99)).require().select(best_title).using(&db).scalar().await;
        assert!(absent.is_err());

        // An exists or in predicate with a stray outer source fails at erasure and blocks the statement.
        let mut stray_pred=None;
        let _=exists(|u:Parent<User>|{
            let p=Post::author_id.column().eq_outer(u.col(User::id.column()));
            stray_pred=Some(p.clone());
            Subquery::new(Post::objects).filter(p)
        });
        let bad=exists(|_:Parent<User>|Subquery::new(Post::objects).filter(stray_pred.take().unwrap()));
        let q=User::objects.filter(bad);
        let err=q.clone().select(User::id).using(&db).all().await;
        assert!(matches!(&err,Err(Error::Compatibility(m)) if m.contains("unresolved outer source")),"{err:?}");
        assert!(matches!(q.using(&db).statement(),Err(Error::Compatibility(_))));
        db.close().await;
    }};
}
#[tokio::test]
async fn sqlite_subqueries() {
    let path = std::env::temp_dir().join(format!("monorm-rust-subqueries-{}.db", std::process::id()));
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
