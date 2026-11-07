//! One transaction for one request, rolled back when the request future is dropped.

use std::sync::Arc;

use monorm_engine::db::{self, DbResult, Executor, Transaction};

/// The transaction of a request. A request that ends without `commit` or `rollback` (the
/// timeout dropped its future) rolls back explicitly: a host can still hold the
/// transaction object, so dropping the last reference is not enough, and on SQLite (one
/// connection) the next request would wait for the connection forever.
pub struct Scoped {
    tx: Arc<dyn Transaction>,
    done: bool,
}

impl Scoped {
    pub async fn begin(conn: &dyn Executor) -> DbResult<Scoped> {
        Ok(Scoped { tx: db::guard(conn.begin().await?), done: false })
    }

    pub fn executor(&self) -> &dyn Executor {
        self.tx.as_ref()
    }

    pub fn transaction(&self) -> Arc<dyn Transaction> {
        self.tx.clone()
    }

    pub async fn commit(mut self) -> DbResult<()> {
        let out = self.tx.commit().await;
        self.done = true;
        out
    }

    pub async fn rollback(mut self) {
        let _ = self.tx.rollback().await;
        self.done = true;
    }
}

impl Drop for Scoped {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // The rollback runs on the runtime that serves the request; with no runtime the
        // transaction drops with its connection.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let tx = self.tx.clone();
            runtime.spawn(async move {
                let _ = tx.rollback().await;
            });
        }
    }
}
