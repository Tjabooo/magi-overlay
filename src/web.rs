//! HTTP: overlay pages, Server-Sent Events, control API, CS2 GSI endpoint.

use crate::cs2::Cs2;
use crate::hub::{now_ms, Hub};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;

#[derive(Clone)]
pub struct App {
    pub hub: Arc<Hub>,
    pub cs2: Arc<Cs2>,
}

macro_rules! assets {
    ($($name:literal => $mime:literal),* $(,)?) => {
        const ASSETS: &[(&str, &str, &[u8])] = &[$(($name, $mime, include_bytes!(concat!("../web/", $name)))),*];
    };
}

assets! {
    "index.html" => "text/html; charset=utf-8",
    "live.html" => "text/html; charset=utf-8",
    "chatting.html" => "text/html; charset=utf-8",
    "starting.html" => "text/html; charset=utf-8",
    "brb.html" => "text/html; charset=utf-8",
    "ending.html" => "text/html; charset=utf-8",
    "alerts.html" => "text/html; charset=utf-8",
    "control.html" => "text/html; charset=utf-8",
    "magi.css" => "text/css; charset=utf-8",
    "magi.js" => "text/javascript; charset=utf-8",
    "widgets.js" => "text/javascript; charset=utf-8",
}

fn asset(app: &App, name: &str) -> Response {
    let mime = ASSETS.iter().find(|a| a.0 == name).map(|a| a.1);
    let Some(mime) = mime else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(dir) = &app.hub.cfg.server.web_dir {
        if let Ok(b) = std::fs::read(dir.join(name)) {
            return (
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                b,
            )
                .into_response();
        }
    }
    let body = ASSETS
        .iter()
        .find(|a| a.0 == name)
        .map(|a| a.2)
        .unwrap_or_default();
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

async fn index(State(app): State<App>) -> Response {
    asset(&app, "index.html")
}

async fn overlay(State(app): State<App>, Path(name): Path<String>) -> Response {
    let name = name.trim_end_matches(".html");
    if !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    asset(&app, &format!("{name}.html"))
}

async fn file(State(app): State<App>, Path(name): Path<String>) -> Response {
    asset(&app, &name)
}

async fn events(State(app): State<App>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let first = Event::default().data(app.hub.snapshot().to_string());
    let hub = app.hub.clone();
    let live = BroadcastStream::new(app.hub.subscribe()).map(move |m| {
        Ok(match m {
            Ok(s) => Event::default().data(s.as_ref()),
            // Fell behind: resend everything.
            Err(_) => Event::default().data(hub.snapshot().to_string()),
        })
    });
    Sse::new(stream::once(async { Ok(first) }).chain(live)).keep_alive(KeepAlive::default())
}

async fn state(State(app): State<App>) -> Json<Value> {
    Json(app.hub.snapshot())
}

/// Control requests must carry `X-Magi: 1`: a custom header a random web page can't send
/// cross-origin without a CORS preflight (which we never approve).
fn guard(h: &HeaderMap) -> Result<(), StatusCode> {
    if h.get("x-magi").is_some() {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn control(
    State(app): State<App>,
    Path(action): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, StatusCode> {
    act(app, action, headers, body).await
}

// GET /api/channel and /api/presets exist too; a static route shadows /api/{action}, so their POSTs
// need their own handlers.
async fn post_channel(
    State(app): State<App>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, StatusCode> {
    act(app, "channel".into(), headers, body).await
}

async fn post_presets(
    State(app): State<App>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, StatusCode> {
    act(app, "presets".into(), headers, body).await
}

async fn act(
    app: App,
    action: String,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, StatusCode> {
    guard(&headers)?;
    let v: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?
    };
    let hub = &app.hub;
    let res = match action.as_str() {
        "golive" => Some(crate::golive::go(hub.clone(), v.clone()).await),
        "endstream" => Some(crate::golive::end(hub.clone(), v["after_secs"].as_u64()).await),
        "presets" => Some(crate::golive::save_preset(&v)),
        "channel" => Some(crate::golive::update(hub.clone(), v.clone()).await),
        "say" => Some(crate::golive::say(v["text"].as_str().unwrap_or("")).await),
        "scene" => Some(crate::golive::scene(hub.clone(), v["scene"].as_str().unwrap_or("")).await),
        _ => None,
    };
    if let Some(r) = res {
        return Ok(Json(match r {
            Ok(v) => v,
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        }));
    }
    match action.as_str() {
        "countdown" => {
            let secs = v["seconds"].as_i64().unwrap_or(0).clamp(0, 6 * 3600);
            let until = if secs > 0 { Some(now_ms() + secs * 1000) } else { None };
            hub.set("countdown", json!({ "until": until, "label": v["label"].as_str().unwrap_or("") }));
        }
        "message" => hub.set("message", json!({ "text": v["text"].as_str().unwrap_or(""), "set_at": now_ms() })),
        "alert" => {
            let mut a = v.clone();
            a["test"] = true.into();
            if a["user"].is_null() {
                a["user"] = "MAGI_TEST".into();
            }
            hub.alert(a);
        }
        "clear-chat" => hub.chat_remove(None, None),
        "session-reset" => hub.session_start(now_ms()),
        "chat-test" => hub.chat(json!({
            "id": format!("test-{}", now_ms()), "user": v["user"].as_str().unwrap_or("MAGI_TEST"),
            "login": "magi_test", "user_id": "0", "color": "#f5a524", "badges": [],
            "parts": [{ "t": "text", "v": v["text"].as_str().unwrap_or("Pattern blue. All units, stand by.") }],
            "action": false, "kind": "chat", "ts": now_ms(),
        })),
        _ => return Err(StatusCode::NOT_FOUND),
    }
    Ok(Json(json!({ "ok": true })))
}

fn result(r: anyhow::Result<Value>) -> Json<Value> {
    Json(match r {
        Ok(v) => v,
        Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
    })
}

async fn channel() -> Json<Value> {
    result(crate::golive::channel().await)
}

async fn categories(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    result(crate::golive::categories(q.get("q").map(String::as_str).unwrap_or("")).await)
}

async fn presets() -> Json<Value> {
    Json(crate::golive::presets())
}

async fn gsi(State(app): State<App>, body: Bytes) -> StatusCode {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    if app.cs2.gsi(&v) {
        StatusCode::OK
    } else {
        StatusCode::UNAUTHORIZED
    }
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/overlay/{name}", get(overlay))
        .route("/assets/{name}", get(file))
        .route("/events", get(events))
        .route("/api/state", get(state))
        .route("/api/channel", get(channel).post(post_channel))
        .route("/api/categories", get(categories))
        .route("/api/presets", get(presets).post(post_presets))
        .route("/api/{action}", post(control))
        .route("/gsi", post(gsi))
        .with_state(app)
}
