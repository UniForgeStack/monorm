#![allow(dead_code)]
mod sqlite {
    include!(concat!(env!("OUT_DIR"), "/models.rs"));
}
mod postgres {
    include!(concat!(env!("OUT_DIR"), "/postgres.rs"));
}
use orm_client::{BatchLoad, Instance};
macro_rules! exercise { ($module:ident,$url:expr) => {{
    use $module::{CompositeOwner as Owner, CompositeChild as Child, Database};
    let driver=orm_engine::db::connect($url,1).await.unwrap();
    driver.batch("DROP TABLE IF EXISTS rust_load_composite_children; DROP TABLE IF EXISTS rust_load_composite_owners; CREATE TABLE rust_load_composite_owners(tenant INTEGER,id INTEGER,name TEXT,PRIMARY KEY(tenant,id)); CREATE TABLE rust_load_composite_children(id INTEGER PRIMARY KEY,owner_tenant INTEGER,owner_id INTEGER,title TEXT); INSERT INTO rust_load_composite_owners VALUES(1,7,'one'),(2,7,'two'); INSERT INTO rust_load_composite_children VALUES(1,1,7,'a'),(2,2,7,'b'),(3,NULL,7,'none');".into()).await.unwrap();
    driver.close().await;
    let db=Database::connect($url,1).await.unwrap();
    let owners=Owner::objects.load(Owner::name).order_by(Owner::tenant.asc()).using(&db).all().await.unwrap();
    assert!(owners[0]!=owners[1]);
    assert_eq!(owners[0].children.query().using(&db).count().await.unwrap(),1);
    let owners=owners.prefetch(Owner::children.load(Child::title),&db).await.unwrap();
    assert_eq!(owners[0].children.cached()[0].title,"a");
    assert_eq!(owners[1].children.cached()[0].title,"b");
    let row=Owner::objects.load(Owner::name).require().using(&db).get(Owner::TENANT_ID_KEY,(2,7)).await.unwrap();
    let row=row.load(Owner::children.load(Child::title),&db).await.unwrap();
    assert_eq!(row.children.cached()[0].title,"b");
    let row=row.reload(Owner::name,&db).await.unwrap();
    assert_eq!(row.name,"two");
    let children=Child::objects.load((Child::title,Child::owner.load(Owner::name))).order_by(Child::id.asc()).using(&db).all().await.unwrap();
    assert_eq!(children[0].owner.cached().unwrap().name,"one");
    assert_eq!(children[1].owner.cached().unwrap().name,"two");
    assert!(children[2].owner.cached().is_none());
    assert_eq!(children[2].owner.query().using(&db).count().await.unwrap(),0);
    let runtime=Owner::objects.runtime(&["name","children.title"]).unwrap().require().using(&db).get(Owner::TENANT_ID_KEY,(2,7)).await.unwrap();
    assert_eq!(runtime.get::<String>("name").unwrap(),"two");
    assert_eq!(runtime.related::<Child>("children").unwrap()[0].get::<String>("title").unwrap(),"b");
    assert_eq!(Owner::objects.using(&db).first().await.unwrap().unwrap().tenant,1);
    assert_eq!(Owner::objects.using(&db).last().await.unwrap().unwrap().tenant,2);
    db.close().await;
}}; }
#[tokio::test]
async fn sqlite_composite_loading() {
    let path =
        std::env::temp_dir().join(format!("orm-composite-loading-{}.db", std::process::id()));
    let url = format!("sqlite://{}", path.display());
    exercise!(sqlite, &url);
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn postgres_composite_loading() {
    let url = std::env::var("ORM_RUST_TEST_POSTGRES_URL").unwrap();
    exercise!(postgres, &url);
}
