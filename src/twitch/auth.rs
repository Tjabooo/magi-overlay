//! Twitch user token: device-code login (public client, no secret) + refresh.

use crate::config::config_dir;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::Mutex;

pub const SCOPES: &str =
    "moderator:read:followers channel:read:redemptions channel:read:subscriptions bits:read \
     channel:manage:broadcast moderator:manage:announcements user:write:chat";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default)]
    pub obtained_at: i64,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub scope: Vec<String>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn tokens_path() -> PathBuf {
    config_dir().join("tokens.json")
}

fn save(t: &Tokens) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("tokens.json.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(serde_json::to_string_pretty(t)?.as_bytes())?;
    std::fs::rename(tmp, tokens_path())?;
    Ok(())
}

pub struct TokenStore {
    client_id: String,
    http: reqwest::Client,
    inner: Mutex<Option<Tokens>>,
}

impl TokenStore {
    pub fn load(client_id: &str, http: reqwest::Client) -> Self {
        let t: Option<Tokens> = std::fs::read_to_string(tokens_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .filter(|t: &Tokens| t.client_id.is_empty() || t.client_id == client_id);
        Self {
            client_id: client_id.to_owned(),
            http,
            inner: Mutex::new(t),
        }
    }

    pub async fn has_token(&self) -> bool {
        self.inner.lock().await.is_some()
    }

    /// A valid access token, refreshed when it's about to expire.
    pub async fn access(&self) -> Result<String> {
        let mut g = self.inner.lock().await;
        let t = g
            .as_ref()
            .ok_or_else(|| anyhow!("not logged in (run `magi-overlay login`)"))?;
        if t.expires_in > 0 && now() > t.obtained_at + t.expires_in - 300 {
            let fresh = self.refresh_with(&t.refresh_token).await?;
            *g = Some(fresh);
        }
        Ok(g.as_ref().unwrap().access_token.clone())
    }

    /// Force a refresh (after a 401).
    pub async fn refresh(&self) -> Result<()> {
        let mut g = self.inner.lock().await;
        let rt = g
            .as_ref()
            .ok_or_else(|| anyhow!("not logged in"))?
            .refresh_token
            .clone();
        *g = Some(self.refresh_with(&rt).await?);
        Ok(())
    }

    async fn refresh_with(&self, refresh_token: &str) -> Result<Tokens> {
        let r = self
            .http
            .post("https://id.twitch.tv/oauth2/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", self.client_id.as_str()),
            ])
            .send()
            .await?;
        let status = r.status();
        let v: serde_json::Value = r.json().await.unwrap_or_default();
        if !status.is_success() {
            bail!("token refresh failed ({status}): {v} — run `magi-overlay login` again");
        }
        let mut t: Tokens = serde_json::from_value(v).context("token refresh response")?;
        t.obtained_at = now();
        t.client_id = self.client_id.clone();
        save(&t)?;
        Ok(t)
    }
}

/// Interactive device-code login: prints a URL + code, waits for approval.
pub async fn login(client_id: &str) -> Result<()> {
    if client_id.is_empty() {
        bail!(
            "set [twitch] client_id in {} first",
            crate::config::config_path().display()
        );
    }
    let http = reqwest::Client::new();
    let d: serde_json::Value = http
        .post("https://id.twitch.tv/oauth2/device")
        .form(&[("client_id", client_id), ("scopes", SCOPES)])
        .send()
        .await?
        .json()
        .await?;
    let device_code = d["device_code"]
        .as_str()
        .ok_or_else(|| anyhow!("device flow: {d}"))?;
    let interval = d["interval"].as_u64().unwrap_or(5).max(1);
    println!(
        "Open {}",
        d["verification_uri"]
            .as_str()
            .unwrap_or("https://www.twitch.tv/activate")
    );
    println!(
        "and check the code is {} — then click Authorize.",
        d["user_code"].as_str().unwrap_or("?")
    );
    let deadline = now() + d["expires_in"].as_i64().unwrap_or(1800);
    while now() < deadline {
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let r = http
            .post("https://id.twitch.tv/oauth2/token")
            .form(&[
                ("client_id", client_id),
                ("scopes", SCOPES),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?;
        let v: serde_json::Value = r.json().await.unwrap_or_default();
        if v.get("access_token").is_some() {
            let mut t: Tokens = serde_json::from_value(v)?;
            t.obtained_at = now();
            t.client_id = client_id.to_owned();
            save(&t)?;
            println!("Logged in. Token saved to {}", tokens_path().display());
            return Ok(());
        }
        let msg = v["message"].as_str().unwrap_or("");
        if msg != "authorization_pending" && msg != "slow_down" {
            bail!("login failed: {v}");
        }
    }
    bail!("the code expired before it was approved")
}
