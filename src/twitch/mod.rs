pub mod auth;
pub mod eventsub;
pub mod helix;
pub mod irc;

use crate::hub::{Badges, Hub};
use std::sync::Arc;

/// Start chat (always, anonymous) and — with a client ID + login — API polling and EventSub.
pub async fn start(hub: Arc<Hub>) {
    let channel = hub.cfg.twitch.channel.clone();
    if channel.is_empty() {
        eprintln!("twitch: no [twitch] channel set; chat and alerts are off");
        return;
    }
    let badges: Badges = Arc::default();
    let client_id = hub.cfg.twitch.client_id.clone();
    if !client_id.is_empty() {
        let http = reqwest::Client::new();
        let tokens = auth::TokenStore::load(&client_id, http.clone());
        if tokens.has_token().await {
            let helix = Arc::new(helix::Helix {
                client_id,
                http,
                tokens,
            });
            match helix.user_id(&channel).await {
                Ok(id) => {
                    helix.load_badges(&id, &badges).await;
                    tokio::spawn(helix::poll(hub.clone(), helix.clone(), id.clone()));
                    tokio::spawn(eventsub::run(hub.clone(), helix, id));
                }
                Err(e) => eprintln!("twitch api: {e:#} (follows/viewers off)"),
            }
        } else {
            eprintln!(
                "twitch api: not logged in — run `magi-overlay login` for follows, viewers, badges"
            );
        }
    }
    tokio::spawn(irc::run(hub, channel, badges));
}
