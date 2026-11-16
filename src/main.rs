//! magi-overlay: self-hosted, MAGI-themed stream overlay for OBS.

mod config;
mod cs2;
mod demo;
mod hub;
mod media;
mod obs;
mod setup;
mod stats;
mod twitch;
mod web;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Parser)]
#[command(version, about = "Self-hosted, MAGI-themed stream overlay for OBS")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the overlay server (default).
    Serve {
        /// Fill every widget with sample data (no Twitch/OBS/CS2 needed): for previews and screenshots.
        #[arg(long)]
        demo: bool,
    },
    /// Log in to Twitch (device code) for follows, viewer counts, badges and channel points.
    Login,
    /// Create or refresh the OBS scenes and browser sources.
    Setup,
    /// Install the CS2 Game State Integration file (restart CS2 afterwards).
    Cs2Config,
    /// Start a countdown on the starting scene: `countdown 5m`, `countdown 90s`, `countdown off`.
    Countdown { time: String, label: Option<String> },
    /// Set the away message on the BRB scene.
    Message { text: String },
    /// Fire a test alert: follow, sub, resub, gift, giftbomb, raid, bits, redeem.
    Test { kind: String, user: Option<String> },
    /// Start a new session (counts for the ending scene) now.
    SessionReset,
    /// Print the overlay's current state.
    Status,
}

fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim().to_lowercase();
    if s == "off" || s == "0" {
        return Ok(0);
    }
    let (num, mul) = if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1)
    } else if let Some((m, sec)) = s.split_once(':') {
        return Ok(m.parse::<i64>()? * 60 + sec.parse::<i64>()?);
    } else {
        (s.as_str(), 60)
    };
    Ok((num.trim().parse::<f64>().context("bad duration")? * mul as f64) as i64)
}

async fn api(cfg: &Config, action: &str, body: Value) -> Result<()> {
    let r = reqwest::Client::new()
        .post(format!("http://{}/api/{action}", cfg.server.bind))
        .header("X-Magi", "1")
        .json(&body)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .context("is `magi-overlay serve` running?")?;
    if !r.status().is_success() {
        bail!("{action}: HTTP {}", r.status());
    }
    Ok(())
}

fn random_token() -> String {
    use std::io::Read;
    let mut b = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_err()
    {
        b = (hub::now_ms() as u128).to_le_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn cs2_config(cfg: &Config) -> Result<()> {
    let dir = cs2::cfg_dir(&cfg.cs2).context("CS2 cfg folder not found: set [cs2] cfg_dir")?;
    let token = if cfg.cs2.gsi_token.is_empty() {
        let t = random_token();
        println!(
            "Add this to [cs2] in {}:\n  gsi_token = \"{t}\"",
            config::config_path().display()
        );
        t
    } else {
        cfg.cs2.gsi_token.clone()
    };
    let path = dir.join("gamestate_integration_magi_overlay.cfg");
    std::fs::write(&path, cs2::gsi_cfg(&cfg.server.bind, &token))?;
    println!("Wrote {} — restart CS2 to load it.", path.display());
    Ok(())
}

async fn serve(cfg: Config, demo: bool) -> Result<()> {
    let hub = hub::Hub::new(cfg.clone());
    let cs2 = cs2::Cs2::new(hub.clone(), cfg.cs2.clone());
    tokio::spawn(stats::run(hub.clone()));
    if demo {
        tokio::spawn(demo::run(hub.clone()));
    } else {
        if !cfg.media.disabled {
            tokio::spawn(media::run(hub.clone(), cfg.media.players.clone()));
        }
        tokio::spawn(obs::watch(hub.clone()));
        tokio::spawn(twitch::start(hub.clone()));
        tokio::spawn(cs2.clone().watchdog());
        tokio::spawn(cs2.clone().leetify());
    }
    {
        let hub = hub.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(3));
            loop {
                t.tick().await;
                hub.flush();
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(&cfg.server.bind)
        .await
        .with_context(|| format!("binding {}", cfg.server.bind))?;
    eprintln!("magi-overlay: http://{}/", cfg.server.bind);
    axum::serve(
        listener,
        web::router(web::App {
            hub: hub.clone(),
            cs2,
        }),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    hub.flush();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();
    let cfg = Config::load()?;
    match cli.cmd.unwrap_or(Cmd::Serve { demo: false }) {
        Cmd::Serve { demo } => serve(cfg, demo).await,
        Cmd::Login => twitch::auth::login(&cfg.twitch.client_id).await,
        Cmd::Setup => setup::run(&cfg).await,
        Cmd::Cs2Config => cs2_config(&cfg),
        Cmd::Countdown { time, label } => {
            api(
                &cfg,
                "countdown",
                json!({ "seconds": parse_duration(&time)?, "label": label.unwrap_or_default() }),
            )
            .await
        }
        Cmd::Message { text } => api(&cfg, "message", json!({ "text": text })).await,
        Cmd::Test { kind, user } => {
            let mut a = json!({ "kind": kind, "user": user.unwrap_or_else(|| "MAGI_TEST".into()) });
            match kind.as_str() {
                "sub" | "resub" => {
                    a["tier"] = "TIER 1".into();
                    a["months"] = 7.into();
                    a["message"] = "Synchronisation ratio holding steady.".into();
                }
                "gift" => {
                    a["tier"] = "TIER 1".into();
                    a["recipient"] = "SHINJI".into();
                }
                "giftbomb" => a["amount"] = 5.into(),
                "raid" => a["amount"] = 42.into(),
                "bits" => {
                    a["amount"] = 500.into();
                    a["message"] = "Cheer500 get in the robot".into();
                }
                "redeem" => {
                    a["reward"] = "Hydrate".into();
                    a["amount"] = 500.into();
                }
                _ => {}
            }
            api(&cfg, "alert", a).await
        }
        Cmd::SessionReset => api(&cfg, "session-reset", json!({})).await,
        Cmd::Status => {
            let v: Value = reqwest::get(format!("http://{}/api/state", cfg.server.bind))
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v["state"])?);
            println!("session: {}", v["session"]);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations() {
        assert_eq!(super::parse_duration("5m").unwrap(), 300);
        assert_eq!(super::parse_duration("90s").unwrap(), 90);
        assert_eq!(super::parse_duration("1.5h").unwrap(), 5400);
        assert_eq!(super::parse_duration("4:30").unwrap(), 270);
        assert_eq!(super::parse_duration("10").unwrap(), 600);
        assert_eq!(super::parse_duration("off").unwrap(), 0);
    }
}
