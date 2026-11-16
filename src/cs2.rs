//! CS2: live match stats from Game State Integration (POST /gsi) + profile stats from Leetify.

use crate::hub::{now_ms, Hub};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Cs2Config {
    /// SteamID64 for Leetify profile stats (empty = live GSI only).
    pub steam_id: String,
    /// Optional Leetify API key (higher rate limits).
    pub leetify_key: String,
    /// Shared secret CS2 sends with every GSI update (`magi-overlay cs2-config` writes it).
    pub gsi_token: String,
    /// CS2's cfg folder (auto-detected under the Steam library when empty).
    pub cfg_dir: String,
    /// Minutes between Leetify refreshes.
    pub leetify_minutes: u64,
}

impl Default for Cs2Config {
    fn default() -> Self {
        Self {
            steam_id: String::new(),
            leetify_key: String::new(),
            gsi_token: String::new(),
            cfg_dir: String::new(),
            leetify_minutes: 5,
        }
    }
}

/// Per-map accumulation of things GSI only reports per round.
#[derive(Default)]
struct Live {
    map: String,
    hs: HashMap<u64, u64>,
    dmg: HashMap<u64, u64>,
    last_seen: i64,
    phase: String,
}

pub struct Cs2 {
    hub: Arc<Hub>,
    cfg: Cs2Config,
    live: Mutex<Live>,
    /// Ask the Leetify poller to refresh soon (a match just ended).
    pub kick: tokio::sync::Notify,
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_owned()
}
fn n(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

impl Cs2 {
    pub fn new(hub: Arc<Hub>, cfg: Cs2Config) -> Arc<Self> {
        Arc::new(Self {
            hub,
            cfg,
            live: Mutex::new(Live::default()),
            kick: tokio::sync::Notify::new(),
        })
    }

    /// Handle one GSI payload. Returns false if the token doesn't match.
    pub fn gsi(&self, v: &Value) -> bool {
        if !self.cfg.gsi_token.is_empty()
            && v["auth"]["token"].as_str() != Some(self.cfg.gsi_token.as_str())
        {
            return false;
        }
        let me = s(&v["provider"]["steamid"]);
        let map = &v["map"];
        let mut l = self.live.lock().unwrap();
        l.last_seen = now_ms();
        if !map.is_object() {
            // Main menu.
            drop(l);
            self.hub
                .merge("cs2", json!({ "active": false, "in_menu": true }));
            return true;
        }
        let map_name = s(&map["name"]);
        let phase = s(&map["phase"]);
        let round = n(&map["round"]);
        if map_name != l.map || (phase == "warmup" && l.phase != "warmup") {
            l.map = map_name.clone();
            l.hs.clear();
            l.dmg.clear();
        }
        if phase == "gameover" && l.phase != "gameover" {
            self.kick.notify_one();
        }
        l.phase = phase.clone();
        let p = &v["player"];
        let mut out = json!({
            "active": true, "in_menu": false, "map": map_name, "mode": s(&map["mode"]), "phase": phase,
            "round": round, "score_ct": n(&map["team_ct"]["score"]), "score_t": n(&map["team_t"]["score"]),
            "round_phase": s(&v["round"]["phase"]),
        });
        // While dead/spectating, "player" is whoever we watch: only use our own data.
        if s(&p["steamid"]) == me && !me.is_empty() {
            let st = &p["state"];
            let ms = &p["match_stats"];
            let e = l.hs.entry(round).or_default();
            *e = (*e).max(n(&st["round_killhs"]));
            let e = l.dmg.entry(round).or_default();
            *e = (*e).max(n(&st["round_totaldmg"]));
            let hs: u64 = l.hs.values().sum();
            let dmg: u64 = l.dmg.values().sum();
            let kills = n(&ms["kills"]);
            let deaths = n(&ms["deaths"]);
            let played = round.max(1);
            out["team"] = p["team"].clone();
            out["kills"] = kills.into();
            out["assists"] = ms["assists"].clone();
            out["deaths"] = deaths.into();
            out["mvps"] = ms["mvps"].clone();
            out["score"] = ms["score"].clone();
            out["hs_pct"] = if kills > 0 {
                ((hs * 100) as f64 / kills as f64).round().into()
            } else {
                Value::Null
            };
            out["adr"] = if dmg > 0 {
                (dmg as f64 / played as f64).round().into()
            } else {
                Value::Null
            };
            out["kd"] = (((kills as f64 / deaths.max(1) as f64) * 100.0).round() / 100.0).into();
            out["round_kills"] = st["round_kills"].clone();
            out["health"] = st["health"].clone();
            out["alive"] = (n(&st["health"]) > 0).into();
        } else {
            out["alive"] = false.into();
        }
        drop(l);
        self.hub.merge("cs2", out);
        true
    }

    /// Mark the match feed stale when CS2 stops talking (game closed).
    pub async fn watchdog(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let stale = now_ms() - self.live.lock().unwrap().last_seen > 35_000;
            if stale && self.hub.get("cs2").and_then(|c| c["active"].as_bool()) == Some(true) {
                self.hub.merge("cs2", json!({ "active": false }));
            }
        }
    }

    pub async fn leetify(self: Arc<Self>) {
        if self.cfg.steam_id.is_empty() {
            return;
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("magi-overlay/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        let every = Duration::from_secs(self.cfg.leetify_minutes.max(2) * 60);
        loop {
            match self.fetch(&http).await {
                Ok(v) => self.hub.set("leetify", v),
                Err(e) => eprintln!("leetify: {e:#}"),
            }
            // A finished match shows up on Leetify a few minutes later: check at +3 and +8 min.
            tokio::select! {
                _ = tokio::time::sleep(every) => {}
                _ = self.kick.notified() => {
                    for wait in [180u64, 300] {
                        tokio::time::sleep(Duration::from_secs(wait)).await;
                        if let Ok(v) = self.fetch(&http).await { self.hub.set("leetify", v); }
                    }
                }
            }
        }
    }

    async fn fetch(&self, http: &reqwest::Client) -> anyhow::Result<Value> {
        let mut rq = http.get(format!(
            "https://api-public.cs-prod.leetify.com/v3/profile?steam64_id={}",
            self.cfg.steam_id
        ));
        if !self.cfg.leetify_key.is_empty() {
            rq = rq.bearer_auth(&self.cfg.leetify_key);
        }
        let r = rq.send().await?;
        if !r.status().is_success() {
            anyhow::bail!("HTTP {}", r.status());
        }
        let p: Value = r.json().await?;
        Ok(curate(&p, self.hub.session_started()))
    }
}

/// Pick the numbers worth showing and pre-compute session/form summaries.
pub fn curate(p: &Value, session_start: Option<i64>) -> Value {
    let f = |v: &Value| v.as_f64();
    let matches: Vec<&Value> = p["recent_matches"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let ts = |m: &Value| -> i64 {
        // "2026-11-15T18:23:27.000Z" → ms (UTC), no date crate needed.
        let t = m["finished_at"].as_str().unwrap_or("");
        parse_iso_ms(t).unwrap_or(0)
    };
    let form: Vec<Value> = matches
        .iter()
        .take(10)
        .map(|m| {
            json!({
                "outcome": m["outcome"], "map": m["map_name"], "score": m["score"],
                "rating": m["leetify_rating"], "premier": m["rank"], "rank_type": m["rank_type"],
                "finished_at": ts(m),
            })
        })
        .collect();
    // Session = since the stream started, else the last 12 hours.
    let since = session_start.unwrap_or_else(|| now_ms() - 12 * 3600 * 1000);
    let in_session: Vec<&&Value> = matches.iter().filter(|m| ts(m) >= since).collect();
    let wins = in_session.iter().filter(|m| m["outcome"] == "win").count();
    let losses = in_session.iter().filter(|m| m["outcome"] == "loss").count();
    // Premier (rank_type 11) change over the session.
    let premier_of = |m: &Value| {
        if m["rank_type"].as_u64() == Some(11) {
            m["rank"].as_i64().filter(|r| *r > 0)
        } else {
            None
        }
    };
    let latest_premier = matches.iter().find_map(|m| premier_of(m));
    let before = matches
        .iter()
        .filter(|m| ts(m) < since)
        .find_map(|m| premier_of(m));
    let delta = match (latest_premier, before) {
        (Some(a), Some(b)) if !in_session.is_empty() => Some(a - b),
        _ => None,
    };
    let best_map = p["ranks"]["competitive"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .filter(|m| m["rank"].as_u64().unwrap_or(0) > 0)
                .max_by_key(|m| m["rank"].as_u64())
        })
        .map(|m| json!({ "map": m["map_name"], "rank": m["rank"] }));
    let st = &p["stats"];
    let rt = &p["rating"];
    json!({
        "name": p["name"],
        "premier": p["ranks"]["premier"].as_i64().or(latest_premier),
        "leetify": f(&p["ranks"]["leetify"]),
        "aim": f(&rt["aim"]),
        "positioning": f(&rt["positioning"]),
        "utility": f(&rt["utility"]),
        "winrate": f(&p["winrate"]).map(|w| (w * 1000.0).round() / 10.0),
        "matches": p["total_matches"],
        "hs_accuracy": f(&st["accuracy_head"]),
        "counter_strafe": f(&st["counter_strafing_good_shots_ratio"]),
        "spray": f(&st["spray_accuracy"]),
        "t_opening": f(&st["t_opening_duel_success_percentage"]),
        "ct_opening": f(&st["ct_opening_duel_success_percentage"]),
        "trade_kills": f(&st["trade_kills_success_percentage"]),
        "preaim": f(&st["preaim"]),
        "best_map": best_map,
        "form": form,
        "session": { "wins": wins, "losses": losses, "premier_delta": delta, "since": since },
        "fetched_at": now_ms(),
    })
}

/// Parse "YYYY-MM-DDTHH:MM:SS(.fff)Z" to Unix ms.
pub fn parse_iso_ms(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| t.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, s) = (
        num(0, 4)?,
        num(5, 7)?,
        num(8, 10)?,
        num(11, 13)?,
        num(14, 16)?,
        num(17, 19)?,
    );
    // Days from civil (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(((days * 86400) + h * 3600 + mi * 60 + s) * 1000)
}

/// CS2 cfg folder: configured, else the default Steam library location.
pub fn cfg_dir(cfg: &Cs2Config) -> Option<std::path::PathBuf> {
    if !cfg.cfg_dir.is_empty() {
        return Some(cfg.cfg_dir.clone().into());
    }
    let home = dirs::home_dir()?;
    [
        ".local/share/Steam/steamapps/common/Counter-Strike Global Offensive/game/csgo/cfg",
        ".steam/steam/steamapps/common/Counter-Strike Global Offensive/game/csgo/cfg",
    ]
    .iter()
    .map(|p| home.join(p))
    .find(|p| p.is_dir())
}

pub fn gsi_cfg(bind: &str, token: &str) -> String {
    format!(
        r#""magi-overlay"
{{
  "uri"       "http://{bind}/gsi"
  "timeout"   "1.0"
  "buffer"    "0.1"
  "throttle"  "0.5"
  "heartbeat" "10.0"
  "auth" {{ "token" "{token}" }}
  "data"
  {{
    "provider"           "1"
    "map"                "1"
    "round"              "1"
    "player_id"          "1"
    "player_state"       "1"
    "player_match_stats" "1"
  }}
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_iso_ms("2026-11-15T18:23:27.000Z"),
            Some(1_794_767_007_000)
        );
    }

    #[test]
    fn curate_picks_session_and_form() {
        let p = json!({
            "name": "ikari", "winrate": 0.7143, "total_matches": 76,
            "ranks": { "premier": 13594, "leetify": 6.46, "competitive": [
                { "map_name": "de_mirage", "rank": 9 }, { "map_name": "de_nuke", "rank": 0 } ] },
            "rating": { "aim": 96.8 }, "stats": { "accuracy_head": 32.4 },
            "recent_matches": [
                { "outcome": "win", "map_name": "de_mirage", "rank": 13594, "rank_type": 11, "finished_at": "2026-11-15T20:00:00.000Z" },
                { "outcome": "loss", "map_name": "de_anubis", "rank": 13400, "rank_type": 11, "finished_at": "2026-11-15T18:23:27.000Z" },
                { "outcome": "win", "map_name": "de_inferno", "rank": 13300, "rank_type": 11, "finished_at": "2026-11-14T18:00:00.000Z" }
            ]
        });
        let since = parse_iso_ms("2026-11-15T12:00:00Z").unwrap();
        let c = curate(&p, Some(since));
        assert_eq!(c["winrate"], 71.4);
        assert_eq!(c["session"]["wins"], 1);
        assert_eq!(c["session"]["losses"], 1);
        assert_eq!(c["session"]["premier_delta"], 294);
        assert_eq!(c["best_map"]["map"], "de_mirage");
        assert_eq!(c["form"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn gsi_tracks_own_stats_only() {
        let hub = Hub::new(crate::config::Config::default());
        let cs = Cs2::new(
            hub.clone(),
            Cs2Config {
                gsi_token: "t".into(),
                ..Default::default()
            },
        );
        let mut v = json!({
            "auth": { "token": "t" }, "provider": { "steamid": "1" },
            "map": { "name": "de_mirage", "mode": "competitive", "phase": "live", "round": 3,
                     "team_ct": { "score": 2 }, "team_t": { "score": 1 } },
            "player": { "steamid": "1", "team": "CT",
                        "state": { "health": 100, "round_killhs": 1, "round_totaldmg": 150, "round_kills": 2 },
                        "match_stats": { "kills": 4, "assists": 1, "deaths": 2, "mvps": 1, "score": 10 } }
        });
        assert!(cs.gsi(&v));
        let c = hub.get("cs2").unwrap();
        assert_eq!(c["kills"], 4);
        assert_eq!(c["hs_pct"], 25.0);
        assert_eq!(c["adr"], 50.0);
        // Spectating a teammate must not overwrite our numbers.
        v["player"]["steamid"] = "2".into();
        v["player"]["match_stats"]["kills"] = 30.into();
        assert!(cs.gsi(&v));
        assert_eq!(hub.get("cs2").unwrap()["kills"], 4);
        v["auth"]["token"] = "wrong".into();
        assert!(!cs.gsi(&v));
    }
}
