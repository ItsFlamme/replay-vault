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

/// Clip path -> its share link, with the paths in one form (clips::tidy_keys). The same clip
/// written two ways keeps the first link.
pub fn links() -> BTreeMap<String, Link> {
    crate::clips::tidy_keys(crate::clips::load_json(&file()), |_, _| {})
}

/// Changes the saved links. One change at a time, so two uploads ending together both stay; the
/// file is replaced whole (clips::save_json), so a crash never loses the delete keys.
fn edit_links(change: impl FnOnce(&mut BTreeMap<String, Link>)) {
    rekeyed(|map| {
        change(map);
        true
    });
}

/// Keeps a clip's share link after it's renamed.
pub fn renamed(old: &Path, new: &Path) {
    if crate::clips::stored_key(links().keys(), old).is_some() {
        edit_links(|map| {
            if let Some(l) = crate::clips::stored_key(map.keys(), old).cloned().and_then(|k| map.remove(&k)) {
                map.insert(crate::clips::tidy(&new.display().to_string()), l);
            }
        });
    }
}

/// Changes the saved links with `change`, saving them if it says something changed (`edit_links`
/// always does; the clips folder moving may not). False if the save failed.
pub fn rekeyed(change: impl FnOnce(&mut BTreeMap<String, Link>) -> bool) -> bool {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock();
    let mut map = links();
    !change(&mut map) || crate::clips::save_json(&file(), &map)
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
    let path = path.to_path_buf();
    spawn(repaint, move |_| delete_link(&path).map(|_| "deleted".into()))
}

/// Takes the uploaded copy of `path` off kappa.lol and forgets the link, right here (a network
/// call: only on the user's say, or for a clip auto-cleanup recycled). False if it had none.
pub fn delete_link(path: &Path) -> anyhow::Result<bool> {
    let map = links();
    let Some((id, l)) = crate::clips::stored_key(map.keys(), path).and_then(|k| Some((k.clone(), map.get(k)?.clone()))) else { return Ok(false) };
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
    // Found again by its delete key: the clip may have been renamed or archived meanwhile.
    let mut at = None;
    rekeyed(|map| {
        at = forget_link(map, &l.key);
        at.is_some()
    });
    crate::log::write(&format!("Deleted the share link for {}", at.as_deref().unwrap_or(&id)));
    Ok(true)
}

/// Forgets the link with delete key `key` in `map`, wherever it's filed now. The path it was
/// filed under, if it was there.
fn forget_link(map: &mut BTreeMap<String, Link>, key: &str) -> Option<String> {
    let at = map.iter().find(|(_, l)| l.key == key).map(|(k, _)| k.clone())?;
    map.remove(&at);
    Some(at)
}

/// Deletes the share links of the clips in `paths`, one after another, skipping any clip that's
/// there (its link still shows on it) or that can't be told to be gone (`clip_gone`). Done is
/// Ok(how many were deleted), or the last error, or what was kept, with how many went.
pub fn delete_gone(paths: Vec<PathBuf>, repaint: impl Fn() + Send + 'static) -> Arc<Job> {
    spawn(repaint, move |_| delete_each(&paths, clip_gone, delete_link))
}

/// Whether the clip at `path` is gone: Some(true) when its file isn't there yet the folder it was
/// in can be read, Some(false) when it's there, None when that folder can't be read (a drive
/// unplugged, a NAS asleep), so there's no telling. A link deleted can't be put back.
fn clip_gone(path: &Path) -> Option<bool> {
    match path.try_exists() {
        Ok(true) => Some(false),
        Ok(false) if path.parent().is_some_and(|d| std::fs::read_dir(d).is_ok()) => Some(true),
        _ => None,
    }
}

/// `delete_gone`'s work: deletes the link of each clip in `paths` that `gone` says is gone with
/// `delete` (true if it had one). The ones `gone` can't judge are kept and said.
fn delete_each(paths: &[PathBuf], gone: impl Fn(&Path) -> Option<bool>, mut delete: impl FnMut(&Path) -> anyhow::Result<bool>) -> anyhow::Result<String> {
    let (mut deleted, mut kept, mut failed) = (0, 0, None);
    for p in paths {
        match gone(p) {
            Some(true) => match delete(p) {
                Ok(had) => deleted += usize::from(had),
                Err(e) => failed = Some(e),
            },
            Some(false) => {}
            None => kept += 1,
        }
    }
    let kept = match kept {
        0 => String::new(),
        n => format!(" Kept {}: couldn't reach the folder their clips are in, so they may still be there.", crate::count(n, "share link")),
    };
    match failed {
        Some(e) if deleted > 0 => anyhow::bail!("Deleted {deleted} share links; the rest didn't go. {e:#}{kept}"),
        Some(e) => anyhow::bail!("{e:#}{kept}"),
        None if kept.is_empty() => Ok(deleted.to_string()),
        None if deleted == 0 => anyhow::bail!("{}", kept.trim_start()),
        None => anyhow::bail!("Deleted {}.{kept}", crate::count(deleted, "share link")),
    }
}

/// Files the share link `link` (just uploaded) under `path`, where its clip is now: it was
/// filed under where the clip was when the upload began, and the clip may have been renamed or
/// archived meanwhile.
pub fn keep_at(link: &str, path: &Path) {
    rekeyed(|map| rekey_link(map, link, path));
}

/// `keep_at` on `map`. True if it moved.
fn rekey_link(map: &mut BTreeMap<String, Link>, link: &str, path: &Path) -> bool {
    let to = crate::clips::tidy(&path.display().to_string());
    let Some(from) = map.iter().find(|(_, l)| l.link == link).map(|(k, _)| k.clone()) else { return false };
    if from == to {
        return false;
    }
    if let Some(l) = map.remove(&from) {
        map.insert(to, l);
    }
    true
}

/// The keys in `links` whose clip is gone: filed in the clips folder `folder` (or a game folder
/// in it), yet not among `listed`, the clips just read from it. Only where the folder the clip
/// was in was read in full (`read`, see `clips::list_read`): with the clips folder unreachable (a
/// NAS asleep, a drive unplugged) its clips aren't gone. That's also what `delete_gone` asks
/// (`clip_gone`), so a whole game folder deleted isn't judged either. Links filed anywhere else
/// (the archive, an earlier clips folder) aren't judged: that would mean looking at the disk.
pub fn orphans<'a>(links: impl Iterator<Item = &'a String>, folder: &str, listed: &[crate::clips::Clip], read: &[PathBuf]) -> Vec<String> {
    if folder.trim().is_empty() || read.is_empty() {
        return Vec::new();
    }
    let inside: Vec<&String> = links.filter(|k| crate::clips::relative(k, folder).is_some_and(|r| r.components().count() <= 2)).collect();
    if inside.is_empty() {
        return Vec::new();
    }
    let fold = |p: &Path| crate::clips::fold(&p.display().to_string());
    let have: std::collections::HashSet<String> = listed.iter().map(|c| fold(&c.path)).collect();
    let read: std::collections::HashSet<String> = read.iter().map(|d| fold(d)).collect();
    let judged = |k: &String| Path::new(k).parent().is_some_and(|p| read.contains(&fold(p)));
    inside.into_iter().filter(|k| judged(k) && !have.contains(&crate::clips::fold(k))).cloned().collect()
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

    /// An upload that ends after its clip was renamed or archived files its link where the clip
    /// is now; one already there stays.
    #[test]
    fn a_finished_upload_follows_its_clip() {
        let link = |s: &str| Link { link: s.into(), key: "k".into() };
        let mut map = BTreeMap::from([(r"D:\Clips\a.mp4".to_string(), link("https://kappa.lol/a")), (r"D:\Clips\b.mp4".to_string(), link("https://kappa.lol/b"))]);
        assert!(rekey_link(&mut map, "https://kappa.lol/a", Path::new("E:/Archive/VALORANT/a.mp4")));
        assert_eq!(map.keys().collect::<Vec<_>>(), [r"D:\Clips\b.mp4", r"E:\Archive\VALORANT\a.mp4"]);
        assert!(!rekey_link(&mut map, "https://kappa.lol/b", Path::new(r"D:\Clips\b.mp4")), "already there");
        assert!(!rekey_link(&mut map, "https://kappa.lol/gone", Path::new(r"D:\Clips\c.mp4")));
        assert_eq!(map.len(), 2);
    }

    /// A deleted link is forgotten where it's filed now, even if its clip was renamed while the
    /// host was deleting it; the others stay.
    #[test]
    fn a_deleted_link_is_forgotten_after_a_rename() {
        let link = |s: &str, k: &str| Link { link: s.into(), key: k.into() };
        let mut map = BTreeMap::from([(r"D:\Clips\a.mp4".to_string(), link("https://kappa.lol/a", "ka")), (r"D:\Clips\b.mp4".to_string(), link("https://kappa.lol/b", "kb"))]);
        // Renamed between reading the links and the host's answer.
        assert!(rekey_link(&mut map, "https://kappa.lol/a", Path::new(r"D:\Clips\Ace.mp4")));
        assert_eq!(forget_link(&mut map, "ka").as_deref(), Some(r"D:\Clips\Ace.mp4"));
        assert_eq!(map.keys().collect::<Vec<_>>(), [r"D:\Clips\b.mp4"]);
        assert_eq!(forget_link(&mut map, "ka"), None, "already forgotten");
        assert_eq!(map.len(), 1);
    }

    /// A clip is gone only when its file is missing from a folder that can be read; a missing
    /// folder (a drive unplugged, a NAS asleep) can't tell.
    #[test]
    fn a_clip_is_gone_only_from_a_folder_that_reads() {
        let dir = std::env::temp_dir().join(format!("instant-replay-clip-gone-{}", crate::config::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("here.mp4"), b"x").unwrap();
        assert_eq!(clip_gone(&dir.join("gone.mp4")), Some(true));
        assert_eq!(clip_gone(&dir.join("here.mp4")), Some(false));
        assert_eq!(clip_gone(&dir.join("missing").join("a.mp4")), None);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(clip_gone(&dir.join("here.mp4")), None, "the whole folder went");
    }

    /// Only links of clips surely gone are deleted; the ones that can't be told are kept and said.
    #[test]
    fn delete_gone_keeps_links_it_cant_judge() {
        let paths: Vec<PathBuf> = ["gone1", "gone2", "here", "unknown"].map(PathBuf::from).into();
        let gone = |p: &Path| match p.to_str().unwrap() {
            "here" => Some(false),
            "unknown" => None,
            _ => Some(true),
        };
        let mut asked = Vec::new();
        let err = delete_each(&paths, gone, |p| {
            asked.push(p.to_path_buf());
            Ok(true)
        })
        .unwrap_err()
        .to_string();
        assert_eq!(asked, [PathBuf::from("gone1"), PathBuf::from("gone2")]);
        assert_eq!(err, "Deleted 2 share links. Kept 1 share link: couldn't reach the folder their clips are in, so they may still be there.");
        let err = delete_each(&paths[2..], gone, |_| unreachable!()).unwrap_err().to_string();
        assert!(err.starts_with("Kept 1 share link:"), "{err}");
        assert_eq!(delete_each(&paths[..3], gone, |_| Ok(true)).unwrap(), "2");
        let err = delete_each(&paths, gone, |_| anyhow::bail!("Couldn't delete it.")).unwrap_err().to_string();
        assert_eq!(err, "Couldn't delete it. Kept 1 share link: couldn't reach the folder their clips are in, so they may still be there.");
    }

    /// Links of clips missing from the clips folder's list are orphans; links filed elsewhere,
    /// deeper than a game folder, or in a folder that wasn't read in full, aren't judged.
    #[test]
    fn orphaned_links() {
        let clip = |p: &str| crate::clips::Clip { path: p.into(), name: String::new(), bytes: 1, modified: std::time::SystemTime::now(), group: None, is_picture: false };
        let listed = [clip(r"D:\Clips\here.mp4"), clip(r"D:\Clips\VALORANT\Ace.mp4")];
        let keys = [
            r"D:\Clips\here.mp4",
            r"d:\clips\valorant\ace.mp4",
            r"D:\Clips\gone.mp4",
            r"D:\Clips\VALORANT\gone.mp4",
            r"D:\Clips\a\b\deep.mp4",
            r"E:\Archive\VALORANT\x.mp4",
            r"D:\Clips2\x.mp4",
        ]
        .map(String::from);
        let read = [PathBuf::from(r"D:\Clips"), PathBuf::from(r"D:\Clips\VALORANT")];
        assert_eq!(orphans(keys.iter(), "D:/Clips/", &listed, &read), [r"D:\Clips\gone.mp4", r"D:\Clips\VALORANT\gone.mp4"]);
        assert!(orphans(keys[5..].iter(), r"D:\Clips", &[], &read).is_empty());
        assert!(orphans(keys.iter(), "", &[], &read).is_empty(), "no clips folder set");
        assert!(orphans(keys.iter(), r"D:\Clips", &[], &[]).is_empty(), "the clips folder couldn't be read");
        assert_eq!(orphans(keys.iter(), r"D:\Clips", &listed, &read[..1]), [r"D:\Clips\gone.mp4"], "VALORANT couldn't be read");
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
