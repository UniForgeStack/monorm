//! The built SPA (`ui/dist`), embedded in the binary, under `/admin`; it speaks only the admin API.
//! An unknown path inside `/admin` gets the index page, so a reload on a detail URL works.

use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::key::UI_BASE;

include!(concat!(env!("OUT_DIR"), "/ui_files.rs"));

/// The place of the API prefix in `index.html`.
const PLACEHOLDER: &str = "__MONORM_API__";

/// Scripts, styles, fonts and requests from this origin only. Images: this origin, `https:` (the brand logo) and `data:`.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: https:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// The import map of a site with packages: a bare `import "react"` in a package module gets the React of the admin.
/// An inline script is blocked under `script-src 'self'`, so the policy of that page adds the hash of this exact text.
pub const IMPORT_MAP: &str = r#"{"imports":{"react":"/admin/shared/react.js","react/jsx-runtime":"/admin/shared/jsx-runtime.js","react-dom":"/admin/shared/react-dom.js","react-dom/client":"/admin/shared/react-dom-client.js"}}"#;

fn csp_with_map() -> String {
    let hash = B64.encode(Sha256::digest(IMPORT_MAP.as_bytes()));
    CSP.replacen("script-src 'self'", &format!("script-src 'self' 'sha256-{hash}'"), 1)
}

/// True for `/admin` and the paths below it that are not API paths.
pub fn is_ui(path: &str, prefix: &str) -> bool {
    let inside = path == UI_BASE || path.strip_prefix(UI_BASE).is_some_and(|rest| rest.starts_with('/'));
    let api = !prefix.is_empty() && (path == prefix || path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('/')));
    inside && !api
}

fn file(name: &str) -> Option<&'static [u8]> {
    FILES.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

fn mime(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" => "application/json",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

fn reply(content_type: &'static str, cache: &'static str, body: Vec<u8>) -> Response {
    reply_with(content_type, cache, body, HeaderValue::from_static(CSP))
}

fn reply_with(content_type: &'static str, cache: &'static str, body: Vec<u8>, csp: HeaderValue) -> Response {
    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(header::CONTENT_SECURITY_POLICY, csp);
    response
}

/// The index page. With `packages`, the import map goes first in `<head>`, before any module script, and the policy allows its hash.
fn index(prefix: &str, packages: bool) -> Option<Response> {
    let mut page = std::str::from_utf8(file("index.html")?).ok()?.replace(PLACEHOLDER, &escape(prefix));
    if !packages {
        return Some(reply("text/html; charset=utf-8", "no-store", page.into_bytes()));
    }
    page = page.replacen("<head>", &format!("<head>\n    <script type=\"importmap\">{IMPORT_MAP}</script>"), 1);
    let csp = HeaderValue::from_str(&csp_with_map()).ok()?;
    Some(reply_with("text/html; charset=utf-8", "no-store", page.into_bytes(), csp))
}

/// The answer for `path` (inside `/admin`, not an API path), or a JSON 404 when no UI is built in.
/// `packages` is true when the site has extension packages: the page then carries the import map.
pub fn respond(method: &Method, path: &str, prefix: &str, packages: bool) -> Response {
    if !matches!(*method, Method::GET | Method::HEAD) || !is_ui(path, prefix) {
        return crate::server::reject(StatusCode::NOT_FOUND, "NotFound", "no such path");
    }
    let rest = path.strip_prefix(UI_BASE).unwrap_or_default().trim_start_matches('/');
    let found = if rest.is_empty() || rest == "index.html" {
        index(prefix, packages)
    } else if let Some(bytes) = file(rest) {
        // The shared React modules keep their name across releases: no long cache.
        let cache = if rest.starts_with("shared/") { "no-cache" } else { "public, max-age=31536000, immutable" };
        Some(reply(mime(rest), cache, bytes.to_vec()))
    } else if rest.starts_with("assets/") || (!rest.contains('/') && rest.contains('.')) {
        // A missing file is a 404: a page route has no dot in its first segment.
        None
    } else {
        index(prefix, packages)
    };
    found.unwrap_or_else(|| crate::server::reject(StatusCode::NOT_FOUND, "NotFound", "no such path"))
}
