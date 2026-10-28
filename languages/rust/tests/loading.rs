#![allow(dead_code)]
mod sqlite {
    include!(concat!(env!("OUT_DIR"), "/models.rs"));
}
mod postgres {
    include!(concat!(env!("OUT_DIR"), "/postgres.rs"));
}
use monorm_client::{BatchLoad, Error, Instance, Loaded, Unloaded};
const FIXTURE:&str="DROP TABLE IF EXISTS rust_load_books; DROP TABLE IF EXISTS rust_load_authors; CREATE TABLE rust_load_authors(id INTEGER PRIMARY KEY,name TEXT NOT NULL,bio TEXT NOT NULL,note TEXT,visible BOOLEAN NOT NULL); CREATE TABLE rust_load_books(id INTEGER PRIMARY KEY,author_id INTEGER NOT NULL,editor_id INTEGER,title TEXT NOT NULL); INSERT INTO rust_load_authors VALUES(1,'ann','large',NULL,TRUE),(2,'hidden','secret','x',FALSE),(3,'empty','none','y',TRUE); INSERT INTO rust_load_books VALUES(1,1,NULL,'one'),(2,1,2,'two'),(3,2,1,'hidden-author');DROP TABLE IF EXISTS rust_load_ordered_authors;CREATE TABLE rust_load_ordered_authors(id INTEGER PRIMARY KEY,name TEXT NOT NULL,visible BOOLEAN NOT NULL);INSERT INTO rust_load_ordered_authors VALUES(1,'ann',TRUE),(2,'hidden',FALSE),(3,'empty',TRUE);";

macro_rules! exercise {($module:ident,$url:expr)=>{{
    use $module::{Database, PolicyAuthor as Author, PolicyBook as Book};
    let fixture = monorm_engine::db::connect($url, 1).await.unwrap();
    fixture.batch(FIXTURE.into()).await.unwrap();
    fixture.close().await;
    let db = Database::connect($url, 1).await.unwrap();
    assert_eq!(
        $module::OrderedAuthor::objects
            .require()
            .using(&db)
            .first()
            .await
            .unwrap()
            .name,
        "empty"
    );
    assert_eq!(
        $module::OrderedAuthor::objects
            .require()
            .using(&db)
            .last()
            .await
            .unwrap()
            .name,
        "ann"
    );
    assert_eq!(
        $module::OrderedAuthor::objects
            .without_defaults()
            .require()
            .using(&db)
            .first()
            .await
            .unwrap()
            .name,
        "ann"
    );
    assert_eq!(
        $module::OrderedAuthor::objects
            .all_scalars()
            .require()
            .using(&db)
            .first()
            .await
            .unwrap()
            .name,
        "empty"
    );
    // Query extensions must work directly at the generated root in either order.
    trait AuthorQueries: Sized {
        fn named_ann(self) -> Self;
    }
    impl<M, R, L, D> AuthorQueries for monorm_client::Query<M, R, L, D>
    where M: monorm_client::Model<Source = $module::PolicyAuthorSource> {
        fn named_ann(self) -> Self {
            self.filter(Author::name.eq("ann".to_owned()))
        }
    }
    let extended = Author::objects.named_ann()
        .load((Author::bio, Author::books.load(Book::title)))
        .require().using(&db).first().await.unwrap();
    let _: &Unloaded = &extended.name;
    assert_eq!(extended.bio, "large");
    assert_eq!(extended.books.cached()[0].title, "one");
    let extended_after = Author::objects
        .load((Author::bio, Author::books.load(Book::title)))
        .named_ann().require().using(&db).first().await.unwrap();
    assert_eq!(extended.public_json().unwrap(), extended_after.public_json().unwrap());
    let normal = Author::objects.require().using(&db).first().await.unwrap();
    assert_eq!(
        monorm_client::serde_json::to_value(&normal).unwrap(),
        monorm_client::serde_json::json!({"name":"ann","note":null})
    );
    let _: &Unloaded = &normal.id;
    let _: &Unloaded = &normal.bio;
    assert_eq!(Author::objects.using(&db).count().await.unwrap(), 2);
    assert_eq!(
        Author::objects
            .without_defaults()
            .using(&db)
            .count()
            .await
            .unwrap(),
        3
    );
    let all = Author::objects
        .without_defaults()
        .filter(Author::ID.eq(2))
        .require()
        .using(&db)
        .first()
        .await
        .unwrap();
    assert_eq!(all.bio, "secret");
    let scalars = Author::objects
        .load(Author::name)
        .load(Author::bio)
        .require()
        .using(&db)
        .first()
        .await
        .unwrap();
    let _: &Unloaded = &scalars.note;
    assert_eq!(scalars.bio, "large");
    let full = Author::objects
        .load(Author::name)
        .all_scalars()
        .require()
        .using(&db)
        .first()
        .await
        .unwrap();
    assert_eq!(full.visible, true);
    assert_eq!(full.note, None);
    let bypass = Author::objects
        .load(Author::name)
        .filter(Author::ID.eq(2))
        .order_by(Author::ID.desc())
        .without_defaults()
        .require()
        .using(&db)
        .first()
        .await
        .unwrap();
    let _: &Unloaded = &bypass.bio;
    assert_eq!(bypass.name, "hidden");
    let root_scalar_after_relation = Author::objects
        .load(Author::books.load(Book::title))
        .load(Author::bio)
        .require()
        .using(&db)
        .get(Author::ID_KEY, 1)
        .await
        .unwrap();
    let _: &Unloaded = &root_scalar_after_relation.name;
    let _: &Unloaded = &root_scalar_after_relation.note;
    assert_eq!(root_scalar_after_relation.bio, "large");
    let bypass_bare=Book::objects.without_defaults().load(Book::author).require().using(&db).get(Book::ID_KEY,3).await.unwrap();
    assert_eq!(bypass_bare.author.cached().unwrap().bio,"secret");assert!(!bypass_bare.author.cached().unwrap().visible);
    let bypass_after=Book::objects.load(Book::author).without_defaults().require().using(&db).get(Book::ID_KEY,3).await.unwrap();
    assert_eq!(bypass_after.author.cached().unwrap().bio,"secret");
    let bypass_selected=Book::objects.load(Book::author.load(Author::name)).without_defaults().require().using(&db).get(Book::ID_KEY,3).await.unwrap();
    let _: &Unloaded=&bypass_selected.author.cached().unwrap().bio;assert_eq!(bypass_selected.author.cached().unwrap().name,"hidden");
    let bypass_nested=Author::objects.without_defaults().load(Author::books).require().using(&db).get(Author::ID_KEY,2).await.unwrap();
    assert_eq!(bypass_nested.books.cached()[0].author_id,2);
    let _: &monorm_client::RelatedSet<Author>=&bypass_nested.books.cached()[0].author;
    let books = Book::objects
        .order_by(Book::ID.asc())
        .using(&db)
        .await
        .unwrap();
    assert_eq!(books[0].author.cached().unwrap().name, "ann");
    assert!(books[2].author.cached().is_none());
    let _: &Unloaded = &books[0].author_id;
    let plain = Book::objects
        .without_related()
        .require()
        .using(&db)
        .first()
        .await
        .unwrap();
    assert_eq!(
        plain
            .author
            .query()
            .using(&db)
            .first()
            .await
            .unwrap()
            .unwrap()
            .name,
        "ann"
    );
    let explicit = Book::objects
        .load(Book::author.load(Author::bio))
        .without_related()
        .require()
        .using(&db)
        .first()
        .await
        .unwrap();
    assert_eq!(explicit.author.cached().unwrap().bio, "large");
    let _: &Unloaded = &explicit.author.cached().unwrap().name;
    let editors = Book::objects
        .load(Book::editor)
        .order_by(Book::ID.asc())
        .using(&db)
        .await
        .unwrap();
    assert!(editors[0].editor.cached().is_none());
    assert!(editors[1].editor.cached().is_none());
    assert_eq!(editors[2].editor.cached().unwrap().name, "ann");

    // Finite requests on a cyclic schema and additive nested target guarantees.
    let cyclic = Author::objects
        .load(Author::books.load((Book::title, Book::author.load(Author::name))))
        .require()
        .using(&db)
        .get(Author::ID_KEY, 1)
        .await
        .unwrap();
    assert_eq!(
        cyclic.books.cached()[0].author.cached().unwrap().name,
        "ann"
    );
    let added = cyclic
        .load(Author::books.load(Book::id), &db)
        .await
        .unwrap();
    let _: &String = &added.books.cached()[0].title;
    let _: i32 = added.books.cached()[0].id;
    assert_eq!(added.books.cached()[0].author.cached().unwrap().name, "ann");
    let selected = Author::objects
        .load(Author::books.load(Book::title))
        .load(Author::books.load(Book::id))
        .require()
        .using(&db)
        .get(Author::ID_KEY, 1)
        .await
        .unwrap();
    let _: &String = &selected.books.cached()[0].title;
    let _: i32 = selected.books.cached()[0].id;
    // A bare target query must initialize its normal selection and default caches.
    let bare_prefetch = Author::objects
        .load(Author::books.as_prefetch(Book::objects))
        .require().using(&db).get(Author::ID_KEY, 1).await.unwrap();
    assert_eq!(bare_prefetch.books.cached()[0].title, "one");
    assert_eq!(bare_prefetch.books.cached()[0].author.cached().unwrap().name, "ann");
    let _: &Unloaded = &bare_prefetch.books.cached()[0].author_id;
    let limited = Author::objects
        .load(
            Author::books.as_prefetch(
                Book::objects
                    .query()
                    .load(Book::title)
                    .filter(Book::TITLE.ne("hidden-author".into()))
                    .order_by(Book::ID.desc())
                    .limit(1),
            ),
        )
        .order_by(Author::ID.asc())
        .using(&db)
        .await
        .unwrap();
    assert_eq!(limited[0].books.cached().len(), 1);
    assert_eq!(limited[0].books.cached()[0].title, "two");
    assert!(limited[1].books.cached().is_empty());
    let batch = Author::objects
        .order_by(Author::ID.asc())
        .using(&db)
        .await
        .unwrap()
        .prefetch(
            Author::books.as_prefetch(
                Book::objects
                    .load(Book::title)
                    .order_by(Book::ID.desc())
                    .limit(1),
            ),
            &db,
        )
        .await
        .unwrap();
    assert_eq!(batch[0].books.cached()[0].title, "two");
    assert!(batch[1].books.cached().is_empty());

    let reload_source = Author::objects
        .load(Author::books.load(Book::title))
        .require()
        .using(&db)
        .get(Author::ID_KEY, 1)
        .await
        .unwrap();
    let reload_union = reload_source
        .reload(Author::books.load(Book::id), &db)
        .await
        .unwrap();
    let _: &String = &reload_union.books.cached()[0].title;
    assert_eq!(reload_union.books.cached()[0].id, 1);
    let left = Author::objects
        .load(
            Author::books.as_prefetch(
                Book::objects
                    .load(Book::title)
                    .filter(Book::title.eq("one".into())),
            ),
        )
        .filter(Author::id.eq(1));
    let right = Author::objects
        .load(
            Author::books.as_prefetch(
                Book::objects
                    .load(Book::title)
                    .filter(Book::title.eq("two".into())),
            ),
        )
        .filter(Author::id.eq(3));
    let union = left
        .or(right)
        .unwrap()
        .order_by(Author::id.asc())
        .using(&db)
        .await
        .unwrap();
    assert_eq!(union.len(), 2);
    assert_eq!(union[0].books.cached()[0].title, "one");
    assert!(union[1].books.cached().is_empty());
    let union = Author::objects
        .load(Author::books.load(Book::title))
        .or(Author::objects.load(Author::books.load(Book::title)))
        .unwrap()
        .using(&db)
        .await
        .unwrap();
    assert_eq!(union.len(), 2);
    // Loaded NULL and caches survive additive loads, including external changes.
    let old_editor = Book::objects
        .without_related()
        .require()
        .using(&db)
        .get(Book::ID_KEY, 2)
        .await
        .unwrap();
    let old = Author::objects
        .load((Author::note, Author::books.load(Book::title)))
        .require()
        .using(&db)
        .get(Author::ID_KEY, 1)
        .await
        .unwrap();
    let fixture = monorm_engine::db::connect($url, 1).await.unwrap();
    fixture.batch("UPDATE rust_load_authors SET note='new' WHERE id=1; UPDATE rust_load_books SET editor_id=1 WHERE id=2; INSERT INTO rust_load_books VALUES(4,1,NULL,'new-book');".into()).await.unwrap();
    fixture.close().await;
    let loaded_editor = old_editor.load(Book::editor_id, &db).await.unwrap();
    assert_eq!(loaded_editor.editor_id, Some(1));
    assert_eq!(
        loaded_editor
            .editor
            .query()
            .require()
            .using(&db)
            .first()
            .await
            .unwrap()
            .name,
        "ann"
    );
    let kept = old.load(Author::bio, &db).await.unwrap();
    assert_eq!(kept.note, None);
    assert_eq!(kept.books.cached().len(), 2);
    let reloaded = kept.reload(Author::note, &db).await.unwrap();
    assert_eq!(reloaded.note.as_deref(), Some("new"));
    assert_eq!(reloaded.books.cached().len(), 2);
    let refreshed = reloaded
        .reload(Author::books.load(Book::title), &db)
        .await
        .unwrap();
    assert_eq!(refreshed.books.cached().len(), 3);

    let named=monorm_client::relation_views!(refreshed,&db;
        newest => Author::books.as_prefetch(Book::objects.load(Book::title).order_by(Book::ID.desc()).limit(1)),
        author_view => Author::books.as_prefetch(Book::objects.load(Book::author))
    ).await.unwrap();
    assert_eq!(named.newest.cached().len(), 1);
    assert_eq!(named.newest.cached()[0].title, "new-book");
    assert_eq!(named.row.books.cached().len(), 3);
    let singles=monorm_client::relation_views!(Author::objects.require().using(&db).get(Author::ID_KEY,1).await.unwrap(),&db;
        first_book => Author::books.label("first_book").as_prefetch(Book::objects.load(Book::title).order_by(Book::ID.desc())).one()
    ).await.unwrap();
    assert_eq!(singles.first_book.cached().unwrap().title, "new-book");
    let single_author=monorm_client::relation_views!(Book::objects.require().using(&db).first().await.unwrap(),&db; person => Book::author).await.unwrap();
    assert_eq!(single_author.person.cached().unwrap().name, "ann");
    let collision = Author::objects
        .load(Author::books.label("name"))
        .using(&db)
        .all()
        .await;
    assert!(matches!(collision, Err(Error::Compatibility(_))));
    let collision = Author::objects
        .load((Author::books.label("view"), Author::books.label("view")))
        .using(&db)
        .all()
        .await;
    assert!(matches!(collision, Err(Error::Compatibility(_))));

    // The same associated-type consumer accepts partial, full, and checked rows.
    fn names<S: $module::PolicyAuthorShape<Name = String>>(row: &S) -> &str {
        row.name()
    }
    assert_eq!(names(&normal), "ann");
    assert_eq!(names(&all), "hidden");
    type NameOnly = Author<Unloaded, Loaded, Unloaded, Unloaded, Unloaded>;
    let runtime = Author::objects
        .runtime(&["name"])
        .unwrap()
        .require()
        .using(&db)
        .get(Author::ID_KEY, 1)
        .await
        .unwrap();
    let checked: NameOnly = runtime.try_into_static().unwrap();
    assert_eq!(names(&checked), "ann");
    let runtime = Author::objects
        .runtime(&["note"])
        .unwrap()
        .require()
        .using(&db)
        .get(Author::ID_KEY, 1)
        .await
        .unwrap();
    assert!(runtime.try_into_static::<NameOnly>().is_err());
    let runtime = Book::objects
        .runtime(&["title", "editor.name"])
        .unwrap()
        .require()
        .using(&db)
        .get(Book::ID_KEY, 1)
        .await
        .unwrap();
    assert!(runtime.related::<Author>("editor").unwrap().is_empty());
    assert!(!runtime.is_loaded("author_id"));
    let runtime = Author::objects
        .runtime(&["name"])
        .unwrap()
        .filter_runtime("name", "ann".to_owned())
        .unwrap()
        .order_runtime("id", true)
        .unwrap()
        .require()
        .using(&db)
        .first()
        .await
        .unwrap();
    assert_eq!(runtime.get::<String>("name").unwrap(), "ann");
    assert!(Author::objects.runtime(&["typo"]).is_err());
    assert!(Author::objects
        .runtime(&["name"])
        .unwrap()
        .order_runtime("books.title", false)
        .is_err());
    assert!(!format!("{checked:?}").contains("bio"));
    // No row survives a missing-parent transition, but its input is recoverable.
    let doomed = Author::objects
        .require()
        .using(&db)
        .get(Author::ID_KEY, 3)
        .await
        .unwrap();
    let fixture = monorm_engine::db::connect($url, 1).await.unwrap();
    fixture
        .batch("DELETE FROM rust_load_authors WHERE id=3".into())
        .await
        .unwrap();
    fixture.close().await;
    let doomed = doomed.load(Author::name, &db).await.unwrap();
    assert_eq!(doomed.name, "empty");
    let failed = doomed.load(Author::bio, &db).await.unwrap_err();
    assert!(matches!(failed.error, Error::NotFound));
    assert_eq!(failed.original.name, "empty");
    // Default execution uses the accepted associated-root syntax.
    db.set_default();
    let root = Book::objects
        .load((Book::title, Book::author))
        .require()
        .first()
        .await
        .unwrap();
    assert_eq!(root.title, "one");
    Database::clear_default();
    db.close().await;
}}}

#[tokio::test]
async fn sqlite_selection_loading_runtime() {
    let path = std::env::temp_dir().join(format!("monorm-rust-loading-{}.db", std::process::id()));
    let url = format!("sqlite://{}", path.display());
    exercise!(sqlite, &url);
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn postgres_selection_loading_runtime() {
    let url = std::env::var("ORM_RUST_TEST_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost/orm_test".into());
    exercise!(postgres, &url);
}
