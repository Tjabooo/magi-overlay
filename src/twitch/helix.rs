//! Minimal Helix (Twitch API) client + the channel poller.

use super::auth::TokenStore;
use crate::hub::{Badges, Hub};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub struct Helix {
    pub client_id: String,
    pub http: reqwest::Client,
    pub tokens: TokenStore,
}

const API: &str = "https://api.twitch.tv/helix";

impl Helix {
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        for attempt in 0..2 {
            let token = self.tokens.access().await?;
            let mut rq = self
                .http
                .request(method.clone(), format!("{API}{path}"))
                .bearer_auth(token)
                .header("Client-Id", &self.client_id)
                .timeout(Duration::from_secs(15));
            if let Some(b) = body {
                rq = rq.json(b);
            }
            let r = rq.send().await?;
            let status = r.status();
            if status.as_u16() == 401 && attempt == 0 {
                self.tokens.refresh().await?;
                continue;
            }
            let v: Value = r.json().await.unwrap_or(Value::Null);
            if !status.is_success() {
                bail!(
                    "{path}: {status} {}",
                    v.get("message").and_then(|m| m.as_str()).unwrap_or("")
                );
            }
            return Ok(v);
        }
        bail!("{path}: unauthorized")
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.send(reqwest::Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }

    pub async fn patch(&self, path: &str, body: &Value) -> Result<Value> {
        self.send(reqwest::Method::PATCH, path, Some(body)).await
    }

    pub async fn user_id(&self, login: &str) -> Result<String> {
        let v = self.get(&format!("/users?login={login}")).await?;
        match v["data"][0]["id"].as_str() {
            Some(id) => Ok(id.to_owned()),
            None => bail!("no Twitch user called {login}"),
        }
    }

    /// Fill the badge map from global + channel badges.
    pub async fn load_badges(&self, broadcaster_id: &str, badges: &Badges) {
        for path in [
            "/chat/badges/global".to_owned(),
            format!("/chat/badges?broadcaster_id={broadcaster_id}"),
        ] {
            match self.get(&path).await {
                Ok(v) => {
                    let mut map = badges.lock().unwrap();
                    for set in v["data"].as_array().into_iter().flatten() {
                        let set_id = set["set_id"].as_str().unwrap_or("");
                        for ver in set["versions"].as_array().into_iter().flatten() {
                            if let (Some(id), Some(url)) =
                                (ver["id"].as_str(), ver["image_url_2x"].as_str())
                            {
                                map.insert(format!("{set_id}/{id}"), url.to_owned());
                            }
                        }
                    }
                }
                Err(e) => eprintln!("badges: {e:#}"),
            }
        }
    }
}

/// Every 60 s: live state, viewers, title, game, follower count + latest follower.
pub async fn poll(hub: Arc<Hub>, helix: Arc<Helix>, id: String) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tick.tick().await;
        let mut info = json!({});
        match helix.get(&format!("/streams?user_id={id}")).await {
            Ok(v) => {
                let s = &v["data"][0];
                if s.is_object() {
                    let viewers = s["viewer_count"].as_u64().unwrap_or(0);
                    hub.viewers(viewers);
                    info = json!({
                        "live": true, "viewers": viewers, "title": s["title"], "game": s["game_name"],
                        "started_at": s["started_at"],
                    });
                } else if let Ok(c) = helix.get(&format!("/channels?broadcaster_id={id}")).await {
                    let c = &c["data"][0];
                    info = json!({ "live": false, "viewers": 0, "title": c["title"], "game": c["game_name"], "started_at": null });
                }
            }
            Err(e) => eprintln!("helix streams: {e:#}"),
        }
        match helix
            .get(&format!("/channels/followers?broadcaster_id={id}&first=1"))
            .await
        {
            Ok(v) => {
                info["followers"] = v["total"].clone();
                info["latest_follower"] = v["data"][0]["user_name"].clone();
            }
            Err(e) => eprintln!("helix followers: {e:#}"),
        }
        if info.as_object().is_some_and(|o| !o.is_empty()) {
            hub.merge("stream", info);
        }
    }
}
