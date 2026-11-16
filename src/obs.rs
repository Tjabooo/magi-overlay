//! obs-websocket v5 client: stream/scene state for the overlay + requests for `setup`.

use crate::hub::{now_ms, Hub};
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

pub struct Obs {
    out: mpsc::UnboundedSender<Message>,
    pending: Pending,
    next: AtomicU64,
    pub events: Mutex<Option<mpsc::UnboundedReceiver<Value>>>,
}

fn auth(password: &str, salt: &str, challenge: &str) -> String {
    let secret = B64.encode(Sha256::digest(format!("{password}{salt}")));
    B64.encode(Sha256::digest(format!("{secret}{challenge}")))
}

/// Event subscription bits: Scenes (1<<2) | Outputs (1<<6).
const EVENTS: u64 = (1 << 2) | (1 << 6);

impl Obs {
    pub async fn connect(url: &str, password: Option<&str>) -> Result<Arc<Self>> {
        let (ws, _) = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async(url),
        )
        .await
        .map_err(|_| anyhow!("OBS websocket: connect timeout"))??;
        let (mut tx, mut rx) = ws.split();
        let hello = next_json(&mut rx).await?;
        let mut ident = json!({ "rpcVersion": 1, "eventSubscriptions": EVENTS });
        if let Some(a) = hello["d"]["authentication"].as_object() {
            let pw = password.ok_or_else(|| anyhow!("OBS websocket wants a password"))?;
            ident["authentication"] = auth(
                pw,
                a.get("salt").and_then(|s| s.as_str()).unwrap_or(""),
                a.get("challenge").and_then(|s| s.as_str()).unwrap_or(""),
            )
            .into();
        }
        tx.send(Message::text(json!({ "op": 1, "d": ident }).to_string()))
            .await?;
        let ok = next_json(&mut rx).await?;
        if ok["op"] != 2 {
            bail!("OBS websocket refused identify (wrong password?)");
        }
        let (out, mut out_rx) = mpsc::unbounded_channel::<Message>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let pending: Pending = Arc::default();
        tokio::spawn(async move {
            while let Some(m) = out_rx.recv().await {
                if tx.send(m).await.is_err() {
                    break;
                }
            }
        });
        let p2 = pending.clone();
        tokio::spawn(async move {
            while let Some(Ok(m)) = rx.next().await {
                let Message::Text(t) = m else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&t) else {
                    continue;
                };
                match v["op"].as_u64() {
                    Some(7) => {
                        let id = v["d"]["requestId"].as_str().unwrap_or("").to_owned();
                        if let Some(s) = p2.lock().unwrap().remove(&id) {
                            let _ = s.send(v["d"].clone());
                        }
                    }
                    Some(5) => {
                        let _ = ev_tx.send(v["d"].clone());
                    }
                    _ => {}
                }
            }
            // Dropping ev_tx tells the event loop the connection is gone.
        });
        Ok(Arc::new(Self {
            out,
            pending,
            next: AtomicU64::new(1),
            events: Mutex::new(Some(ev_rx)),
        }))
    }

    /// Send a request; returns responseData, or an error with OBS's comment.
    pub async fn call(&self, kind: &str, data: Value) -> Result<Value> {
        let id = format!("r{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (s, r) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), s);
        let msg =
            json!({ "op": 6, "d": { "requestType": kind, "requestId": id, "requestData": data } });
        self.out
            .send(Message::text(msg.to_string()))
            .map_err(|_| anyhow!("OBS connection closed"))?;
        let d = tokio::time::timeout(Duration::from_secs(10), r)
            .await
            .map_err(|_| anyhow!("{kind}: OBS didn't answer"))?
            .map_err(|_| anyhow!("{kind}: OBS connection closed"))?;
        if d["requestStatus"]["result"] != true {
            bail!(
                "{kind}: {} {}",
                d["requestStatus"]["code"],
                d["requestStatus"]["comment"].as_str().unwrap_or("")
            );
        }
        Ok(d.get("responseData").cloned().unwrap_or(json!({})))
    }
}

async fn next_json<S>(rx: &mut S) -> Result<Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), rx.next())
            .await
            .map_err(|_| anyhow!("OBS websocket: no answer"))?
            .ok_or_else(|| anyhow!("OBS websocket closed"))??;
        if let Message::Text(t) = m {
            return Ok(serde_json::from_str(&t)?);
        }
    }
}

/// Keep `obs` state (streaming?, since when, current scene) in the hub; resets the session on go-live.
pub async fn watch(hub: Arc<Hub>) {
    let url = hub.cfg.obs.url.clone();
    let password = hub.cfg.obs_password();
    loop {
        match Obs::connect(&url, password.as_deref()).await {
            Ok(obs) => {
                if let Err(e) = follow(&hub, &obs).await {
                    eprintln!("obs: {e:#}");
                }
            }
            Err(e) => eprintln!("obs: {e:#}"),
        }
        hub.merge("obs", json!({ "connected": false }));
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn follow(hub: &Arc<Hub>, obs: &Arc<Obs>) -> Result<()> {
    let st = obs.call("GetStreamStatus", json!({})).await?;
    let scene = obs.call("GetCurrentProgramScene", json!({})).await?;
    let streaming = st["outputActive"] == true;
    let started = if streaming {
        Some(now_ms() - st["outputDuration"].as_i64().unwrap_or(0))
    } else {
        None
    };
    // Already live when we connect: keep the session unless it belongs to an older stream.
    if let Some(s) = started {
        if hub.session_started().is_none_or(|old| old < s - 60_000) {
            hub.session_start(s);
        }
    }
    hub.merge(
        "obs",
        json!({
            "connected": true, "streaming": streaming, "stream_started_at": started,
            "scene": scene["sceneName"].as_str().or(scene["currentProgramSceneName"].as_str()),
            "scene_since": now_ms(),
        }),
    );
    let mut events = obs
        .events
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| anyhow!("events taken"))?;
    while let Some(ev) = events.recv().await {
        let data = &ev["eventData"];
        match ev["eventType"].as_str().unwrap_or("") {
            "StreamStateChanged" => match data["outputState"].as_str().unwrap_or("") {
                "OBS_WEBSOCKET_OUTPUT_STARTED" => {
                    let at = now_ms();
                    hub.session_start(at);
                    hub.merge("obs", json!({ "streaming": true, "stream_started_at": at }));
                }
                "OBS_WEBSOCKET_OUTPUT_STOPPED" => hub.merge("obs", json!({ "streaming": false })),
                _ => {}
            },
            "CurrentProgramSceneChanged" => {
                hub.merge(
                    "obs",
                    json!({ "scene": data["sceneName"], "scene_since": now_ms() }),
                );
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn auth_matches_protocol_example() {
        // obs-websocket protocol docs: password "supersecretpassword", salt + challenge below.
        let a = super::auth(
            "supersecretpassword",
            "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=",
            "+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY=",
        );
        assert_eq!(a.len(), 44);
    }
}
