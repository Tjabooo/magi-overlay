//! Configuration: `~/.config/magi-overlay/config.toml` (or `$MAGI_OVERLAY_CONFIG`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: Server,
    pub twitch: Twitch,
    pub obs: Obs,
    pub media: Media,
    pub overlay: Overlay,
    pub layout: Layout,
    pub cs2: crate::cs2::Cs2Config,
    pub golive: GoLive,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GoLive {
    /// Shell command run once you're live, with the notification text on stdin and
    /// $MAGI_TITLE, $MAGI_GAME, $MAGI_URL set (e.g. a Discord webhook post). Empty = off.
    pub notify_command: String,
    /// Scene shown when "end stream" is pressed, and seconds before the stream stops.
    pub ending_scene: String,
    pub end_delay_secs: u64,
}

impl Default for GoLive {
    fn default() -> Self {
        Self {
            notify_command: String::new(),
            ending_scene: "ending".into(),
            end_delay_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Server {
    /// Address the overlay server listens on. Keep it on loopback.
    pub bind: String,
    /// Serve the web pages from this folder instead of the built-in copies (for theming).
    pub web_dir: Option<PathBuf>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7788".into(),
            web_dir: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Twitch {
    /// Channel login (lowercase name in the channel URL).
    pub channel: String,
    /// Client ID of your own Twitch application (public client). Enables follows,
    /// channel-point redemptions, viewer counts and chat badges.
    pub client_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Obs {
    pub url: String,
    /// obs-websocket password. Falls back to `$OBS_WEBSOCKET_PASSWORD`, then to OBS's own
    /// plugin config on this machine.
    pub password: Option<String>,
    /// Name of the camera source used by `setup`.
    pub camera: String,
    /// Name of the game/screen capture source used by `setup` (and hidden by the deck's privacy screen).
    pub screen: String,
    /// Mic input muted by the deck's stream-only mic button.
    pub mic: String,
    /// Scene that shows gameplay (created if missing).
    pub live_scene: String,
}

impl Default for Obs {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:4455".into(),
            password: None,
            camera: "Camera".into(),
            screen: "Screen Capture".into(),
            mic: "Mic/Aux".into(),
            live_scene: "stream".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Media {
    /// playerctl player names to follow, in priority order (empty = any player).
    pub players: Vec<String>,
    /// Hide the now-playing widget entirely.
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Overlay {
    /// Name shown on the overlay (defaults to the channel).
    pub pilot: String,
    pub tagline: String,
    /// Lines for the scrolling ticker.
    pub ticker: Vec<String>,
    /// Seconds a chat message stays on the gameplay overlay (0 = forever).
    pub chat_fade_secs: u64,
    /// Alert sound volume 0.0 – 1.0.
    pub alert_volume: f64,
    /// Default away message on the BRB scene.
    pub brb_message: String,
    /// Line shown on the ending scene.
    pub next_stream: String,
}

impl Default for Overlay {
    fn default() -> Self {
        Self {
            pilot: String::new(),
            tagline: "BROADCAST UNIT".into(),
            ticker: vec![],
            chat_fade_secs: 60,
            alert_volume: 0.45,
            brb_message: "Pilot has left the entry plug. Back shortly.".into(),
            next_stream: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Positions in a 2560×1440 design space; scaled to the real canvas.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    /// Camera on the gameplay scene.
    pub cam: Rect,
    /// Chat on the gameplay scene.
    pub chat: Rect,
    /// Camera on the "chatting" scene.
    pub chatting_cam: Rect,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            cam: Rect {
                x: 2040.0,
                y: 440.0,
                w: 496.0,
                h: 279.0,
            },
            chat: Rect {
                x: 24.0,
                y: 500.0,
                w: 520.0,
                h: 440.0,
            },
            chatting_cam: Rect {
                x: 64.0,
                y: 140.0,
                w: 1632.0,
                h: 918.0,
            },
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("magi-overlay")
}

pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("magi-overlay")
}

pub fn config_path() -> PathBuf {
    std::env::var_os("MAGI_OVERLAY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_dir().join("config.toml"))
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        let mut cfg: Config = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
        } else {
            Config::default()
        };
        cfg.twitch.channel = cfg
            .twitch
            .channel
            .trim()
            .trim_start_matches('#')
            .to_lowercase();
        if cfg.overlay.pilot.is_empty() {
            cfg.overlay.pilot = cfg.twitch.channel.to_uppercase();
        }
        Ok(cfg)
    }

    pub fn obs_password(&self) -> Option<String> {
        if let Some(p) = &self.obs.password {
            return Some(p.clone());
        }
        if let Ok(p) = std::env::var("OBS_WEBSOCKET_PASSWORD") {
            return Some(p);
        }
        let path = dirs::config_dir()?.join("obs-studio/plugin_config/obs-websocket/config.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        if v.get("auth_required").and_then(|b| b.as_bool()) == Some(false) {
            return None;
        }
        v.get("server_password")?.as_str().map(str::to_owned)
    }
}
