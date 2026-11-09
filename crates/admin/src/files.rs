//! File fields: upload and download.
//!
//! The bytes live in the app's file storage, which only the host knows.
//! A file field is a nullable `Json` field that holds the storage reference.
//! The core hands the bytes to the host functions `file.store` and `file.fetch` through a temporary file and never shows a storage URL.

use std::path::PathBuf;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use http_body_util::channel::Channel;
use serde_json::{json, Map, Value};

use crate::bridge::CallContext;
use crate::list;
use crate::list::Record;
use crate::permissions::Op;
use crate::server::{authorize, deny, reach, reject, segment, Reach, Shared, State_, User};
use crate::site::Resource;
use crate::write::{convert, decode_key, finish, list_failure, meta_of, open_head, run_update, server_error, write_audit, writable, Call, Meta, Problems};

/// The largest request body of an upload, in bytes. The core buffers the body, so the cap stays small.
pub const MAX_UPLOAD: usize = 25 * 1024 * 1024;

/// A temporary file that the host reads or writes. The file goes away when the guard drops.
struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("monorm-admin-{}", uuid::Uuid::new_v4().simple())))
    }

    async fn write(&self, data: Vec<u8>) -> Result<(), String> {
        let path = self.0.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            options.open(&path)?.write_all(&data)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
    }

    /// The file as a streamed body: 64 KiB chunks, so a large file never sits in memory whole. The guard lives until the last chunk.
    async fn stream(self) -> Result<Body, String> {
        use std::io::Read;
        let path = self.0.clone();
        let mut file = tokio::task::spawn_blocking(move || std::fs::File::open(path)).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let (mut tx, body) = Channel::<Bytes, std::io::Error>::new(2);
        tokio::spawn(async move {
            let _guard = self;
            loop {
                let step = tokio::task::spawn_blocking(move || {
                    let mut chunk = vec![0u8; 64 * 1024];
                    file.read(&mut chunk).map(|n| {
                        chunk.truncate(n);
                        (file, chunk)
                    })
                })
                .await;
                match step {
                    Ok(Ok((f, chunk))) if chunk.is_empty() => break drop(f),
                    Ok(Ok((f, chunk))) => {
                        file = f;
                        if tx.send_data(Bytes::from(chunk)).await.is_err() {
                            break;
                        }
                    }
                    Ok(Err(e)) => break tx.abort(e),
                    Err(e) => break tx.abort(std::io::Error::other(e.to_string())),
                }
            }
        });
        Ok(Body::new(body))
    }

    fn text(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The file part of a multipart body.
#[derive(Debug, PartialEq)]
struct Part {
    filename: String,
    content_type: String,
    data: Vec<u8>,
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || from > haystack.len() - needle.len() {
        return None;
    }
    haystack[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// The `boundary` of a `multipart/form-data` content type.
fn boundary_of(content_type: &str) -> Option<String> {
    let mut parts = content_type.split(';');
    if !parts.next()?.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    parts.find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("boundary").then(|| v.trim().trim_matches('"').to_owned())
    })
    .filter(|b| !b.is_empty() && b.len() <= 200)
}

/// The value of `key` in a header such as `form-data; name="file"; filename="a.txt"`.
fn param(header: &str, key: &str) -> Option<String> {
    let mut out = None;
    let mut chars = header.char_indices().peekable();
    let mut start = 0;
    let mut quoted = false;
    let mut pieces: Vec<&str> = Vec::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' if quoted => {
                chars.next();
            }
            '"' => quoted = !quoted,
            ';' if !quoted => {
                pieces.push(&header[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    pieces.push(&header[start..]);
    for piece in pieces {
        let Some((k, v)) = piece.split_once('=') else { continue };
        if k.trim().eq_ignore_ascii_case(key) {
            let v = v.trim();
            out = Some(match v.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
                Some(inner) => inner.replace("\\\"", "\"").replace("\\\\", "\\"),
                None => v.to_owned(),
            });
        }
    }
    out
}

/// The name that a client sent as a plain file name: no directory, no control character, at most 255 characters.
fn clean_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let name: String = base.chars().filter(|c| !c.is_control()).take(255).collect();
    let name = name.trim().to_owned();
    if name.is_empty() || name == "." || name == ".." {
        "file".to_owned()
    } else {
        name
    }
}

/// A content type that a header can carry; anything else is a byte stream.
fn clean_type(raw: &str) -> String {
    let t = raw.trim();
    if t.len() <= 255 && t.contains('/') && !t.contains(char::is_whitespace) && HeaderValue::from_str(t).is_ok() {
        t.to_owned()
    } else {
        "application/octet-stream".to_owned()
    }
}

/// The one part named `file` of a `multipart/form-data` body.
fn parse_multipart(body: &[u8], boundary: &str) -> Result<Part, String> {
    let delimiter = format!("--{boundary}").into_bytes();
    let next = [b"\r\n".as_slice(), &delimiter].concat();
    let mut at = find(body, &delimiter, 0).ok_or("the body is not multipart/form-data")?;
    loop {
        at += delimiter.len();
        if body[at..].starts_with(b"--") {
            return Err("the body needs one part named \"file\"".into());
        }
        if !body[at..].starts_with(b"\r\n") {
            return Err("the multipart body is not valid".into());
        }
        at += 2;
        let head_end = find(body, b"\r\n\r\n", at - 2).ok_or("the multipart body is not valid")?;
        let head = std::str::from_utf8(&body[at - 2..head_end + 2]).map_err(|_| "the part headers are not UTF-8")?;
        let start = head_end + 4;
        let end = find(body, &next, start).ok_or("the multipart body is not valid")?;
        let (mut disposition, mut kind) = (None, None);
        for line in head.split("\r\n").filter(|l| !l.is_empty()) {
            let Some((k, v)) = line.split_once(':') else { return Err("the multipart body is not valid".into()) };
            match k.trim().to_ascii_lowercase().as_str() {
                "content-disposition" => disposition = Some(v.trim().to_owned()),
                "content-type" => kind = Some(v.trim().to_owned()),
                _ => {}
            }
        }
        if let Some(d) = disposition {
            if param(&d, "name").as_deref() == Some("file") {
                let Some(filename) = param(&d, "filename") else { return Err("the part \"file\" needs a file name".into()) };
                return Ok(Part { filename: clean_name(&filename), content_type: clean_type(kind.as_deref().unwrap_or_default()), data: body[start..end].to_vec() });
            }
        }
        at = end + 2;
    }
}

/// The `Content-Disposition` of a download: `attachment`, with the name in plain ASCII and in UTF-8.
fn disposition(filename: &str) -> String {
    let plain: String = filename.chars().map(|c| if c.is_ascii_graphic() && !matches!(c, '"' | '\\' | '%') || c == ' ' { c } else { '_' }).collect();
    let mut encoded = String::new();
    for b in filename.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{plain}\"; filename*=UTF-8''{encoded}")
}

/// The file field `field` of the resource: its column index and name, or none when it is not a file field.
fn file_column<'a>(resource: &'a Resource, field: &str) -> Option<(usize, &'a str)> {
    if !resource.files.iter().any(|f| f == field) {
        return None;
    }
    resource.detail.iter().position(|d| d.field == field && writable(d)).map(|at| (at, resource.detail[at].label.as_str()))
}

fn no_bridge(meta: &Meta) -> Response {
    server_error(meta, "a file field needs the host bridge and a file storage")
}

fn host_failure(meta: &Meta, name: &str, e: crate::bridge::BridgeError) -> Response {
    server_error(meta, &format!("{name}: {}: {}", e.kind(), e.message()))
}

/// A checked upload target: the open request, the file column and the record.
struct Aim<'a> {
    c: Call<'a>,
    reach: Reach,
    at: usize,
    label: &'a str,
    key: Vec<Value>,
    raw: String,
    record: Record,
}

/// The permission to change the record, the file field (404, 422 for a read-only field), the key and the record (404, 403 by the `can` rule).
/// `lock` takes the row lock. The transaction stays open on `Ok`; every `Err` closed it.
async fn locate<'a>(s: &'a State_, user: Option<Extension<User>>, name: &str, field: &str, head: (Meta, Uri), lock: bool) -> Result<Aim<'a>, Response> {
    let c = open_head(s, user, name, Op::Update, head).await.map_err(|r| *r)?;
    let (site, resource) = (&s.site, c.resource);
    let Some((at, label)) = file_column(resource, field) else {
        c.scope.rollback().await;
        return Err(reject(StatusCode::NOT_FOUND, "NotFound", &format!("no file field {field:?} on {name:?}")));
    };
    if resource.form.readonly.iter().any(|l| l == label) {
        c.scope.rollback().await;
        let mut problems = Problems::default();
        problems.field(label, "This field is read-only.");
        return Err(problems.response());
    }
    let (raw, key) = match decode_key(resource, segment(&c.uri, 1)) {
        Ok(k) => k,
        Err(m) => {
            c.scope.rollback().await;
            return Err(reject(StatusCode::BAD_REQUEST, "BadRequest", &m));
        }
    };
    let (scope, reach) = reach(s, resource, &c.user, &c.grants, Vec::new(), c.scope).await.map_err(|r| *r)?;
    let c = Call { scope, ..c };
    let record = match list::read_record(site, resource, c.scope.executor(), s.target, &key, &reach.rows, lock).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            c.scope.rollback().await;
            return Err(reject(StatusCode::NOT_FOUND, "NotFound", "no such record"));
        }
        Err(e) => {
            let response = list_failure(&c.meta, e);
            c.scope.rollback().await;
            return Err(response);
        }
    };
    if record.flags.get("change") == Some(&Value::Bool(false)) {
        return Err(deny(site, c.scope, &c.user, &resource.name, json!({"reason": "can", "rule": "change"})).await);
    }
    Ok(Aim { c, reach, at, label, key, raw, record })
}

/// `POST /files/<resource>/<key>/<field>`: the multipart body replaces the file of the record. One audit row.
/// The checks run in a short transaction. The body read and `file.store` hold none: on SQLite one transaction holds the one connection of the app.
/// Then a second transaction locks the row, checks again, writes the reference and the audit row. A failure after `file.store` leaves the stored file unused.
pub(crate) async fn upload(State(s): Shared, user: Option<Extension<User>>, Path((name, _, field)): Path<(String, String, String)>, req: Request) -> Response {
    let (first, second) = (meta_of(&s, &req), meta_of(&s, &req));
    let boundary = req.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).and_then(boundary_of);
    let body = req.into_body();
    let Aim { c, at, label, .. } = match locate(&s, user.clone(), &name, &field, first, false).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    c.scope.rollback().await;
    let (site, resource, meta) = (&s.site, c.resource, &c.meta);
    let Some((bridge, auth)) = &s.bridge else { return no_bridge(meta) };
    let Some(boundary) = boundary else { return reject(StatusCode::BAD_REQUEST, "BadRequest", "the body is multipart/form-data with a boundary") };
    let Ok(bytes) = axum::body::to_bytes(body, MAX_UPLOAD).await else {
        return reject(StatusCode::PAYLOAD_TOO_LARGE, "PayloadTooLarge", &format!("the file is larger than {} MiB", MAX_UPLOAD / 1024 / 1024));
    };
    let part = match parse_multipart(&bytes, &boundary) {
        Ok(p) => p,
        Err(m) => return reject(StatusCode::BAD_REQUEST, "BadRequest", &m),
    };
    drop(bytes);
    let size = part.data.len();
    let temp = Temp::new();
    if let Err(m) = temp.write(part.data).await {
        return server_error(meta, &format!("the upload file: {m}"));
    }
    let payload = json!({"model": site.schema.model(resource.model).ir.name, "field": field, "path": temp.text(), "filename": part.filename, "content_type": part.content_type, "size": size, "max_size": MAX_UPLOAD});
    let ctx = CallContext { request_id: auth.security.call_id(), tenant: None, scope: json!({}), tx: None };
    let reference = match bridge.call("file.store", payload, ctx).await {
        Ok(v) if v.is_object() => v,
        Ok(_) => return server_error(meta, "file.store returns the reference of the file"),
        Err(e) => return host_failure(meta, "file.store", e),
    };
    drop(temp);
    let model = site.schema.model(resource.model);
    let bind = match model.field(&resource.detail[at].field).map_err(|e| e.to_string()).and_then(|f| convert(&site.schema, f, &reference)) {
        Ok(b) => b,
        Err(m) => return server_error(meta, &format!("file.store: {m}")),
    };
    let Aim { c, reach, key, raw, record, .. } = match locate(&s, user, &name, &field, second, true).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let changed = vec![(label.to_owned(), bind, reference.clone())];
    if let Err(r) = run_update(&s, &c, &reach, &key, &changed).await {
        c.scope.rollback().await;
        return r;
    }
    let mut diff = Map::new();
    diff.insert(label.to_owned(), json!({"from": record.cells[at], "to": reference}));
    if let Err(m) = write_audit(&s, &c, "update", &raw, &record.cells, Value::Object(diff)).await {
        let response = server_error(&c.meta, &m);
        c.scope.rollback().await;
        return response;
    }
    let response = Json(json!({"ok": true, "key": raw, "changed": [label], "value": reference})).into_response();
    finish(c, response).await
}

/// `GET /files/<resource>/<key>/<field>`: the stored file as an attachment. The browser never sees a storage URL.
pub(crate) async fn download(State(s): Shared, user: Option<Extension<User>>, Path((name, _, field)): Path<(String, String, String)>, req: Request) -> Response {
    let (meta, uri) = meta_of(&s, &req);
    drop(req);
    let (user, scope, grants) = match authorize(&s, user).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let Some(resource) = s.site.resource(&name) else {
        scope.rollback().await;
        return reject(StatusCode::NOT_FOUND, "NotFound", &format!("no resource {name:?}"));
    };
    if !resource.allow.contains(&Op::Read) {
        return deny(&s.site, scope, &user, &resource.name, json!({"reason": "allow"})).await;
    }
    if !s.site.allows(&grants, resource, Op::Read) {
        return deny(&s.site, scope, &user, &resource.name, json!({"needs": s.site.requires(resource, Op::Read)})).await;
    }
    let Some((at, _)) = file_column(resource, &field) else {
        scope.rollback().await;
        return reject(StatusCode::NOT_FOUND, "NotFound", &format!("no file field {field:?} on {name:?}"));
    };
    let key = match decode_key(resource, segment(&uri, 1)) {
        Ok((_, k)) => k,
        Err(m) => {
            scope.rollback().await;
            return reject(StatusCode::BAD_REQUEST, "BadRequest", &m);
        }
    };
    let (scope, reach) = match reach(&s, resource, &user, &grants, Vec::new(), scope).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let record = list::read_record(&s.site, resource, scope.executor(), s.target, &key, &reach.rows, false).await;
    scope.rollback().await;
    let record = match record {
        Ok(Some(r)) => r,
        Ok(None) => return reject(StatusCode::NOT_FOUND, "NotFound", "no such record"),
        Err(e) => return list_failure(&meta, e),
    };
    let reference = record.cells[at].clone();
    let Some(object) = reference.as_object() else { return reject(StatusCode::NOT_FOUND, "NotFound", "the record has no file") };
    let Some((bridge, auth)) = &s.bridge else { return no_bridge(&meta) };
    let filename = object.get("filename").and_then(Value::as_str).map(clean_name).unwrap_or_else(|| "file".to_owned());
    let content_type = object.get("content_type").and_then(Value::as_str).map(clean_type).unwrap_or_else(|| "application/octet-stream".to_owned());
    let temp = Temp::new();
    let payload = json!({"model": s.site.schema.model(resource.model).ir.name, "field": field, "reference": reference, "path": temp.text()});
    let ctx = CallContext { request_id: auth.security.call_id(), tenant: None, scope: json!({}), tx: None };
    if let Err(e) = bridge.call("file.fetch", payload, ctx).await {
        return host_failure(&meta, "file.fetch", e);
    }
    let body = match temp.stream().await {
        Ok(b) => b,
        Err(m) => return server_error(&meta, &format!("file.fetch left no file: {m}")),
    };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_str(&content_type).unwrap_or(HeaderValue::from_static("application/octet-stream")));
    headers.insert(CONTENT_DISPOSITION, HeaderValue::from_str(&disposition(&filename)).unwrap_or(HeaderValue::from_static("attachment")));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    type PartSpec<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

    fn body(parts: &[PartSpec]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, file, kind, data) in parts {
            out.extend_from_slice(b"--XyZ\r\n");
            out.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"").as_bytes());
            if let Some(f) = file {
                out.extend_from_slice(format!("; filename=\"{f}\"").as_bytes());
            }
            out.extend_from_slice(b"\r\n");
            if let Some(k) = kind {
                out.extend_from_slice(format!("Content-Type: {k}\r\n").as_bytes());
            }
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"--XyZ--\r\n");
        out
    }

    #[test]
    fn finds_the_file_part_after_other_parts() {
        let b = body(&[("note", None, None, b"hi"), ("file", Some("a b.txt"), Some("text/plain"), b"line1\r\n--Xy not a delimiter\nline2")]);
        let p = parse_multipart(&b, "XyZ").unwrap();
        assert_eq!(p.filename, "a b.txt");
        assert_eq!(p.content_type, "text/plain");
        assert_eq!(p.data, b"line1\r\n--Xy not a delimiter\nline2");
    }

    #[test]
    fn keeps_binary_bytes_and_empty_files() {
        let data: Vec<u8> = (0..=255).collect();
        let b = body(&[("file", Some("x.bin"), None, &data)]);
        let p = parse_multipart(&b, "XyZ").unwrap();
        assert_eq!(p.data, data);
        assert_eq!(p.content_type, "application/octet-stream");
        let b = body(&[("file", Some("e"), None, b"")]);
        assert_eq!(parse_multipart(&b, "XyZ").unwrap().data, b"");
    }

    #[test]
    fn refuses_a_body_without_a_file_part() {
        assert!(parse_multipart(&body(&[("note", None, None, b"hi")]), "XyZ").is_err());
        assert!(parse_multipart(&body(&[("file", None, None, b"hi")]), "XyZ").is_err());
        assert!(parse_multipart(b"plain text", "XyZ").is_err());
        assert!(parse_multipart(b"--XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a\"\r\n\r\ntruncated", "XyZ").is_err());
    }

    #[test]
    fn cleans_names_and_types() {
        assert_eq!(clean_name("..\\..\\etc/passwd"), "passwd");
        assert_eq!(clean_name("a\u{0}b\r\n.txt"), "ab.txt");
        assert_eq!(clean_name(".."), "file");
        assert_eq!(clean_type("text/html\r\nX: y"), "application/octet-stream");
        assert_eq!(clean_type("application/pdf"), "application/pdf");
        assert_eq!(clean_type(""), "application/octet-stream");
    }

    #[test]
    fn the_disposition_is_an_attachment_with_a_safe_name() {
        let d = disposition("r\"é.pdf");
        assert!(d.starts_with("attachment; filename=\"r__.pdf\""), "{d}");
        assert!(d.ends_with("filename*=UTF-8''r%22%C3%A9.pdf"), "{d}");
    }

    #[test]
    fn reads_the_boundary() {
        assert_eq!(boundary_of("multipart/form-data; boundary=abc").as_deref(), Some("abc"));
        assert_eq!(boundary_of("Multipart/Form-Data; charset=x; boundary=\"a b\"").as_deref(), Some("a b"));
        assert_eq!(boundary_of("application/json"), None);
        assert_eq!(boundary_of("multipart/form-data"), None);
    }
}
