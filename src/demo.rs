//! `serve --demo`: sample data in every widget, a chat line every few seconds, an alert every 30 s.

use crate::hub::{now_ms, Hub, Session};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const CHAT: &[(&str, &str, &str)] = &[
    (
        "rei_ayanami",
        "#8fb4d9",
        "o7 pilot, what rank are we pushing today",
    ),
    ("kensuke", "#f5a524", "that smoke lineup was clean"),
    ("misato_k", "#e5484d", "aim rating 94 is actually unreal"),
    ("toji", "#d8cbbf", "PREMIER GRIND LETS GO"),
    ("pen_pen", "#ff8c1a", "Pattern blue. All units, stand by."),
    ("asuka_s", "#ff4d1f", "anta baka? rotate B"),
    ("hikari", "#b2a397", "first time here, the overlay is sick"),
    ("shigeru", "#f5a524", "ADR going crazy this half"),
];

const ALERTS: &[&str] = &[
    "follow", "sub", "raid", "bits", "giftbomb", "resub", "redeem",
];

pub async fn run(hub: Arc<Hub>) {
    let now = now_ms();
    let started = now - (83 * 60 + 12) * 1000;
    let form = |o: &str, m: &str, s: [u64; 2], r: f64| json!({ "outcome": o, "map": m, "score": s, "rating": r, "premier": 18420, "rank_type": 11, "finished_at": now - 3_600_000 });
    hub.set("leetify", json!({
        "name": "Pilot", "premier": 18420, "leetify": 4.21, "aim": 94.2, "positioning": 71.5, "utility": 48.0,
        "winrate": 63.8, "matches": 214, "hs_accuracy": 29.7, "counter_strafe": 86.4, "spray": 41.2,
        "t_opening": 57.3, "ct_opening": 51.9, "trade_kills": 52.0, "preaim": 6.1,
        "best_map": { "map": "de_mirage", "rank": 14 },
        "form": [form("win", "de_mirage", [13, 9], 0.061), form("win", "de_inferno", [13, 11], 0.032), form("loss", "de_anubis", [10, 13], -0.012),
                 form("win", "de_nuke", [13, 6], 0.084), form("win", "de_ancient", [13, 10], 0.027), form("tie", "de_dust2", [12, 12], 0.004),
                 form("loss", "de_mirage", [8, 13], -0.031), form("win", "de_overpass", [13, 7], 0.052), form("win", "de_inferno", [13, 12], 0.019),
                 form("loss", "de_train", [11, 13], -0.008)],
        "session": { "wins": 3, "losses": 1, "premier_delta": 412, "since": started },
        "fetched_at": now,
    }));
    hub.merge("cs2", json!({
        "active": true, "in_menu": false, "map": "de_mirage", "mode": "competitive", "phase": "live", "round": 17,
        "score_ct": 9, "score_t": 7, "team": "CT", "kills": 21, "assists": 5, "deaths": 11, "mvps": 4, "score": 52,
        "hs_pct": 52.0, "adr": 96.0, "kd": 1.91, "round_kills": 0, "health": 100, "alive": true,
    }));
    hub.merge("stream", json!({
        "live": true, "viewers": 128, "title": "Premier grind · road to 20k", "game": "Counter-Strike",
        "started_at": null, "followers": 1337, "latest_follower": "kaworu_n",
    }));
    hub.merge("obs", json!({ "connected": true, "streaming": true, "stream_started_at": started, "scene": "stream", "scene_since": now - 240_000 }));
    hub.set("media", json!({ "status": "Playing", "artist": "Ember Unit", "title": "Synchronisation Test", "player": "spotify" }));
    hub.set("countdown", json!({ "until": now + 298_000, "label": "" }));
    hub.merge("chat_link", json!({ "up": true }));
    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    hub.set_session(Session {
        started_at: Some(started),
        follows: names(&["kaworu_n", "makoto", "maya_ibuki", "gendo_ikari", "yui"]),
        subs: names(&["misato_k", "kensuke", "toji"]),
        gifts: 5,
        raids: vec![("ritsuko_live".into(), 42)],
        bits: 1500,
        cheerers: names(&["shigeru"]),
        redeems: 9,
        messages: 1284,
        chatters: CHAT.iter().map(|c| c.0.to_string()).collect(),
        peak_viewers: 164,
        latest_sub: Some("toji".into()),
        latest_cheer: Some("shigeru".into()),
    });
    for (i, (user, color, text)) in CHAT.iter().enumerate().take(5) {
        hub.chat(chat(i, user, color, text));
    }
    let mut n = 5usize;
    let mut a = 0usize;
    let mut tick = tokio::time::interval(Duration::from_secs(6));
    loop {
        tick.tick().await;
        let (user, color, text) = CHAT[n % CHAT.len()];
        hub.chat(chat(n, user, color, text));
        n += 1;
        if n.is_multiple_of(5) {
            let kind = ALERTS[a % ALERTS.len()];
            a += 1;
            hub.alert(json!({
                "kind": kind, "user": "kaworu_n", "test": true, "tier": "TIER 1", "months": 7, "amount": match kind { "raid" => 42, "bits" => 500, "giftbomb" => 5, "redeem" => 500, _ => 1 },
                "reward": "Hydrate", "recipient": "SHINJI", "message": if kind == "resub" { "Synchronisation ratio holding steady." } else { "" },
            }));
        }
    }
}

fn chat(i: usize, user: &str, color: &str, text: &str) -> serde_json::Value {
    json!({
        "id": format!("demo-{i}"), "user": user, "login": user, "user_id": format!("{i}"), "color": color,
        "badges": [], "parts": [{ "t": "text", "v": text }], "action": false, "kind": "chat", "ts": now_ms(),
    })
}
