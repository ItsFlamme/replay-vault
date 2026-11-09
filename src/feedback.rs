//! Send feedback from the Logs page: a message, an optional way to reply, and (if left ticked) a
//! one-line PC summary and the end of the app log with personal bits taken out, posted as JSON to
//! Replay Vault's feedback worker on a thread of its own.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub const FEEDBACK_URL: &str = "https://feedback.replayvault.app/feedback";
pub const GITHUB_ISSUES: &str = "https://github.com/ItsFlamme/replay-vault/issues/new";
pub const MAX_MESSAGE: usize = 4000;
pub const MAX_CONTACT: usize = 200;
pub const LOG_LINES: usize = 150;
pub const MAX_LOG: usize = 6000;
/// One send per this long.
pub const WAIT: Duration = Duration::from_secs(60);
/// At most this many sends a day, kept across restarts; anything more serious goes to the
/// Discord or GitHub. The feedback worker holds each IP to the same cap.
pub const PER_DAY: usize = 2;
pub const DISCORD: &str = "https://discord.gg/n2KYWUPqGY";
/// A message needs at least this many letters or digits.
pub const MIN_MESSAGE: usize = 10;
const OFFLINE: &str = "Couldn't send. Check your connection and try again.";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Bug,
    Idea,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Bug => "bug",
            Kind::Idea => "idea",
            Kind::Other => "other",
        }
    }
}

pub enum State {
    Idle,
    /// Filled in by the sending thread.
    Sending(Arc<Mutex<Option<Result<(), String>>>>),
    Sent,
    Error(String),
}

/// The feedback card's fields and where sending is at.
pub struct Form {
    pub kind: Kind,
    pub message: String,
    pub contact: String,
    pub include: bool,
    pub state: State,
    pub last_send: Option<Instant>,
    /// When the last sends went out (Unix seconds), from feedback-sends.json.
    pub sends: Vec<u64>,
    /// The last message sent, so the same one can't go twice.
    pub last_message: String,
}

impl Default for Form {
    fn default() -> Self {
        Form { kind: Kind::Bug, message: String::new(), contact: String::new(), include: true, state: State::Idle, last_send: None, sends: Vec::new(), last_message: String::new() }
    }
}

impl Form {
    /// Picks up the sending thread's answer: the message is cleared once it's sent.
    pub fn poll(&mut self) {
        let State::Sending(slot) = &self.state else { return };
        let Some(result) = slot.lock().unwrap().take() else { return };
        self.state = match result {
            Ok(()) => {
                self.last_message = std::mem::take(&mut self.message);
                State::Sent
            }
            Err(e) => State::Error(e),
        };
    }

    /// Seconds until another send is allowed (0 = now): the short wait after each send, and the
    /// daily limit.
    pub fn wait_left(&self) -> u64 {
        let short = self.last_send.map_or(0, |t| WAIT.saturating_sub(t.elapsed()).as_secs_f32().ceil() as u64);
        short.max(limit_wait(&self.sends, now()))
    }

    /// Why the message can't go as it is, if it can't.
    pub fn problem(&self) -> Option<&'static str> {
        let m = self.message.trim();
        if m.chars().filter(|c| c.is_alphanumeric()).count() < MIN_MESSAGE {
            Some("Write a little more first")
        } else if !self.last_message.is_empty() && normalized(m) == normalized(&self.last_message) {
            Some("You already sent this one")
        } else {
            None
        }
    }

    /// Posts `body` on a thread of its own; `repaint` is called when it's done.
    pub fn start(&mut self, body: String, repaint: impl Fn() + Send + 'static) {
        let slot = Arc::new(Mutex::new(None));
        let s = slot.clone();
        std::thread::spawn(move || {
            *s.lock().unwrap() = Some(post(&body));
            repaint();
        });
        self.state = State::Sending(slot);
        self.last_send = Some(Instant::now());
        let now = now();
        self.sends.retain(|&t| now.saturating_sub(t) < DAY);
        self.sends.push(now);
        let _ = std::fs::write(sends_path(), serde_json::to_string(&self.sends).unwrap_or_default());
    }

    /// The form with the sends kept from earlier runs.
    pub fn load() -> Form {
        let sends = std::fs::read_to_string(sends_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Form { sends, ..Form::default() }
    }
}

const DAY: u64 = 24 * 3600;

fn sends_path() -> std::path::PathBuf {
    crate::config::dir().join("feedback-sends.json")
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Seconds until the daily limit allows another send, given earlier sends.
pub fn limit_wait(sends: &[u64], now: u64) -> u64 {
    let mut recent: Vec<u64> = sends.iter().copied().filter(|&t| t <= now && now - t < DAY).collect();
    if recent.len() < PER_DAY {
        return 0;
    }
    recent.sort_unstable();
    // Free again once enough of them are a day old.
    recent[recent.len() - PER_DAY] + DAY - now
}

/// A message with case, spaces and punctuation set aside, to spot the same one sent again.
fn normalized(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn post(body: &str) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .post(FEEDBACK_URL)
        .header("User-Agent", "replay-vault")
        .header("Content-Type", "application/json")
        .send(body)
        .map_err(|_| OFFLINE.to_string())?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(refusal(status, &resp.body_mut().read_to_string().unwrap_or_default()))
}

/// What to show when the server said no: its own words if it gave any (its 429 explains the wait).
pub fn refusal(status: u16, body: &str) -> String {
    let said = serde_json::from_str::<Value>(body).ok().and_then(|v| ["error", "message"].iter().find_map(|k| v[k].as_str().map(str::to_string)));
    let said = said.or_else(|| Some(body.trim().to_string()).filter(|b| !b.is_empty() && !b.starts_with('<') && b.len() <= 300));
    match (status, said) {
        (_, Some(s)) if !s.trim().is_empty() => s.trim().chars().take(300).collect(),
        (429, _) => "That's a lot of feedback at once. Wait a bit and try again.".into(),
        _ => OFFLINE.into(),
    }
}

/// The JSON to post. The message and contact are trimmed and cut to their limits.
pub fn payload(kind: Kind, message: &str, contact: &str, pc: &str, log: &str) -> Value {
    let cut = |s: &str, max: usize| s.trim().chars().take(max).collect::<String>();
    json!({
        "message": cut(message, MAX_MESSAGE),
        "contact": cut(contact, MAX_CONTACT),
        "kind": kind.as_str(),
        "app_version": env!("CARGO_PKG_VERSION"),
        "pc": pc,
        "log": log,
    })
}

/// The last `LOG_LINES` lines, scrubbed, at most `MAX_LOG` characters (whole lines from the end).
pub fn log_tail(lines: &[String], home: Option<&str>, user: Option<&str>, secrets: &[&str]) -> String {
    let mut out: Vec<String> = lines[lines.len().saturating_sub(LOG_LINES)..].iter().map(|l| scrub(l, home, user, secrets)).collect();
    while out.iter().map(|l| l.chars().count() + 1).sum::<usize>() > MAX_LOG + 1 {
        out.remove(0);
    }
    out.join("\n")
}

/// `text` without secrets, the Windows user folder (as ~) and user name, URL query strings,
/// email addresses and anything that looks like a key or token.
pub fn scrub(text: &str, home: Option<&str>, user: Option<&str>, secrets: &[&str]) -> String {
    let mut text = secrets.iter().filter(|s| s.len() >= 4).fold(text.to_string(), |t, s| t.replace(s, "<removed>"));
    // Old logs have kappa.lol share links; taken out the same way as in the support zip.
    text = crate::support::without_links(text);
    if let Some(home) = home.filter(|h| h.len() > 3) {
        text = replace_ci(&replace_ci(&text, home, "~"), &home.replace('\\', "/"), "~");
    }
    if let Some(user) = user.filter(|u| u.len() >= 3) {
        text = replace_ci(&text, user, "<user>");
    }
    text.split_inclusive(char::is_whitespace)
        .map(|w| {
            let end = w.trim_end_matches(char::is_whitespace).len();
            scrub_word(&w[..end]) + &w[end..]
        })
        .collect()
}

fn replace_ci(text: &str, needle: &str, with: &str) -> String {
    let (lower, needle) = (text.to_ascii_lowercase(), needle.to_ascii_lowercase());
    let (mut out, mut at) = (String::new(), 0);
    while let Some(i) = lower[at..].find(&needle) {
        out.push_str(&text[at..at + i]);
        out.push_str(with);
        at += i + needle.len();
    }
    out + &text[at..]
}

fn scrub_word(word: &str) -> String {
    let mut w = word.to_string();
    if let Some(i) = w.find("://")
        && let Some(q) = w[i..].find('?')
    {
        w.truncate(i + q);
        w.push_str("?<removed>");
    }
    // Ids in a link's path (share and delete links, webhook tokens) go; the host stays.
    if let Some(i) = w.find("://")
        && let Some(h) = w[i + 3..].find('/').map(|j| i + 3 + j)
    {
        let path: Vec<&str> = w[h..].split('/').map(|s| if path_token(s) { "<token>" } else { s }).collect();
        w = format!("{}{}", &w[..h], path.join("/"));
    }
    if let Some(at) = w.find('@') {
        let local = |c: char| c.is_alphanumeric() || "._%+-".contains(c);
        let domain = |c: char| c.is_ascii_alphanumeric() || ".-".contains(c);
        // By char, so a letter like ü right before the address never splits a character.
        let start = w[..at].char_indices().rev().find(|&(_, c)| !local(c)).map_or(0, |(i, c)| i + c.len_utf8());
        let end = w[at + 1..].find(|c| !domain(c)).map_or(w.len(), |i| at + 1 + i);
        if start < at && w[at + 1..end].trim_end_matches('.').contains('.') {
            w.replace_range(start..end, "<email>");
        }
    }
    // Runs of key-like characters.
    let mut out = String::new();
    let mut run = String::new();
    for c in w.chars().chain([' ']) {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            run.push(c);
            continue;
        }
        out.push_str(if looks_like_key(&run) { "<token>" } else { &run });
        run.clear();
        out.push(c);
    }
    out.pop();
    out
}

/// A link path part like "aB3dE5fG7h": 10+ letters and digits (with - or _), not a word or a file name.
fn path_token(s: &str) -> bool {
    s.len() >= 10
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && s.chars().any(|c| c.is_ascii_digit())
        && s.chars().any(|c| c.is_ascii_alphabetic())
}

/// 24+ hex digits, or 24+ mostly-letter characters with digits and both cases (base64-ish).
fn looks_like_key(run: &str) -> bool {
    if run.len() < 24 {
        return false;
    }
    if run.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    let letters = run.chars().filter(char::is_ascii_alphabetic).count();
    run.chars().any(|c| c.is_ascii_digit()) && run.chars().any(|c| c.is_ascii_lowercase()) && run.chars().any(|c| c.is_ascii_uppercase()) && letters * 2 >= run.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_personal_bits() {
        let home = r"C:\Users\Dan";
        let s = |t: &str| scrub(t, Some(home), Some("Dan"), &["hunter2-secret"]);
        assert_eq!(s(r"Saved C:\Users\Dan\Videos\a.mp4"), r"Saved ~\Videos\a.mp4");
        assert_eq!(s("from c:/users/dan/Videos"), "from ~/Videos");
        assert_eq!(s("signed in as DAN on PC"), "signed in as <user> on PC");
        assert_eq!(s("phone at http://192.168.1.5:8080/clips?key=abc&x=1 ok"), "phone at http://192.168.1.5:8080/clips?<removed> ok");
        assert_eq!(s("mail me: someone.x+1@mail.example.com, thanks"), "mail me: <email>, thanks");
        assert_eq!(s("user@host"), "user@host", "no dot after the @, not an address");
        assert_eq!(s("password hunter2-secret set"), "password <removed> set");
        assert_eq!(s("key 0123456789abcdef0123456789abcdef"), "key <token>");
        assert_eq!(s("https://discord.com/api/webhooks/123/aBcD3fGh1jKlMn0pQrStUvWxYz_-Q9"), "https://discord.com/api/webhooks/123/<token>");
        // A letter of more than one byte right before the @ (this used to panic).
        assert_eq!(s("from über@mail.example.com ok"), "from <email> ok");
        assert_eq!(s("Saved josé@gmail.com\\a.mp4"), "Saved <email>\\a.mp4");
        assert_eq!(s("ü@"), "ü@");
        // Share and delete links.
        assert_eq!(s("link https://kappa.lol/AbC12 ok"), "link (link removed) ok");
        assert_eq!(s("got https://example.com/files/aB3dE5fG7h9/clip.mp4"), "got https://example.com/files/<token>/clip.mp4");
        assert_eq!(s("https://api.github.com/repos/obsproject/obs-studio/releases/latest"), "https://api.github.com/repos/obsproject/obs-studio/releases/latest");
        // Clip names and ordinary words stay.
        let plain = "Clip saved: Replay_2026-11-08_10-53-22.mp4 (VALORANT), 12.4 MB  Health: OK";
        assert_eq!(s(plain), plain);
        assert_eq!(s("a  b\tc\n"), "a  b\tc\n", "whitespace is kept");
    }

    #[test]
    fn log_tail_is_short() {
        let lines: Vec<String> = (0..400).map(|i| format!("line {i} {}", "x".repeat(60))).collect();
        let tail = log_tail(&lines, None, None, &[]);
        assert!(tail.chars().count() <= MAX_LOG, "{}", tail.len());
        assert!(tail.ends_with(&lines[399]) && tail.starts_with("line "), "whole lines from the end");
        let few: Vec<String> = (0..200).map(|i| format!("{i}")).collect();
        let tail = log_tail(&few, None, None, &[]);
        assert_eq!(tail.lines().count(), LOG_LINES);
        assert!(tail.starts_with("50\n") && tail.ends_with("199"));
        assert_eq!(log_tail(&[], None, None, &[]), "");
    }

    #[test]
    fn payload_fields() {
        let long = "é".repeat(MAX_MESSAGE + 50);
        let p = payload(Kind::Idea, &format!("  {long}  "), &"c".repeat(500), "Windows 11; RTX", "log");
        assert_eq!(p["message"].as_str().unwrap().chars().count(), MAX_MESSAGE);
        assert_eq!(p["contact"].as_str().unwrap().len(), MAX_CONTACT);
        assert_eq!(p["kind"], "idea");
        assert_eq!(p["app_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!((p["pc"].as_str(), p["log"].as_str()), (Some("Windows 11; RTX"), Some("log")));
        assert_eq!([Kind::Bug, Kind::Idea, Kind::Other].map(Kind::as_str), ["bug", "idea", "other"]);
        assert_eq!(payload(Kind::Other, " hi ", "", "", "")["message"], "hi");
    }

    #[test]
    fn two_a_day() {
        let now = 100 * DAY;
        assert_eq!(limit_wait(&[], now), 0);
        assert_eq!(limit_wait(&[now - 10], now), 0);
        // Two today: free when the older one is a day old.
        assert_eq!(limit_wait(&[now - 100, now - 5000], now), DAY - 5000);
        // Old sends don't count.
        assert_eq!(limit_wait(&[now - DAY - 1; 20], now), 0);
    }

    #[test]
    fn short_and_repeated_messages_are_held() {
        let mut f = Form { message: "ok!!".into(), ..Form::default() };
        assert!(f.problem().is_some());
        f.message = "The save key stopped working.".into();
        assert_eq!(f.problem(), None);
        f.last_message = "the SAVE key stopped working".into();
        assert_eq!(f.problem(), Some("You already sent this one"));
    }

    #[test]
    fn server_refusals() {
        assert_eq!(refusal(429, r#"{"error":"Slow down: one message a minute."}"#), "Slow down: one message a minute.");
        assert!(refusal(429, "").contains("Wait a bit"));
        assert_eq!(refusal(400, r#"{"error":"Message is empty."}"#), "Message is empty.");
        assert_eq!(refusal(502, "<html>bad gateway</html>"), OFFLINE);
        assert_eq!(refusal(500, "Too many requests today"), "Too many requests today");
    }
}
