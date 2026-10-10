#[allow(dead_code)]
mod models {
    include!(concat!(env!("OUT_DIR"), "/models.rs"));
}
use models::{Database, User};
use orm_client::{Loaded, Unloaded};
#[tokio::main]
async fn main() -> orm_client::Result<()> {
    let url = std::env::var("ORM_DATABASE_URL").unwrap_or_else(|_| "sqlite://:memory:".into());
    let db = Database::connect(&url, 1).await?;
    let rows = User::objects()
        .filter(User::EMAIL.eq("reader@example.com".into()))
        .using(&db)
        .await?;
    for row in rows {
        println!("{} {:?}", row.email, row.nickname);
    }
    let partial = User::<Unloaded, Loaded, Unloaded, Unloaded>::query()
        .using(&db)
        .first()
        .await?;
    if let Some(row) = partial {
        let _marker = &row.id;
        println!("{}", row.email);
    }
    db.close().await;
    Ok(())
}
