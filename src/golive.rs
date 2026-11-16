//! Go live from outside OBS: set the channel (title, category, tags, language, labels), pick the
//! opening scene + countdown, start the stream, then announce it. Also: end stream, category search,
//! presets (`~/.config/magi-overlay/presets.json`).

use crate::config::config_dir;
use crate::hub::{now_ms, Hub};
use crate::obs::Obs;
use crate::twitch::{self, helix::Helix};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

fn api() -> Result<(Arc<Helix>, String)> {
    twitch::API.get().cloned().ok_or_else(|| {
        anyhow!("Twitch API not ready (set [twitch] client_id and run `magi-overlay login`)")
    })
}

/// Percent-encode a query value.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Bigger box art than the search default (52×72).
fn box_art(url: &str, w: u32, h: u32) -> String {
    match url.rfind('-') {
        Some(i) if url[i..].contains('x') && url.ends_with(".jpg") => {
            format!("{}-{w}x{h}.jpg", &url[..i])
        }
        _ => url
            .replace("{width}", &w.to_string())
            .replace("{height}", &h.to_string()),
    }
}

pub async fn channel() -> Result<Value> {
    let (h, id) = api()?;
    let c = h.get(&format!("/channels?broadcaster_id={id}")).await?;
    let c = &c["data"][0];
    let game_id = c["game_id"].as_str().unwrap_or("").to_owned();
    let mut art = Value::Null;
    if !game_id.is_empty() {
        if let Ok(g) = h.get(&format!("/games?id={game_id}")).await {
            if let Some(u) = g["data"][0]["box_art_url"].as_str() {
                art = box_art(u, 144, 192).into();
            }
        }
    }
    let profanity = c["content_classification_labels"]
        .as_array()
        .is_some_and(|a| a.iter().any(|l| l == "ProfanityVulgarity"));
    Ok(json!({
        "title": c["title"], "game_id": game_id, "game_name": c["game_name"], "art": art,
        "tags": c["tags"], "language": c["broadcaster_language"], "profanity": profanity,
    }))
}

pub async fn categories(q: &str) -> Result<Value> {
    let (h, _) = api()?;
    let v = h
        .get(&format!("/search/categories?query={}&first=8", enc(q)))
        .await?;
    let list: Vec<Value> = v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            json!({
                "id": c["id"], "name": c["name"],
                "art": box_art(c["box_art_url"].as_str().unwrap_or(""), 144, 192),
            })
        })
        .collect();
    Ok(Value::Array(list))
}

fn presets_path() -> std::path::PathBuf {
    config_dir().join("presets.json")
}

pub fn presets() -> Value {
    std::fs::read_to_string(presets_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([]))
}

/// Save (or with `"delete": true`, remove) a preset, matched by name.
pub fn save_preset(p: &Value) -> Result<Value> {
    let name = p["name"]
        .as_str()
        .filter(|n| !n.trim().is_empty())
        .context("preset needs a name")?
        .trim()
        .to_owned();
    let mut list: Vec<Value> = presets().as_array().cloned().unwrap_or_default();
    list.retain(|x| x["name"].as_str() != Some(name.as_str()));
    if p["delete"] != true {
        let mut q = p.clone();
        q["name"] = name.clone().into();
        if let Some(o) = q.as_object_mut() {
            o.remove("delete");
        }
        list.insert(0, q);
        list.truncate(12);
    }
    std::fs::create_dir_all(config_dir())?;
    let tmp = presets_path().with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&list)?)?;
    std::fs::rename(tmp, presets_path())?;
    Ok(Value::Array(list))
}

/// Twitch tag rules: ≤10 tags, ≤25 chars, letters/digits only.
pub fn clean_tags(v: &Value) -> Vec<String> {
    let raw: Vec<String> = match v {
        Value::Array(a) => a
            .iter()
            .filter_map(|t| t.as_str().map(str::to_owned))
            .collect(),
        Value::String(s) => s.split(',').map(str::to_owned).collect(),
        _ => vec![],
    };
    let mut out: Vec<String> = Vec::new();
    for t in raw {
        let t: String = t.chars().filter(|c| c.is_alphanumeric()).take(25).collect();
        if !t.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&t)) {
            out.push(t);
        }
    }
    out.truncate(10);
    out
}

async fn obs(hub: &Hub) -> Result<Arc<Obs>> {
    Obs::connect(&hub.cfg.obs.url, hub.cfg.obs_password().as_deref()).await
}

/// `req`: title, game_id, game_name, tags, language, profanity, countdown_secs, start_scene, notify.
pub async fn go(hub: Arc<Hub>, req: Value) -> Result<Value> {
    let (h, id) = api()?;
    let mut body = json!({});
    if let Some(t) = req["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        body["title"] = t.chars().take(140).collect::<String>().into();
    }
    if let Some(g) = req["game_id"].as_str().filter(|g| !g.is_empty()) {
        body["game_id"] = g.into();
    }
    if !req["tags"].is_null() {
        body["tags"] = json!(clean_tags(&req["tags"]));
    }
    if let Some(l) = req["language"].as_str().filter(|l| l.len() == 2) {
        body["broadcaster_language"] = l.into();
    }
    if let Some(p) = req["profanity"].as_bool() {
        body["content_classification_labels"] =
            json!([{ "id": "ProfanityVulgarity", "is_enabled": p }]);
    }
    h.patch(&format!("/channels?broadcaster_id={id}"), &body)
        .await
        .context("updating the channel")?;
    let mut last = req.clone();
    last["name"] = "Last stream".into();
    let _ = save_preset(&last);

    let o = obs(&hub).await.context("OBS")?;
    let scene = req["start_scene"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("starting")
        .to_owned();
    let secs = req["countdown_secs"]
        .as_i64()
        .unwrap_or(0)
        .clamp(0, 6 * 3600);
    hub.set(
        "countdown",
        json!({ "until": if secs > 0 { Some(now_ms() + secs * 1000) } else { None }, "label": "" }),
    );
    o.call("SetCurrentProgramScene", json!({ "sceneName": scene }))
        .await
        .context("switching scene")?;
    let already = o.call("GetStreamStatus", json!({})).await?["outputActive"] == true;
    if !already {
        o.call("StartStream", json!({}))
            .await
            .context("starting the stream")?;
    }

    // Announce once OBS reports the output as active (Twitch needs a few seconds after that).
    let notify = req["notify"].as_str().unwrap_or("").trim().to_owned();
    let title = body["title"].as_str().unwrap_or("").to_owned();
    let game = req["game_name"].as_str().unwrap_or("").to_owned();
    let hub2 = hub.clone();
    tokio::spawn(async move {
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if hub2.get("obs").is_some_and(|o| o["streaming"] == true) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(8)).await;
        if notify.is_empty() {
            return;
        }
        let msg: String = notify.chars().take(500).collect();
        if let Err(e) = h
            .post(
                &format!("/chat/announcements?broadcaster_id={id}&moderator_id={id}"),
                &json!({ "message": msg, "color": "orange" }),
            )
            .await
        {
            eprintln!("golive: chat announcement failed: {e:#}");
        }
        let cmd = hub2.cfg.golive.notify_command.clone();
        if !cmd.is_empty() {
            let url = format!("https://twitch.tv/{}", hub2.cfg.twitch.channel);
            if let Err(e) = run_notify(&cmd, &format!("{msg}\n{url}"), &title, &game, &url).await {
                eprintln!("golive: notify_command failed: {e:#}");
            }
        }
    });
    Ok(json!({ "ok": true, "scene": scene, "already_live": already, "channel": body }))
}

async fn run_notify(cmd: &str, text: &str, title: &str, game: &str, url: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .env("MAGI_TITLE", title)
        .env("MAGI_GAME", game)
        .env("MAGI_URL", url)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut s) = child.stdin.take() {
        s.write_all(text.as_bytes()).await?;
    }
    let st = tokio::time::timeout(Duration::from_secs(60), child.wait()).await??;
    if !st.success() {
        bail!("exit {st}");
    }
    Ok(())
}

/// Switch to the ending scene now, stop the stream `after` seconds later.
pub async fn end(hub: Arc<Hub>, after: Option<u64>) -> Result<Value> {
    let o = obs(&hub).await.context("OBS")?;
    let scene = hub.cfg.golive.ending_scene.clone();
    let after = after.unwrap_or(hub.cfg.golive.end_delay_secs).min(600);
    let _ = o
        .call("SetCurrentProgramScene", json!({ "sceneName": scene }))
        .await;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(after)).await;
        if let Err(e) = o.call("StopStream", json!({})).await {
            eprintln!("golive: stop stream: {e:#}");
        }
    });
    Ok(json!({ "ok": true, "scene": scene, "stops_in": after }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_follow_twitch_rules() {
        let t = clean_tags(&json!(
            "Swedish, CS2, cs2, no spaces here, ÅÄÖ, a-very-long-tag-that-goes-way-past-25"
        ));
        assert_eq!(
            t,
            vec![
                "Swedish",
                "CS2",
                "nospaceshere",
                "ÅÄÖ",
                "averylongtagthatgoeswaypa"
            ]
        );
        assert!(
            clean_tags(&json!((0..20).map(|i| format!("t{i}")).collect::<Vec<_>>())).len() == 10
        );
    }

    #[test]
    fn box_art_sizes() {
        assert_eq!(
            box_art(
                "https://static-cdn.jtvnw.net/ttv-boxart/32399-52x72.jpg",
                144,
                192
            ),
            "https://static-cdn.jtvnw.net/ttv-boxart/32399-144x192.jpg"
        );
        assert_eq!(
            box_art("https://x/ttv-boxart/1-{width}x{height}.jpg", 144, 192),
            "https://x/ttv-boxart/1-144x192.jpg"
        );
    }

    #[test]
    fn query_encoding() {
        assert_eq!(
            enc("Counter-Strike 2 & co"),
            "Counter-Strike%202%20%26%20co"
        );
    }
}
