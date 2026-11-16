//! `GET /export/{name}`: the current list (search, filters, sort and the `queryset` filter) as a CSV stream.
//! The first batch is read before the answer starts, so a bad request is a plain error. Each later batch is
//! a keyset batch of `EXPORT_BATCH` rows in its own short transaction, so no connection waits for a slow client.
//! The request timeout bounds the answer head only; the export has its own idle timeout (`Site::export_idle_timeout`).

use std::io;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, Uri};
use axum::response::Response;
use axum::Extension;
use http_body_util::channel::{Channel, Sender};
use serde_json::json;

use crate::audit::{self, Entry};
use crate::csv;
use crate::list::{self, ListError, ListRequest, Page, Position};
use crate::server::{authorize, begin_scope, database_error, error_response, fill_batch, filters_of, found_keys, open_resource_with, query, ListQuery, Shared, State_, User};
use crate::site::{Resource, EXPORT_BATCH};
use crate::write::user_key_text;

/// How many CSV chunks wait for the client while the next batch is read.
const WAITING_CHUNKS: usize = 2;

pub(crate) async fn export(State(s): Shared, user: Option<Extension<User>>, Path(name): Path<String>, uri: Uri, q: Result<Query<ListQuery>, QueryRejection>) -> Response {
    let (user, scope, grants) = match authorize(&s, user).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, resource, reach, (user, _)) = match open_resource_with(&s, user, scope, grants, &name, &uri).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let (scope, q) = match query(scope, q).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let (filters, named) = match filters_of(&uri) {
        Ok(f) => f,
        Err(e) => {
            scope.rollback().await;
            return error_response(e);
        }
    };
    // The cursor, page and limit of the query string do not apply: the export starts at the first row and takes batches of its own size.
    let mut request = ListRequest { limit: Some(EXPORT_BATCH), sort: q.sort, q: q.q, filters, named, scope: reach.rows, export: true, ..ListRequest::new(Position::First) };
    let first = async {
        list::check_request(&s.site, resource, &request)?;
        request.found = found_keys(&s, resource, &scope, request.q.as_deref()).await?;
        let mut page = list::list(&s.site, resource, scope.executor(), s.target, &request).await?;
        fill_batch(&s, resource, &scope, &mut page).await?;
        Ok::<_, ListError>(page)
    }
    .await;
    let page = match first {
        Ok(page) => page,
        Err(e) => {
            scope.rollback().await;
            return error_response(e);
        }
    };
    // The audit row commits before the first byte leaves: a client that stops at 99% still leaves its row. The row count is not known yet.
    let entry = Entry { user_key: &user_key_text(&user), action: "export", resource: &resource.name, record_key: "", record_label: "", changes: Some(json!({"query": uri.query().unwrap_or_default()})) };
    if let Err(m) = audit::record(&s.site, scope.executor(), entry).await {
        scope.rollback().await;
        return database_error(&m);
    }
    if let Err(e) = scope.commit().await {
        return database_error(&e.message);
    }
    let (tx, body) = Channel::<Bytes, io::Error>::new(WAITING_CHUNKS);
    let first_chunk = format!("{}{}", csv::header(resource), csv::rows(resource, &page.rows));
    let file: String = resource.name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).collect();
    tokio::spawn(stream(s.clone(), resource.name.clone(), request, page, first_chunk, tx));
    let mut response = Response::new(Body::new(body));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert("content-type", "text/csv; charset=utf-8".parse().expect("static header"));
    if let Ok(v) = format!("attachment; filename=\"{file}.csv\"").parse() {
        headers.insert("content-disposition", v);
    }
    response
}

/// Sends the chunks, a batch after a batch. An error or an idle timeout aborts the body, so a cut file never looks complete.
async fn stream(s: Arc<State_>, name: String, mut request: ListRequest, mut page: Page, mut chunk: String, mut tx: Sender<Bytes, io::Error>) {
    let site = s.site.clone();
    let Some(resource) = site.resource(&name) else { return };
    loop {
        if !chunk.is_empty() {
            let data = Bytes::from(std::mem::take(&mut chunk));
            let sent = match site.export_idle_timeout {
                Some(idle) => tokio::time::timeout(idle, tx.send_data(data)).await.map_err(|_| idle),
                None => Ok(tx.send_data(data).await),
            };
            match sent {
                Ok(Ok(())) => {}
                // The client is gone: nobody reads an error.
                Ok(Err(_)) => return,
                Err(idle) => return tx.abort(io::Error::new(io::ErrorKind::TimedOut, format!("the client took no data for {} ms", idle.as_millis()))),
            }
        }
        let Some(cursor) = page.next.take() else { return };
        request.position = Position::After(cursor);
        page = match batch(&s, resource, &request).await {
            Ok(p) => p,
            Err(e) => return tx.abort(io::Error::other(describe(e))),
        };
        chunk = csv::rows(resource, &page.rows);
    }
}

/// One batch in its own transaction.
async fn batch(s: &State_, resource: &Resource, request: &ListRequest) -> Result<Page, ListError> {
    let scope = begin_scope(s).await.map_err(ListError::Engine)?;
    let out = async {
        let mut page = list::list(&s.site, resource, scope.executor(), s.target, request).await?;
        fill_batch(s, resource, &scope, &mut page).await?;
        Ok::<_, ListError>(page)
    }
    .await;
    match out {
        Ok(page) => {
            scope.commit().await.map_err(|e| ListError::Engine(e.message))?;
            Ok(page)
        }
        Err(e) => {
            scope.rollback().await;
            Err(e)
        }
    }
}

fn describe(e: ListError) -> String {
    match e {
        ListError::Bad(m) | ListError::Invalid(m) | ListError::Engine(m) | ListError::Timeout(m) => m,
    }
}
