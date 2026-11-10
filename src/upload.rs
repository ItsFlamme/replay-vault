//! Share links: uploads a clip to kappa.lol (a free host; no account, 100 MiB max) and keeps the link plus its delete key in uploads.json, so a link can be taken down later.
//! Only ever runs when the user clicks Upload and confirms.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

const API: &str = "https://kappa.lol/api/";
pub const MAX_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct Link {
    pub link: String,
    pub key: String,
}

/// One running upload or delete, shared with the window.
#[derive(Default)]
pub struct Job {
    pub sent: AtomicU64,
    pub total: AtomicU64,
    /// None while running; Some(Ok(link or "deleted")) / Some(Err(message)) when done.
    pub done: Mutex<Option<Result<String, String>>>,
}

fn file() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-uploads-test.json") } else { crate::config::dir().join("uploads.json") }
}

/// Clip path -> its share link.
pub fn links() -> BTreeMap<String, Link> {
    crate::clips::load_json(&file())
}

/// Changes the saved links. One change at a time, so two uploads ending together both stay; the
/// file is replaced whole (clips::save_json), so a crash never loses the delete keys.
fn edit_links(change: impl FnOnce(&mut BTreeMap<String, Link>)) {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock();
    let mut map = links();
    change(&mut map);
    crate::clips::save_json(&file(), &map);
}

/// Keeps a clip's share link after it's renamed.
pub fn renamed(old: &Path, new: &Path) {
    if links().contains_key(&old.display().to_string()) {
        edit_links(|map| {
            if let Some(l) = map.remove(&old.display().to_string()) {
                map.insert(new.display().to_string(), l);
            }
        });
    }
}

/// Counts bytes as they're read, for the progress bar.
struct Counted<R> {
    inner: R,
    job: Arc<Job>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.job.sent.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(15 * 60)))
        .http_status_as_error(false)
        .build()
        .into()
}

/// Plain words for what the host said.
fn refusal(status: u16, body: &str) -> String {
    match status {
        413 => "That file is too big for kappa.lol (100 MB max). Trim it first.".into(),
        429 => "kappa.lol says that's too many uploads. Wait a bit and try again.".into(),
        500.. => "kappa.lol isn't taking uploads right now. Try again later.".into(),
        _ => format!("kappa.lol refused it ({status}): {}", body.chars().take(120).collect::<String>()),
    }
}

/// Starts uploading `path`; `repaint` is called as it goes and when it's done.
pub fn start(path: &Path, repaint: impl Fn() + Send + 'static) -> Arc<Job> {
    let path = path.to_path_buf();
    spawn(repaint, move |job| {
        let link = send(&path, job)?;
        // The link itself stays out of the log (the log goes into support zips).
        crate::log::write(&format!("Uploaded {} for a share link.", path.display()));
        Ok(link)
    })
}

/// Runs `work` on a thread of its own; its result (an error in words) goes in the job, then
/// `repaint` is called.
fn spawn(repaint: impl Fn() + Send + 'static, work: impl FnOnce(&Arc<Job>) -> anyhow::Result<String> + Send + 'static) -> Arc<Job> {
    let job = Arc::new(Job::default());
    let j = job.clone();
    std::thread::spawn(move || {
        let result = work(&j).map_err(|e| format!("{e:#}"));
        *j.done.lock().unwrap() = Some(result);
        repaint();
    });
    job
}

/// Posts `path` to `url` as a one-file form under `field`, counting the bytes sent into `job`.
/// Returns the status and the reply.
fn post_file(url: &str, field: &str, path: &Path, job: &Arc<Job>) -> Result<(u16, String), ureq::Error> {
    let file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "mp4".into());
    let mime = match ext.as_str() {
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "png" => "image/png",
        "gif" => "image/gif",
        _ => "video/mp4",
    };
    // A generic name: the real one may have a game or a date in it.
    let boundary = format!("replayvault{}", crate::config::random_hex());
    let head = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"clip.{ext}\"\r\nContent-Type: {mime}\r\n\r\n");
    let tail = format!("\r\n--{boundary}--\r\n");
    let total = head.len() as u64 + len + tail.len() as u64;
    // Progress counts the form around the file too, so it ends at exactly 100%.
    job.sent.store(0, Ordering::Relaxed);
    job.total.store(total, Ordering::Relaxed);
    let mut body = Counted { inner: head.as_bytes().chain(file).chain(tail.as_bytes()), job: job.clone() };
    let mut resp = agent()
        .post(url)
        .header("User-Agent", "replay-vault")
        .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
        .header("Content-Length", total.to_string())
        .send(ureq::SendBody::from_reader(&mut body))?;
    let status = resp.status().as_u16();
    Ok((status, resp.body_mut().read_to_string().unwrap_or_default()))
}

fn send(path: &Path, job: &Arc<Job>) -> anyhow::Result<String> {
    let len = std::fs::metadata(path)?.len();
    anyhow::ensure!(len <= MAX_BYTES, "That clip is {} and kappa.lol takes up to 100 MB. Trim it first.", crate::clips::size(len));
    let (status, text) = post_file(&format!("{API}upload"), "file", path, job).map_err(|e| anyhow::anyhow!("Upload failed. Check your connection and try again. ({e})"))?;
    anyhow::ensure!(status == 200, "{}", refusal(status, &text));
    let reply: serde_json::Value = serde_json::from_str(&text).map_err(|_| anyhow::anyhow!("{}", refusal(status, &text)))?;
    let (Some(link), Some(key)) = (reply["link"].as_str(), reply["key"].as_str()) else {
        anyhow::bail!("{}", refusal(status, &text));
    };
    anyhow::ensure!(link.starts_with("https://"), "kappa.lol sent back a link that isn't https, so it wasn't kept.");
    edit_links(|map| _ = map.insert(path.display().to_string(), Link { link: link.into(), key: key.into() }));
    Ok(link.into())
}

/// Takes the uploaded copy of `path` off kappa.lol and forgets the link.
pub fn delete(path: &Path, repaint: impl Fn() + Send + 'static) -> Arc<Job> {
    let id = path.display().to_string();
    spawn(repaint, move |_| {
        let Some(l) = links().get(&id).cloned() else { return Ok("deleted".into()) };
        let mut resp = agent()
            .get(format!("{API}delete"))
            .query("key", &l.key)
            .header("User-Agent", "replay-vault")
            .call()
            .map_err(|e| anyhow::anyhow!("Couldn't reach kappa.lol. The link still works. ({e})"))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        let ok = serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|v| v["success"] == true);
        // 404: already gone from the host.
        anyhow::ensure!(ok || status == 404, "Couldn't delete it, try again in a bit. The link still works.");
        edit_links(|map| _ = map.remove(&id));
        crate::log::write(&format!("Deleted the share link for {id}"));
        Ok("deleted".into())
    })
}

/// Discord's upload limit for webhooks on servers without boosts.
pub const DISCORD_MAX_BYTES: u64 = 10 << 20;

/// A Discord webhook link: https://discord.com/api/webhooks/<id>/<token> (or discordapp.com,
/// ptb. or canary.).
pub fn is_discord_webhook(url: &str) -> bool {
    let hosts = ["discord.com", "discordapp.com", "ptb.discord.com", "canary.discord.com", "ptb.discordapp.com", "canary.discordapp.com"];
    let rest = url.trim().strip_prefix("https://").and_then(|r| hosts.iter().find_map(|h| r.strip_prefix(h)?.strip_prefix("/api/webhooks/")));
    let Some((id, token)) = rest.and_then(|r| r.split_once('/')) else { return false };
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && !token.is_empty() && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Posts `path` to the Discord channel behind `webhook`. Done is Ok("posted").
pub fn post_discord(path: &Path, webhook: &str, repaint: impl Fn() + Send + 'static) -> Arc<Job> {
    let (path, url) = (path.to_path_buf(), webhook.trim().to_string());
    spawn(repaint, move |job| {
        let done = send_discord(&url, &path, job)?;
        // The link is a secret, so only the clip goes in the log.
        crate::log::write(&format!("Posted {} to Discord", path.display()));
        Ok(done)
    })
}

fn send_discord(url: &str, path: &Path, job: &Arc<Job>) -> anyhow::Result<String> {
    let len = std::fs::metadata(path)?.len();
    anyhow::ensure!(
        len <= DISCORD_MAX_BYTES,
        "That clip is {} and Discord takes up to 10 MB here. Use Export for sharing > Discord 10 MB, then post the copy.",
        crate::clips::size(len)
    );
    // Not the error itself: it could have the link in it.
    let (status, text) = post_file(url, "files[0]", path, job).map_err(|_| anyhow::anyhow!("Couldn't reach Discord. Check your connection and try again."))?;
    anyhow::ensure!((200..300).contains(&status), "{}", match status {
        401 | 403 | 404 => "Discord doesn't know that webhook (deleted?). Make a new one and paste it on the General page.".to_string(),
        413 => "That file is too big for this Discord server. Use Export for sharing > Discord 10 MB first.".into(),
        429 => "Discord says that's too many posts. Wait a bit and try again.".into(),
        500.. => "Discord isn't taking uploads right now. Try again later.".into(),
        _ => format!("Discord refused it ({status}): {}", text.chars().take(120).collect::<String>()),
    });
    Ok("posted".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_webhook_links() {
        for ok in [
            "https://discord.com/api/webhooks/123456/abc-DEF_789",
            " https://discordapp.com/api/webhooks/1/x ",
            "https://ptb.discord.com/api/webhooks/1/x",
            "https://canary.discord.com/api/webhooks/1/x",
        ] {
            assert!(is_discord_webhook(ok), "{ok}");
        }
        for bad in [
            "",
            "http://discord.com/api/webhooks/1/x",
            "https://discord.com/api/webhooks/1/",
            "https://discord.com/api/webhooks/abc/x",
            "https://discord.com/api/webhooks/1/x/y",
            "https://discord.com.evil.io/api/webhooks/1/x",
            "https://evil.io/discord.com/api/webhooks/1/x",
            "https://discord.com/channels/1/2",
        ] {
            assert!(!is_discord_webhook(bad), "{bad}");
        }
    }

    /// A one-request server on 127.0.0.1 that answers `status`; hands back what it was sent.
    fn serve(status: &str) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api/webhooks/1/abc", listener.local_addr().unwrap());
        let status = status.to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let (mut request, mut len) = (Vec::new(), 0);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                request.extend_from_slice(line.as_bytes());
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            request.extend(body);
            reader.get_mut().write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").as_bytes()).unwrap();
            request
        });
        (url, server)
    }

    #[test]
    fn posts_to_a_webhook() {
        let clip = std::env::temp_dir().join("instant-replay-discord-test.mp4");
        let bytes: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&clip, &bytes).unwrap();
        let (url, server) = serve("200 OK");
        let job = Arc::new(Job::default());
        assert_eq!(send_discord(&url, &clip, &job).unwrap(), "posted");
        let request = server.join().unwrap();
        assert!(request.starts_with(b"POST /api/webhooks/1/abc HTTP/1.1\r\n"));
        let has = |needle: &[u8]| request.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"name=\"files[0]\"; filename=\"clip.mp4\""));
        assert!(has(&bytes), "the file goes in whole");
        // Progress: the file plus the form around it, all of it sent and no more.
        let total = job.total.load(Ordering::Relaxed);
        assert!(total > bytes.len() as u64);
        assert_eq!(job.sent.load(Ordering::Relaxed), total);
        // A refusal comes back in plain words.
        let (url, server) = serve("429 Too Many Requests");
        let err = send_discord(&url, &clip, &job).unwrap_err().to_string();
        server.join().unwrap();
        assert!(err.contains("too many posts"), "{err}");
        // Too big: said before anything is sent.
        std::fs::File::options().write(true).open(&clip).unwrap().set_len(DISCORD_MAX_BYTES + 1).unwrap();
        let err = send_discord("http://127.0.0.1:9/never", &clip, &job).unwrap_err().to_string();
        assert!(err.contains("Discord 10 MB"), "{err}");
        let _ = std::fs::remove_file(&clip);
    }
}
