//! Shared state + event fan-out to every overlay page (Server-Sent Events).

use crate::config::{state_dir, Config};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

const CHAT_KEEP: usize = 60;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// What happened during the current broadcast (reset when OBS starts streaming).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub started_at: Option<i64>,
    pub follows: Vec<String>,
    pub subs: Vec<String>,
    pub gifts: u64,
    pub raids: Vec<(String, u64)>,
    pub bits: u64,
    pub cheerers: Vec<String>,
    pub redeems: u64,
    pub messages: u64,
    pub chatters: BTreeSet<String>,
    pub peak_viewers: u64,
    /// Survive across sessions.
    pub latest_sub: Option<String>,
    pub latest_cheer: Option<String>,
}

#[derive(Default)]
struct State {
    /// Last value of each "sticky" event type (stats, media, stream, obs, countdown, message).
    sticky: Map<String, Value>,
    chat: VecDeque<Value>,
    session: Session,
}

pub struct Hub {
    tx: broadcast::Sender<Arc<str>>,
    st: Mutex<State>,
    dirty: AtomicBool,
    /// Write session.json (off in demo mode).
    persist: AtomicBool,
    pub cfg: Config,
}

fn push_unique(v: &mut Vec<String>, name: &str) {
    if !name.is_empty() && !v.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        v.push(name.to_owned());
    }
}

impl Hub {
    pub fn new(cfg: Config) -> Arc<Self> {
        let (tx, _) = broadcast::channel(512);
        let session = std::fs::read_to_string(state_dir().join("session.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let mut st = State {
            session,
            ..Default::default()
        };
        st.sticky.insert(
            "message".into(),
            json!({ "text": cfg.overlay.brb_message, "set_at": now_ms() }),
        );
        Arc::new(Self {
            tx,
            st: Mutex::new(st),
            dirty: AtomicBool::new(false),
            persist: AtomicBool::new(true),
            cfg,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.tx.subscribe()
    }

    fn send(&self, kind: &str, mut v: Value) {
        if let Value::Object(m) = &mut v {
            m.insert("type".into(), Value::String(kind.into()));
        }
        let _ = self.tx.send(Arc::from(v.to_string()));
    }

    /// Public, non-secret config the pages need.
    pub fn page_config(&self) -> Value {
        json!({
            "channel": self.cfg.twitch.channel,
            "overlay": self.cfg.overlay,
            "layout": self.cfg.layout,
            "media": !self.cfg.media.disabled,
            "twitch_api": !self.cfg.twitch.client_id.is_empty(),
        })
    }

    pub fn snapshot(&self) -> Value {
        let st = self.st.lock().unwrap();
        json!({
            "type": "snapshot",
            "config": self.page_config(),
            "state": st.sticky,
            "chat": st.chat,
            "session": st.session,
            "now": now_ms(),
        })
    }

    /// Replace a sticky value and tell everyone.
    pub fn set(&self, kind: &str, v: Value) {
        {
            let mut st = self.st.lock().unwrap();
            if st.sticky.get(kind) == Some(&v) {
                return;
            }
            st.sticky.insert(kind.into(), v.clone());
        }
        self.send(kind, v);
    }

    /// Merge fields into a sticky value.
    pub fn merge(&self, kind: &str, fields: Value) {
        let merged = {
            let mut st = self.st.lock().unwrap();
            let cur = st
                .sticky
                .entry(kind.to_owned())
                .or_insert_with(|| json!({}));
            let mut changed = false;
            if let (Value::Object(c), Value::Object(f)) = (cur, fields) {
                for (k, v) in f {
                    if c.get(&k) != Some(&v) {
                        c.insert(k, v);
                        changed = true;
                    }
                }
            }
            if !changed {
                return;
            }
            st.sticky.get(kind).cloned().unwrap_or_default()
        };
        self.send(kind, merged);
    }

    pub fn get(&self, kind: &str) -> Option<Value> {
        self.st.lock().unwrap().sticky.get(kind).cloned()
    }

    pub fn chat(&self, msg: Value) {
        {
            let mut st = self.st.lock().unwrap();
            st.session.messages += 1;
            if let Some(l) = msg.get("login").and_then(|l| l.as_str()) {
                st.session.chatters.insert(
                    msg.get("user")
                        .and_then(|u| u.as_str())
                        .unwrap_or(l)
                        .to_owned(),
                );
            }
            st.chat.push_back(msg.clone());
            while st.chat.len() > CHAT_KEEP {
                st.chat.pop_front();
            }
        }
        self.dirty.store(true, Ordering::Relaxed);
        self.send("chat", msg);
    }

    /// Remove one message (by id), all of a user's messages (by user_id), or everything.
    pub fn chat_remove(&self, id: Option<&str>, user_id: Option<&str>) {
        {
            let mut st = self.st.lock().unwrap();
            st.chat.retain(|m| match (id, user_id) {
                (Some(i), _) => m.get("id").and_then(|v| v.as_str()) != Some(i),
                (None, Some(u)) => m.get("user_id").and_then(|v| v.as_str()) != Some(u),
                (None, None) => false,
            });
        }
        self.send("chat_remove", json!({ "id": id, "user_id": user_id }));
    }

    /// Show an alert. Tests don't count towards the session.
    pub fn alert(&self, a: Value) {
        let test = a.get("test").and_then(|t| t.as_bool()).unwrap_or(false);
        if !test {
            let mut st = self.st.lock().unwrap();
            let s = &mut st.session;
            let user = a
                .get("user")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_owned();
            let amount = a.get("amount").and_then(|n| n.as_u64()).unwrap_or(1);
            match a.get("kind").and_then(|k| k.as_str()).unwrap_or("") {
                "follow" => push_unique(&mut s.follows, &user),
                "sub" | "resub" => {
                    push_unique(&mut s.subs, &user);
                    s.latest_sub = Some(user);
                }
                "gift" => {
                    s.gifts += 1;
                    push_unique(&mut s.subs, &user);
                    s.latest_sub = Some(user);
                }
                "giftbomb" => {
                    s.gifts += amount;
                    push_unique(&mut s.subs, &user);
                    s.latest_sub = Some(user);
                }
                "raid" => s.raids.push((user, amount)),
                "bits" => {
                    s.bits += amount;
                    push_unique(&mut s.cheerers, &user);
                    s.latest_cheer = Some(user);
                }
                "redeem" => s.redeems += 1,
                _ => {}
            }
            drop(st);
            self.dirty.store(true, Ordering::Relaxed);
        }
        self.send("alert", a);
    }

    pub fn session_start(&self, at: i64) {
        {
            let mut st = self.st.lock().unwrap();
            let keep_sub = st.session.latest_sub.take();
            let keep_cheer = st.session.latest_cheer.take();
            st.session = Session {
                started_at: Some(at),
                latest_sub: keep_sub,
                latest_cheer: keep_cheer,
                ..Default::default()
            };
        }
        self.dirty.store(true, Ordering::Relaxed);
        self.flush();
    }

    /// Replace the whole session (demo mode) and stop persisting it.
    pub fn set_session(&self, s: Session) {
        self.persist.store(false, Ordering::Relaxed);
        self.st.lock().unwrap().session = s;
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn session_started(&self) -> Option<i64> {
        self.st.lock().unwrap().session.started_at
    }

    pub fn viewers(&self, n: u64) {
        let mut st = self.st.lock().unwrap();
        if n > st.session.peak_viewers {
            st.session.peak_viewers = n;
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Persist + broadcast the session if it changed (called every few seconds).
    pub fn flush(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let s = serde_json::to_value(&self.st.lock().unwrap().session).unwrap_or_default();
        self.send("session", json!({ "session": s }));
        if !self.persist.load(Ordering::Relaxed) {
            return;
        }
        let dir = state_dir();
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join("session.json.tmp");
        if std::fs::write(&tmp, s.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join("session.json"));
        }
    }
}

/// Twitch chat badge images: "set/version" → URL.
pub type Badges = Arc<Mutex<HashMap<String, String>>>;
