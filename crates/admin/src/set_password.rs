//! The built-in password action on the auth user resource.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

use crate::actions::fail;
use crate::audit::{self, Entry};
use crate::auth::{self, AuthConfig, MAX_PASSWORD};
use crate::bridge::{BridgeError, CallContext};
use crate::hostwrite::InputConfig;
use crate::scope::Scoped;
use crate::security::Session;
use crate::server::{reject, State_, User};
use crate::site::{Action, Placement, Resource};
use crate::write::{server_error, user_key_text, Meta};
use crate::{key, revoke};

pub(crate) const NAME: &str = "set_password";
const SET_PASSWORD: &str = "auth.set_password";

pub(crate) fn action(can: Option<Value>) -> Action {
    Action {
        name: NAME.into(),
        label: "Set password".into(),
        placement: Some(Placement::Detail),
        permission: None,
        confirm: Some("Set a new password for {display}? All sessions of this user end.".into()),
        inputs: [
            ("new_password", "New password"),
            ("new_password_again", "New password again"),
        ]
        .into_iter()
        .map(|(name, label)| InputConfig {
            name: name.into(),
            kind: "password".into(),
            label: Some(label.into()),
            required: true,
            help: None,
            choices: Vec::new(),
            secret: true,
        })
        .collect(),
        applies: None,
        can,
        sync: false,
        threaded: false,
    }
}

#[allow(clippy::result_large_err)]
pub(crate) fn inputs(body: &Map<String, Value>) -> Result<&str, Response> {
    let inputs = body
        .get("inputs")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            reject(
                StatusCode::BAD_REQUEST,
                "BadRequest",
                "inputs must be an object",
            )
        })?;
    let password = auth::text(inputs, "new_password", MAX_PASSWORD)?;
    let again = auth::text(inputs, "new_password_again", MAX_PASSWORD)?;
    if password != again {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "PasswordMismatch",
            "The two new passwords differ.",
        ));
    }
    Ok(password)
}

pub(crate) fn not_found() -> Response {
    reject(StatusCode::NOT_FOUND, "NotFound", "no such user")
}

fn context(auth: &AuthConfig, scope: &Scoped) -> CallContext {
    CallContext {
        request_id: auth.security.call_id(),
        scope: json!({}),
        tx: Some(scope.transaction()),
    }
}

pub(crate) async fn target(
    auth: &AuthConfig,
    scope: &Scoped,
    key: &Value,
) -> Result<Value, BridgeError> {
    auth.bridge
        .call(auth::USER, json!({"key": key}), context(auth, scope))
        .await
}

pub(crate) struct Call<'a> {
    pub resource: &'a Resource,
    pub user: &'a User,
    pub session: Option<&'a Session>,
    pub meta: &'a Meta,
    pub key: &'a [Value],
    pub password: &'a str,
}

pub(crate) async fn run(s: &State_, scope: Scoped, call: Call<'_>) -> Response {
    let Some(auth) = s.viewer() else {
        return fail(
            scope,
            reject(
                StatusCode::FORBIDDEN,
                "ReadOnly",
                "this action needs an auth host",
            ),
        )
        .await;
    };
    let [id] = call.key else {
        return fail(scope, not_found()).await;
    };
    let record_key = key::encode(call.key);
    let target = match target(auth, &scope, id).await {
        Ok(v)
            if key::encode(&[v["key"].clone()]) == record_key && v["is_superuser"].is_boolean() =>
        {
            v
        }
        Ok(_) => return fail(scope, not_found()).await,
        Err(e) => return fail(scope, auth::host_error(&e)).await,
    };
    let guarded = target["is_superuser"] == true && call.user.0["is_superuser"] != true;
    let outcome = if guarded {
        Value::Null
    } else {
        match auth
            .bridge
            .call(
                SET_PASSWORD,
                json!({"key": id, "new_password": call.password}),
                context(auth, &scope),
            )
            .await
        {
            Ok(v) => v,
            Err(_) => {
                return fail(
                    scope,
                    server_error(call.meta, "the password host call failed"),
                )
                .await
            }
        }
    };
    let changed = outcome["changed"] == true;
    let hash = auth::password_hash(&outcome);
    if changed && hash.is_none() {
        return fail(
            scope,
            server_error(call.meta, "the password host returned no hash"),
        )
        .await;
    }
    let errors = outcome["errors"].as_array().filter(|items| !items.is_empty()).map(|items| {
        items.iter().map(|error| {
            let mut params = error["params"].clone();
            redact(&mut params, call.password);
            // Rule codes are identifiers. In particular, "password" is a prefix of the built-in codes.
            let code = error["code"].as_str().unwrap_or_default();
            let code = if code == call.password { crate::hostwrite::SECRET_MASK } else { code };
            let message = error["message"].as_str().unwrap_or_default();
            json!({"code": code, "message": message.replace(call.password, crate::hostwrite::SECRET_MASK), "params": params})
        }).collect::<Vec<_>>()
    });
    let mut changes = json!({"ip": call.meta.ip, "request_id": call.meta.request_id});
    if let Some(errors) = &errors {
        changes["codes"] = errors.iter().filter_map(|e| e["code"].as_str()).collect();
    }
    let actor_key = user_key_text(call.user);
    let entry = Entry {
        user_key: &actor_key,
        action: if changed {
            "password_set"
        } else {
            "password_set_failed"
        },
        resource: &call.resource.name,
        record_key: &record_key,
        record_label: target["display"].as_str().unwrap_or_default(),
        changes: Some(changes),
    };
    if let Err(e) = audit::record(&s.site, scope.executor(), entry).await {
        return fail(scope, server_error(call.meta, &e)).await;
    }
    let own = call
        .session
        .filter(|_| changed && actor_key == auth::user_key(&target));
    let sec = &auth.security;
    if let Some(old) = own {
        if let Err(e) = revoke::write(
            &s.site,
            scope.executor(),
            &old.sid,
            old.iat + sec.options.absolute_secs,
            sec.now(),
        )
        .await
        {
            return fail(scope, server_error(call.meta, &e)).await;
        }
    }
    if let Err(e) = scope.commit().await {
        return server_error(call.meta, &e.message);
    }
    if changed {
        let response = Json(json!({"ok": true, "count": 1, "capped": false, "affected": 1, "result": {"kind": "refresh"}})).into_response();
        return match own {
            Some(old) => {
                sec.revoke(old);
                let session = sec.rotate(old, sec.pwd_tag(hash));
                let csrf = sec.csrf_token(&session.sid);
                auth::session_cookies(&s.site, sec, &session, &csrf, response)
            }
            None => response,
        };
    }
    if guarded {
        return reject(
            StatusCode::FORBIDDEN,
            "Forbidden",
            "Only a superuser may set a superuser's password.",
        );
    }
    if let Some(errors) = errors {
        let message = errors
            .iter()
            .filter_map(|v| v["message"].as_str())
            .collect::<Vec<_>>()
            .join(" ");
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": {"kind": "WeakPassword", "message": message, "errors": errors}}))).into_response();
    }
    not_found()
}

/// A custom password rule can return messages and params derived from its input.
fn redact(value: &mut Value, password: &str) {
    match value {
        Value::String(text) => *text = text.replace(password, crate::hostwrite::SECRET_MASK),
        Value::Array(items) => items.iter_mut().for_each(|v| redact(v, password)),
        Value::Object(obj) => {
            *obj = std::mem::take(obj)
                .into_iter()
                .map(|(k, mut v)| {
                    redact(&mut v, password);
                    (k.replace(password, crate::hostwrite::SECRET_MASK), v)
                })
                .collect();
        }
        _ => {}
    }
}
