//! `magi-overlay setup`: build/refresh the OBS scenes through obs-websocket (idempotent).

use crate::config::{Config, Rect};
use crate::obs::Obs;
use anyhow::Result;
use serde_json::{json, Value};

pub const ALERTS: &str = "MAGI · Alerts";

struct Page {
    source: &'static str,
    path: &'static str,
    fps: u32,
}

const LIVE: Page = Page {
    source: "MAGI · Live",
    path: "live",
    fps: 30,
};
const CHATTING: Page = Page {
    source: "MAGI · Chatting",
    path: "chatting",
    fps: 30,
};
const STARTING: Page = Page {
    source: "MAGI · Starting",
    path: "starting",
    fps: 30,
};
const BRB: Page = Page {
    source: "MAGI · BRB",
    path: "brb",
    fps: 30,
};
const ENDING: Page = Page {
    source: "MAGI · Ending",
    path: "ending",
    fps: 30,
};
const ALERT_PAGE: Page = Page {
    source: ALERTS,
    path: "alerts",
    fps: 60,
};

async fn exists(obs: &Obs, input: &str) -> bool {
    obs.call("GetInputSettings", json!({ "inputName": input }))
        .await
        .is_ok()
}

async fn ensure_scene(obs: &Obs, scene: &str) -> Result<()> {
    let list = obs.call("GetSceneList", json!({})).await?;
    let have = list["scenes"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|s| s["sceneName"] == scene);
    if !have {
        obs.call("CreateScene", json!({ "sceneName": scene }))
            .await?;
        println!("  + scene {scene}");
    }
    Ok(())
}

async fn ensure_browser(
    obs: &Obs,
    scene: &str,
    p: &Page,
    base: &str,
    w: u64,
    h: u64,
) -> Result<()> {
    let settings = json!({
        "url": format!("{base}/overlay/{}", p.path),
        "width": w, "height": h, "fps_custom": true, "fps": p.fps,
        // Hidden pages cost nothing; alerts stay loaded so the queue survives scene switches.
        "shutdown": p.source != ALERTS,
        "restart_when_active": false,
        "reroute_audio": false,
        "css": "body { background-color: rgba(0,0,0,0); margin: 0; overflow: hidden; }",
    });
    if exists(obs, p.source).await {
        obs.call(
            "SetInputSettings",
            json!({ "inputName": p.source, "inputSettings": settings, "overlay": true }),
        )
        .await?;
    } else {
        obs.call(
            "CreateInput",
            json!({ "sceneName": scene, "inputName": p.source, "inputKind": "browser_source", "inputSettings": settings }),
        )
        .await?;
        println!("  + source {}", p.source);
    }
    Ok(())
}

async fn ensure_item(obs: &Obs, scene: &str, source: &str) -> Result<i64> {
    if let Ok(v) = obs
        .call(
            "GetSceneItemId",
            json!({ "sceneName": scene, "sourceName": source }),
        )
        .await
    {
        return Ok(v["sceneItemId"].as_i64().unwrap_or(0));
    }
    let v = obs
        .call(
            "CreateSceneItem",
            json!({ "sceneName": scene, "sourceName": source }),
        )
        .await?;
    Ok(v["sceneItemId"].as_i64().unwrap_or(0))
}

async fn place(obs: &Obs, scene: &str, id: i64, t: Value) -> Result<()> {
    obs.call(
        "SetSceneItemTransform",
        json!({ "sceneName": scene, "sceneItemId": id, "sceneItemTransform": t }),
    )
    .await?;
    obs.call(
        "SetSceneItemEnabled",
        json!({ "sceneName": scene, "sceneItemId": id, "sceneItemEnabled": true }),
    )
    .await?;
    Ok(())
}

fn full() -> Value {
    json!({ "positionX": 0.0, "positionY": 0.0, "alignment": 5, "rotation": 0.0, "scaleX": 1.0, "scaleY": 1.0,
            "boundsType": "OBS_BOUNDS_NONE", "cropLeft": 0, "cropRight": 0, "cropTop": 0, "cropBottom": 0 })
}

fn rect(r: Rect, k: f64) -> Value {
    json!({ "positionX": r.x * k, "positionY": r.y * k, "alignment": 5, "rotation": 0.0,
            "boundsType": "OBS_BOUNDS_SCALE_OUTER", "boundsWidth": r.w * k, "boundsHeight": r.h * k,
            "boundsAlignment": 0, "cropLeft": 0, "cropRight": 0, "cropTop": 0, "cropBottom": 0 })
}

/// Stack `order` bottom → top above whatever else is in the scene.
async fn order(obs: &Obs, scene: &str, ids: &[i64]) -> Result<()> {
    let list = obs
        .call("GetSceneItemList", json!({ "sceneName": scene }))
        .await?;
    let total = list["sceneItems"].as_array().map(|a| a.len()).unwrap_or(0);
    let start = total.saturating_sub(ids.len());
    for (i, id) in ids.iter().enumerate() {
        obs.call(
            "SetSceneItemIndex",
            json!({ "sceneName": scene, "sceneItemId": id, "sceneItemIndex": start + i }),
        )
        .await?;
    }
    Ok(())
}

pub async fn run(cfg: &Config) -> Result<()> {
    let obs = Obs::connect(&cfg.obs.url, cfg.obs_password().as_deref()).await?;
    let video = obs.call("GetVideoSettings", json!({})).await?;
    let (w, h) = (
        video["baseWidth"].as_u64().unwrap_or(1920),
        video["baseHeight"].as_u64().unwrap_or(1080),
    );
    let k = w as f64 / 2560.0;
    let base = format!("http://{}", cfg.server.bind);
    println!("OBS canvas {w}x{h}; overlay at {base}");
    let cam = exists(&obs, &cfg.obs.camera).await;
    let screen = exists(&obs, &cfg.obs.screen).await;
    if !cam {
        println!(
            "  ! camera source '{}' not found: scenes get no camera (set [obs] camera)",
            cfg.obs.camera
        );
    }
    if !screen {
        println!(
            "  ! screen source '{}' not found: live scene gets no capture (set [obs] screen)",
            cfg.obs.screen
        );
    }

    let live = cfg.obs.live_scene.as_str();
    let scenes: [(&str, &Page); 5] = [
        ("starting", &STARTING),
        (live, &LIVE),
        ("chatting", &CHATTING),
        ("brb", &BRB),
        ("ending", &ENDING),
    ];
    for (scene, page) in scenes {
        ensure_scene(&obs, scene).await?;
        ensure_browser(&obs, scene, page, &base, w, h).await?;
        ensure_browser(&obs, scene, &ALERT_PAGE, &base, w, h).await?;
        let mut stack = Vec::new();
        if scene == live {
            if screen {
                let id = ensure_item(&obs, scene, &cfg.obs.screen).await?;
                place(&obs, scene, id, full()).await?;
                stack.push(id);
            }
            if cam {
                let id = ensure_item(&obs, scene, &cfg.obs.camera).await?;
                place(&obs, scene, id, rect(cfg.layout.cam, k)).await?;
                stack.push(id);
            }
            let id = ensure_item(&obs, scene, page.source).await?;
            place(&obs, scene, id, full()).await?;
            stack.push(id);
        } else {
            let id = ensure_item(&obs, scene, page.source).await?;
            place(&obs, scene, id, full()).await?;
            stack.push(id);
            if scene == "chatting" && cam {
                let id = ensure_item(&obs, scene, &cfg.obs.camera).await?;
                place(&obs, scene, id, rect(cfg.layout.chatting_cam, k)).await?;
                stack.push(id);
            }
        }
        let id = ensure_item(&obs, scene, ALERTS).await?;
        place(&obs, scene, id, full()).await?;
        stack.push(id);
        order(&obs, scene, &stack).await?;
        println!("  ✓ {scene}");
    }
    println!("Done. Scenes: starting → {live} ↔ chatting ↔ brb → ending.");
    Ok(())
}
