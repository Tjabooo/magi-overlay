//! Now playing via `playerctl --follow` (MPRIS).

use crate::hub::Hub;
use serde_json::json;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

const SEP: char = '\u{1f}';

pub async fn run(hub: Arc<Hub>, players: Vec<String>) {
    loop {
        let mut cmd = Command::new("playerctl");
        if !players.is_empty() {
            cmd.arg("--player").arg(players.join(","));
        }
        cmd.args(["--follow", "metadata", "--format"])
            .arg(format!(
                "{{{{status}}}}{SEP}{{{{artist}}}}{SEP}{{{{title}}}}{SEP}{{{{playerName}}}}"
            ))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        match cmd.spawn() {
            Ok(mut child) => {
                if let Some(out) = child.stdout.take() {
                    let mut lines = BufReader::new(out).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let f: Vec<&str> = line.split(SEP).collect();
                        if f.len() < 4 || f[2].is_empty() {
                            hub.set("media", json!({ "status": "Stopped" }));
                            continue;
                        }
                        hub.set("media", json!({ "status": f[0], "artist": f[1], "title": f[2], "player": f[3] }));
                    }
                }
                let _ = child.wait().await;
            }
            Err(e) => {
                eprintln!("media: playerctl unavailable ({e}); now playing disabled");
                return;
            }
        }
        hub.set("media", json!({ "status": "Stopped" }));
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
