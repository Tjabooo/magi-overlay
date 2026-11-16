//! EventSub over WebSocket: follows, channel-point redemptions, stream online/offline.

use super::helix::Helix;
use crate::hub::Hub;
use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

const URL: &str = "wss://eventsub.wss.twitch.tv/ws";

pub async fn run(hub: Arc<Hub>, helix: Arc<Helix>, broadcaster_id: String) {
    let mut followed: HashSet<String> = HashSet::new();
    let mut backoff = 1u64;
    loop {
        match session(&hub, &helix, &broadcaster_id, &mut followed).await {
            Ok(()) => backoff = 1,
            Err(e) => eprintln!("eventsub: {e:#}"),
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(120);
    }
}

async fn subscribe(helix: &Helix, session_id: &str, id: &str) {
    let subs = [
        (
            "channel.follow",
            "2",
            json!({ "broadcaster_user_id": id, "moderator_user_id": id }),
        ),
        (
            "channel.channel_points_custom_reward_redemption.add",
            "1",
            json!({ "broadcaster_user_id": id }),
        ),
        ("stream.online", "1", json!({ "broadcaster_user_id": id })),
        ("stream.offline", "1", json!({ "broadcaster_user_id": id })),
    ];
    for (kind, version, condition) in subs {
        let body = json!({
            "type": kind, "version": version, "condition": condition,
            "transport": { "method": "websocket", "session_id": session_id },
        });
        match helix.post("/eventsub/subscriptions", &body).await {
            Ok(_) => eprintln!("eventsub: subscribed {kind}"),
            // Channel points need Affiliate; not fatal.
            Err(e) => eprintln!("eventsub: {kind} not available: {e:#}"),
        }
    }
}

async fn session(
    hub: &Arc<Hub>,
    helix: &Helix,
    id: &str,
    followed: &mut HashSet<String>,
) -> Result<()> {
    let mut url = URL.to_owned();
    let mut fresh = true;
    loop {
        let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
        let (_tx, mut rx) = ws.split();
        let mut keepalive = 30u64;
        let next_url = loop {
            let msg = tokio::time::timeout(Duration::from_secs(keepalive + 10), rx.next())
                .await
                .map_err(|_| anyhow!("keepalive timeout"))?
                .ok_or_else(|| anyhow!("closed"))??;
            let Message::Text(text) = msg else { continue };
            let v: Value = serde_json::from_str(&text)?;
            match v["metadata"]["message_type"].as_str().unwrap_or("") {
                "session_welcome" => {
                    keepalive = v["payload"]["session"]["keepalive_timeout_seconds"]
                        .as_u64()
                        .unwrap_or(30);
                    if fresh {
                        let sid = v["payload"]["session"]["id"]
                            .as_str()
                            .unwrap_or("")
                            .to_owned();
                        subscribe(helix, &sid, id).await;
                    }
                }
                "session_reconnect" => {
                    break v["payload"]["session"]["reconnect_url"]
                        .as_str()
                        .map(str::to_owned);
                }
                "notification" => notify(hub, &v, followed),
                "revocation" => {
                    eprintln!("eventsub: revoked {}", v["payload"]["subscription"]["type"])
                }
                _ => {}
            }
        };
        match next_url {
            // Twitch moves us to another server; subscriptions carry over.
            Some(u) => {
                url = u;
                fresh = false;
            }
            None => return Ok(()),
        }
    }
}

fn notify(hub: &Hub, v: &Value, followed: &mut HashSet<String>) {
    let e = &v["payload"]["event"];
    match v["payload"]["subscription"]["type"].as_str().unwrap_or("") {
        "channel.follow" => {
            let uid = e["user_id"].as_str().unwrap_or("").to_owned();
            // Twitch re-sends follow events on unfollow/refollow: alert once per run.
            if followed.insert(uid) {
                hub.alert(json!({ "kind": "follow", "user": e["user_name"] }));
                hub.merge("stream", json!({ "latest_follower": e["user_name"] }));
            }
        }
        "channel.channel_points_custom_reward_redemption.add" => hub.alert(json!({
            "kind": "redeem", "user": e["user_name"], "reward": e["reward"]["title"],
            "amount": e["reward"]["cost"], "message": e["user_input"],
        })),
        "stream.online" => hub.merge(
            "stream",
            json!({ "live": true, "started_at": e["started_at"] }),
        ),
        "stream.offline" => hub.merge("stream", json!({ "live": false })),
        _ => {}
    }
}
