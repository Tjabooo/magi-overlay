//! Anonymous Twitch chat (IRC over WebSocket): messages, subs, gifts, raids, bits, moderation.

use crate::hub::{now_ms, Badges, Hub};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

const URL: &str = "wss://irc-ws.chat.twitch.tv:443";

pub struct Line<'a> {
    pub tags: HashMap<String, String>,
    pub prefix: &'a str,
    pub command: &'a str,
    pub params: Vec<&'a str>,
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('s') => out.push(' '),
                Some(':') => out.push(';'),
                Some('\\') => out.push('\\'),
                Some('r') => out.push('\r'),
                Some('n') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn parse(line: &str) -> Option<Line<'_>> {
    let mut rest = line.trim_end_matches(['\r', '\n']);
    let mut tags = HashMap::new();
    if let Some(r) = rest.strip_prefix('@') {
        let (t, r) = r.split_once(' ')?;
        for kv in t.split(';') {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            tags.insert(k.to_owned(), unescape(v));
        }
        rest = r;
    }
    let mut prefix = "";
    if let Some(r) = rest.strip_prefix(':') {
        let (p, r) = r.split_once(' ')?;
        prefix = p;
        rest = r;
    }
    let (command, mut rest) = rest.split_once(' ').unwrap_or((rest, ""));
    let mut params = Vec::new();
    while !rest.is_empty() {
        if let Some(trailing) = rest.strip_prefix(':') {
            params.push(trailing);
            break;
        }
        let (p, r) = rest.split_once(' ').unwrap_or((rest, ""));
        params.push(p);
        rest = r;
    }
    Some(Line {
        tags,
        prefix,
        command,
        params,
    })
}

/// Split message text into text / emote parts using the `emotes` tag (code-point offsets).
pub fn parts(text: &str, emotes: &str) -> Vec<Value> {
    let chars: Vec<char> = text.chars().collect();
    let mut ranges: Vec<(usize, usize, String)> = Vec::new();
    for e in emotes.split('/').filter(|s| !s.is_empty()) {
        let Some((id, pos)) = e.split_once(':') else {
            continue;
        };
        for r in pos.split(',') {
            if let Some((a, b)) = r.split_once('-') {
                if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                    if a <= b && b < chars.len() {
                        ranges.push((a, b, id.to_owned()));
                    }
                }
            }
        }
    }
    ranges.sort_by_key(|r| r.0);
    let mut out = Vec::new();
    let mut i = 0;
    for (a, b, id) in ranges {
        if a < i {
            continue;
        }
        if a > i {
            out.push(json!({ "t": "text", "v": chars[i..a].iter().collect::<String>() }));
        }
        out.push(json!({
            "t": "emote",
            "v": chars[a..=b].iter().collect::<String>(),
            "url": format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/2.0"),
        }));
        i = b + 1;
    }
    if i < chars.len() {
        out.push(json!({ "t": "text", "v": chars[i..].iter().collect::<String>() }));
    }
    out
}

fn tier(plan: &str) -> &'static str {
    match plan {
        "2000" => "TIER 2",
        "3000" => "TIER 3",
        "Prime" => "PRIME",
        _ => "TIER 1",
    }
}

struct Ctx {
    hub: Arc<Hub>,
    badges: Badges,
    /// community-gift-ids of gift bombs, so their individual gift notices don't alert again.
    bombs: VecDeque<String>,
    seen_ids: HashSet<String>,
}

impl Ctx {
    fn chat_message(&self, l: &Line, text: &str, kind: &str) -> Value {
        let login = l.prefix.split('!').next().unwrap_or("").to_owned();
        let login = l.tags.get("login").cloned().unwrap_or(login);
        let user = l
            .tags
            .get("display-name")
            .filter(|d| !d.is_empty())
            .cloned()
            .unwrap_or(login.clone());
        let (action, text) = match text
            .strip_prefix("\u{1}ACTION ")
            .and_then(|t| t.strip_suffix('\u{1}'))
        {
            Some(t) => (true, t),
            None => (false, text),
        };
        let badge_urls: Vec<String> = {
            let map = self.badges.lock().unwrap();
            l.tags
                .get("badges")
                .map(|b| b.split(',').filter_map(|k| map.get(k).cloned()).collect())
                .unwrap_or_default()
        };
        json!({
            "id": l.tags.get("id").cloned().unwrap_or_default(),
            "user": user,
            "login": login,
            "user_id": l.tags.get("user-id").cloned().unwrap_or_default(),
            "color": l.tags.get("color").cloned().unwrap_or_default(),
            "badges": badge_urls,
            "parts": parts(text, l.tags.get("emotes").map(String::as_str).unwrap_or("")),
            "action": action,
            "kind": kind,
            "ts": now_ms(),
        })
    }

    fn handle(&mut self, l: Line) {
        let tag = |k: &str| l.tags.get(k).cloned().unwrap_or_default();
        let num = |k: &str| {
            l.tags
                .get(k)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        match l.command {
            "PRIVMSG" => {
                let text = l.params.get(1).copied().unwrap_or("");
                let m = self.chat_message(&l, text, "chat");
                let bits = num("bits");
                if bits > 0 {
                    self.hub.alert(json!({ "kind": "bits", "user": m["user"], "amount": bits, "message": text }));
                }
                self.hub.chat(m);
            }
            "USERNOTICE" => {
                let id = tag("id");
                if !id.is_empty() && !self.seen_ids.insert(id) {
                    return;
                }
                let user = tag("display-name");
                let text = l.params.get(1).copied().unwrap_or("");
                match tag("msg-id").as_str() {
                    "sub" | "resub" => self.hub.alert(json!({
                        "kind": tag("msg-id"), "user": user, "tier": tier(&tag("msg-param-sub-plan")),
                        "months": num("msg-param-cumulative-months"), "message": text,
                    })),
                    "subgift" => {
                        let gid = tag("msg-param-community-gift-id");
                        if gid.is_empty() || !self.bombs.contains(&gid) {
                            self.hub.alert(json!({
                                "kind": "gift", "user": user, "tier": tier(&tag("msg-param-sub-plan")),
                                "recipient": tag("msg-param-recipient-display-name"),
                            }));
                        }
                    }
                    "submysterygift" => {
                        let gid = tag("msg-param-community-gift-id");
                        if !gid.is_empty() {
                            self.bombs.push_back(gid);
                            if self.bombs.len() > 50 {
                                self.bombs.pop_front();
                            }
                        }
                        self.hub.alert(json!({
                            "kind": "giftbomb", "user": user, "tier": tier(&tag("msg-param-sub-plan")),
                            "amount": num("msg-param-mass-gift-count"),
                        }));
                    }
                    "raid" => self.hub.alert(json!({
                        "kind": "raid", "user": tag("msg-param-displayName"), "amount": num("msg-param-viewerCount"),
                    })),
                    "announcement" => {
                        let m = self.chat_message(&l, text, "announcement");
                        self.hub.chat(m);
                    }
                    _ => {}
                }
                if self.seen_ids.len() > 500 {
                    self.seen_ids.clear();
                }
            }
            "CLEARMSG" => {
                if let Some(id) = l.tags.get("target-msg-id") {
                    self.hub.chat_remove(Some(id), None);
                }
            }
            "CLEARCHAT" => match l.tags.get("target-user-id") {
                Some(u) => self.hub.chat_remove(None, Some(u)),
                None => self.hub.chat_remove(None, None),
            },
            _ => {}
        }
    }
}

pub async fn run(hub: Arc<Hub>, channel: String, badges: Badges) {
    let mut ctx = Ctx {
        hub,
        badges,
        bombs: VecDeque::new(),
        seen_ids: HashSet::new(),
    };
    let mut backoff = 1u64;
    loop {
        match session(&mut ctx, &channel).await {
            Ok(()) => backoff = 1,
            Err(e) => eprintln!("chat: {e:#}"),
        }
        ctx.hub.merge("chat_link", json!({ "up": false }));
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}

async fn session(ctx: &mut Ctx, channel: &str) -> anyhow::Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(URL).await?;
    let (mut tx, mut rx) = ws.split();
    let nick = format!("justinfan{}", 10000 + now_ms() % 80000);
    for line in [
        "CAP REQ :twitch.tv/tags twitch.tv/commands".to_owned(),
        "PASS SCHMOOPIIE".to_owned(),
        format!("NICK {nick}"),
        format!("JOIN #{channel}"),
    ] {
        tx.send(Message::text(line)).await?;
    }
    eprintln!("chat: joined #{channel}");
    ctx.hub.merge("chat_link", json!({ "up": true }));
    loop {
        // Twitch PINGs about every 5 minutes; silence for 7 means the link is dead.
        let msg = tokio::time::timeout(Duration::from_secs(420), rx.next())
            .await
            .map_err(|_| anyhow::anyhow!("no traffic for 7 minutes"))?;
        let Some(msg) = msg else { return Ok(()) };
        let Message::Text(text) = msg? else { continue };
        for raw in text.split("\r\n").filter(|l| !l.is_empty()) {
            let Some(l) = parse(raw) else { continue };
            match l.command {
                "PING" => {
                    let p = l.params.first().copied().unwrap_or("tmi.twitch.tv");
                    tx.send(Message::text(format!("PONG :{p}"))).await?;
                }
                "RECONNECT" => return Ok(()),
                _ => ctx.handle(l),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_privmsg_with_tags() {
        let l = parse("@badges=broadcaster/1;color=#FF8C1A;display-name=Tjabo;emotes=25:6-10;id=abc :tjabo!tjabo@tjabo.tmi.twitch.tv PRIVMSG #tjabo :hello Kappa").unwrap();
        assert_eq!(l.command, "PRIVMSG");
        assert_eq!(l.params, vec!["#tjabo", "hello Kappa"]);
        assert_eq!(l.tags["display-name"], "Tjabo");
        let p = parts(l.params[1], &l.tags["emotes"]);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0]["v"], "hello ");
        assert_eq!(p[1]["t"], "emote");
        assert_eq!(p[1]["v"], "Kappa");
    }

    #[test]
    fn emote_offsets_are_code_points() {
        let p = parts("åäö Kappa", "25:4-8");
        assert_eq!(p[1]["v"], "Kappa");
    }

    #[test]
    fn unescapes_tag_values() {
        assert_eq!(
            unescape(r"Thanks\sfor\sthe\:\sraid\\"),
            r"Thanks for the; raid\"
        );
    }

    #[test]
    fn bad_emote_ranges_are_ignored() {
        let p = parts("hi", "25:0-40");
        assert_eq!(p, vec![json!({"t": "text", "v": "hi"})]);
    }
}
