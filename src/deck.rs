//! Remote-deck helpers (for a touch panel such as a tablet): stream health, a privacy screen, the
//! stream-only mic mute and Twitch clips. Exposed as `GET /api/health` and `POST /api/{privacy,
//! stream-mic,clip}` (all behind the same `X-Magi` guard as the other control routes).

use crate::hub::{now_ms, Hub};
use crate::obs::Obs;
use crate::twitch;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;
use tokio::sync::Mutex;

/// One OBS connection reused across health polls (a tablet asks about once a second).
fn conn() -> &'static Mutex<Option<Arc<Obs>>> {
    static C: OnceLock<Mutex<Option<Arc<Obs>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

async fn obs(hub: &Hub) -> Result<Arc<Obs>> {
    let mut g = conn().lock().await;
    if let Some(o) = g.as_ref() {
        return Ok(o.clone());
    }
    let o = Obs::connect(&hub.cfg.obs.url, hub.cfg.obs_password().as_deref()).await?;
    *g = Some(o.clone());
    Ok(o)
}

async fn call(hub: &Hub, kind: &str, data: Value) -> Result<Value> {
    let o = obs(hub).await.context("OBS")?;
    match tokio::time::timeout(Duration::from_secs(3), o.call(kind, data)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => {
            conn().lock().await.take();
            Err(e)
        }
        Err(_) => {
            conn().lock().await.take();
            bail!("OBS didn't answer {kind}")
        }
    }
}

/// (bytes, ms) of the previous health sample, for the bitrate.
static LAST: StdMutex<Option<(i64, i64)>> = StdMutex::new(None);

pub async fn health(hub: Arc<Hub>) -> Result<Value> {
    let st = call(&hub, "GetStreamStatus", json!({})).await?;
    let stats = call(&hub, "GetStats", json!({}))
        .await
        .unwrap_or(Value::Null);
    let mic = call(
        &hub,
        "GetInputMute",
        json!({ "inputName": hub.cfg.obs.mic }),
    )
    .await
    .ok()
    .and_then(|v| v["inputMuted"].as_bool());
    let live = st["outputActive"] == true;
    let bytes = st["outputBytes"].as_i64().unwrap_or(0);
    let now = now_ms();
    let kbps = {
        let mut last = LAST.lock().unwrap();
        let k = match *last {
            Some((b, t)) if live && now > t && bytes >= b => {
                Some(((bytes - b) * 8) as f64 / (now - t) as f64)
            }
            _ => None,
        };
        *last = Some((bytes, now));
        k
    };
    Ok(json!({
        "streaming": live,
        "reconnecting": st["outputReconnecting"],
        "congestion": st["outputCongestion"],
        "kbps": kbps.map(|k| k.round()),
        "skipped": st["outputSkippedFrames"],
        "total": st["outputTotalFrames"],
        "render_skipped": stats["renderSkippedFrames"],
        "render_total": stats["renderTotalFrames"],
        "fps": stats["activeFps"],
        "cpu": stats["cpuUsage"],
        "mic_muted": mic,
        "privacy": PRIVACY_UNTIL.load(Ordering::Relaxed) > now,
    }))
}

/// Bumped by every privacy call so an older auto-restore timer can't undo a newer one.
static PRIVACY_GEN: AtomicU64 = AtomicU64::new(0);
static PRIVACY_UNTIL: AtomicI64 = AtomicI64::new(0);
/// (scene, item id) pairs hidden by the privacy screen.
static HIDDEN: StdMutex<Vec<(String, i64)>> = StdMutex::new(Vec::new());

/// Hide the screen capture (`[obs] screen`) in every scene for `secs` (default 10), or show it again
/// (`on: false`). A message on the overlay says why the screen is gone.
pub async fn privacy(hub: Arc<Hub>, req: Value) -> Result<Value> {
    let on = req["on"].as_bool().unwrap_or(true);
    let gen = PRIVACY_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    if !on {
        restore(&hub).await;
        return Ok(json!({ "ok": true, "privacy": false }));
    }
    let secs = req["secs"].as_u64().unwrap_or(10).clamp(2, 600);
    let screen = hub.cfg.obs.screen.clone();
    let scenes = call(&hub, "GetSceneList", json!({})).await?;
    let mut hidden = Vec::new();
    for s in scenes["scenes"].as_array().into_iter().flatten() {
        let Some(name) = s["sceneName"].as_str() else {
            continue;
        };
        let items = call(&hub, "GetSceneItemList", json!({ "sceneName": name })).await?;
        for it in items["sceneItems"].as_array().into_iter().flatten() {
            if it["sourceName"] == screen.as_str() && it["sceneItemEnabled"] == true {
                let id = it["sceneItemId"].as_i64().unwrap_or(-1);
                call(
                    &hub,
                    "SetSceneItemEnabled",
                    json!({ "sceneName": name, "sceneItemId": id, "sceneItemEnabled": false }),
                )
                .await?;
                hidden.push((name.to_owned(), id));
            }
        }
    }
    if hidden.is_empty() && HIDDEN.lock().unwrap().is_empty() {
        return Err(anyhow!("no visible source named \"{screen}\" in any scene"));
    }
    HIDDEN.lock().unwrap().extend(hidden);
    PRIVACY_UNTIL.store(now_ms() + secs as i64 * 1000, Ordering::Relaxed);
    hub.set(
        "message",
        json!({ "text": "Screen hidden for a moment", "set_at": now_ms() }),
    );
    let h = hub.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(secs)).await;
        if PRIVACY_GEN.load(Ordering::SeqCst) == gen {
            restore(&h).await;
        }
    });
    Ok(json!({ "ok": true, "privacy": true, "secs": secs }))
}

async fn restore(hub: &Arc<Hub>) {
    let items: Vec<(String, i64)> = std::mem::take(&mut *HIDDEN.lock().unwrap());
    for (scene, id) in items {
        let r = call(
            hub,
            "SetSceneItemEnabled",
            json!({ "sceneName": scene, "sceneItemId": id, "sceneItemEnabled": true }),
        )
        .await;
        if let Err(e) = r {
            eprintln!("deck: privacy restore {scene}/{id}: {e:#}");
        }
    }
    PRIVACY_UNTIL.store(0, Ordering::Relaxed);
    hub.set("message", json!({ "text": "", "set_at": now_ms() }));
}

/// Mute/unmute the stream's mic input (`[obs] mic`) only; Discord and clips keep hearing you.
pub async fn stream_mic(hub: Arc<Hub>, req: Value) -> Result<Value> {
    let input = hub.cfg.obs.mic.clone();
    let muted = match req["muted"].as_bool() {
        Some(m) => {
            call(
                &hub,
                "SetInputMute",
                json!({ "inputName": input, "inputMuted": m }),
            )
            .await?;
            m
        }
        None => call(&hub, "ToggleInputMute", json!({ "inputName": input })).await?["inputMuted"]
            .as_bool()
            .unwrap_or(false),
    };
    Ok(json!({ "ok": true, "muted": muted }))
}

/// Create a Twitch clip of the last ~30 s (needs the `clips:edit` scope and a live stream).
pub async fn clip() -> Result<Value> {
    let (h, id) = twitch::API
        .get()
        .cloned()
        .ok_or_else(|| anyhow!("Twitch API not ready (run `magi-overlay login`)"))?;
    let r = h
        .post(&format!("/clips?broadcaster_id={id}"), &json!({}))
        .await
        .context(
            "creating the clip (live? `magi-overlay login` again if this says unauthorized)",
        )?;
    let cid = r["data"][0]["id"]
        .as_str()
        .ok_or_else(|| anyhow!("Twitch returned no clip"))?;
    Ok(json!({ "ok": true, "id": cid, "url": format!("https://clips.twitch.tv/{cid}") }))
}
