//! "Watch clips from your phone": a small read-only web page with the clips, for the home network
//! and Tailscale. Every link starts with a random key; anything else gets 404. Only files that
//! clips::list returns are served, picked by an id made from their path, never by a path taken
//! from the request.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::clips;

pub const PORT: u16 = 4480;
/// More phones or tabs than this at once are turned away.
const MAX_CONNECTIONS: usize = 8;

struct Shared {
    key: String,
    folder: String,
    stop: AtomicBool,
    busy: AtomicUsize,
}

/// The running page; dropping it closes the listeners and cuts off running downloads.
pub struct Server {
    pub addrs: Vec<SocketAddr>,
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Server {
    /// The page's link on each address, like http://192.168.1.20:4480/<key>/.
    pub fn urls(&self) -> Vec<(IpAddr, String)> {
        self.addrs.iter().map(|a| (a.ip(), format!("http://{a}/{}/", self.shared.key))).collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Starts the page on `addrs`, serving the clips in `folder` to links that carry `key`.
pub fn start(addrs: &[SocketAddr], key: &str, folder: &str) -> io::Result<Server> {
    if key.len() < 16 || addrs.is_empty() {
        return Err(io::Error::other(if addrs.is_empty() { "no network found" } else { "no access key" }));
    }
    let shared = Arc::new(Shared { key: key.into(), folder: folder.into(), stop: AtomicBool::new(false), busy: AtomicUsize::new(0) });
    let listeners = addrs.iter().map(TcpListener::bind).collect::<io::Result<Vec<_>>>()?;
    let mut bound = Vec::new();
    for l in &listeners {
        // Polled, so Drop can stop the loop without a connection to wake it.
        l.set_nonblocking(true)?;
        bound.push(l.local_addr()?);
    }
    let threads = listeners
        .into_iter()
        .map(|l| {
            let s = shared.clone();
            std::thread::spawn(move || accept_loop(l, s))
        })
        .collect();
    Ok(Server { addrs: bound, shared, threads })
}

fn accept_loop(listener: TcpListener, s: Arc<Shared>) {
    while !s.stop.load(Relaxed) {
        let Ok((stream, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let slot = Slot(s.clone());
        if s.busy.fetch_add(1, Relaxed) >= MAX_CONNECTIONS {
            continue;
        }
        std::thread::spawn(move || {
            let _ = serve(stream, &slot.0);
        });
    }
}

/// One taken connection slot, given back however serving ends (a panic too).
struct Slot(Arc<Shared>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.busy.fetch_sub(1, Relaxed);
    }
}

/// Reads that fail once `until` has passed, so the whole request has to arrive in time, not just
/// each piece of it (a client sending a byte every few seconds can't hold a slot).
struct Deadline<R> {
    inner: R,
    until: Instant,
}

impl<R: Read> Read for Deadline<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if Instant::now() > self.until {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.inner.read(buf)
    }
}

/// Cuts off a download that takes less than 64 KiB in 30 s (a phone that stopped reading, or a
/// client holding a slot on purpose). A paused player asks again with a Range when it resumes.
struct Paced<W> {
    inner: W,
    since: Instant,
    sent: u64,
}

impl<W: Write> Write for Paced<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.since.elapsed() >= Duration::from_secs(30) {
            if self.sent < 64 * 1024 {
                return Err(io::ErrorKind::TimedOut.into());
            }
            (self.since, self.sent) = (Instant::now(), 0);
        }
        let n = self.inner.write(buf)?;
        self.sent += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Reads one request and answers it, then closes the connection.
fn serve(stream: TcpStream, s: &Shared) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    // Each read or write waits at most this long; Deadline and Paced bound the whole.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
    let header = Deadline { inner: stream.try_clone()?.take(16 * 1024), until: Instant::now() + Duration::from_secs(10) };
    let mut reader = BufReader::new(header);
    let (mut request, mut range) = (String::new(), None);
    reader.read_line(&mut request)?;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("range")
        {
            range = Some(value.trim().to_string());
        }
    }
    let mut words = request.split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    respond(&mut Paced { inner: &stream, since: Instant::now(), sent: 0 }, s, method, target, range.as_deref())
}

fn respond(out: &mut impl Write, s: &Shared, method: &str, target: &str, range: Option<&str>) -> io::Result<()> {
    let path = target.split('?').next().unwrap_or("");
    let mut parts = path.strip_prefix('/').unwrap_or("").splitn(2, '/');
    if !same(parts.next().unwrap_or(""), &s.key) {
        return text(out, "404 Not Found");
    }
    if method != "GET" {
        return text(out, "405 Method Not Allowed");
    }
    let list = clips::list(&s.folder, usize::MAX);
    match parts.next() {
        None | Some("") => {
            let body = page(&list, &s.key);
            write!(out, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}", body.len())
        }
        Some(id) => match list.iter().find(|c| clip_id(c) == id) {
            Some(c) => send_file(out, c, range, &s.stop),
            None => text(out, "404 Not Found"),
        },
    }
}

/// A short plain-text answer, like "404 Not Found".
fn text(out: &mut impl Write, status: &str) -> io::Result<()> {
    write!(out, "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{status}", status.len())
}

/// Key check that takes the same time however much of the key is right.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A clip's id in links: the start of a hash of its path.
fn clip_id(c: &clips::Clip) -> String {
    Sha256::digest(c.path.to_string_lossy().as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The file, or the part of it asked for with a Range header (which phones need to seek).
fn send_file(out: &mut impl Write, c: &clips::Clip, range: Option<&str>, stop: &AtomicBool) -> io::Result<()> {
    let mut f = File::open(&c.path)?;
    let len = f.metadata()?.len();
    let kind = match c.path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("mkv") => "video/x-matroska",
        Some("mov") => "video/quicktime",
        _ => "video/mp4",
    };
    let (start, end) = match range.map(|r| parse_range(r, len)) {
        None => {
            write!(out, "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n")?;
            (0, len)
        }
        Some(Some((a, b))) => {
            write!(out, "HTTP/1.1 206 Partial Content\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nContent-Range: bytes {a}-{}/{len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n", b - a, b - 1)?;
            (a, b)
        }
        Some(None) => return write!(out, "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    };
    f.seek(SeekFrom::Start(start))?;
    let mut f = f.take(end - start);
    let mut buf = vec![0; 64 * 1024];
    while !stop.load(Relaxed) {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
    }
    Ok(())
}

/// "bytes=100-199", "bytes=100-" or "bytes=-500" as a [start, end) span inside `len`; None when
/// it's outside the file or not understood (one span only).
fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let (a, b) = header.strip_prefix("bytes=")?.trim().split_once('-')?;
    let (start, end) = if a.is_empty() {
        (len.saturating_sub(b.parse().ok()?), len)
    } else {
        let end = if b.is_empty() { len } else { b.parse::<u64>().ok()?.saturating_add(1).min(len) };
        (a.parse().ok()?, end)
    };
    (start < end).then_some((start, end))
}

/// The clips newest first, grouped by game folder (the group with the newest clip first).
fn page(list: &[clips::Clip], key: &str) -> String {
    let mut groups: Vec<(Option<&str>, Vec<&clips::Clip>)> = Vec::new();
    for c in list {
        match groups.iter_mut().find(|(g, _)| *g == c.group.as_deref()) {
            Some((_, v)) => v.push(c),
            None => groups.push((c.group.as_deref(), vec![c])),
        }
    }
    let mut html = String::from(
        "<!doctype html><html><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>Replay Vault</title>\
         <style>body{margin:0;padding:12px;background:#16161a;color:#eee;font:15px system-ui,sans-serif}h2{font-size:17px;color:#b288ff;margin:20px 0 8px}\
         .c{background:#1c1c22;border-radius:10px;padding:8px;margin-bottom:12px}video,img{width:100%;border-radius:6px;background:#000}\
         p{margin:6px 2px;word-break:break-all}a{color:#b288ff}small{color:#8e8e9a}</style></head><body><h1 style=\"font-size:20px\">Replay Vault</h1>",
    );
    if list.is_empty() {
        html.push_str("<p>No clips yet.</p>");
    }
    for (group, clips) in groups {
        html.push_str(&format!("<h2>{}</h2>", esc(group.unwrap_or("Clips"))));
        for c in clips {
            let (src, name) = (format!("/{key}/{}", clip_id(c)), esc(&c.name));
            let media = if c.is_picture { format!("<img loading=lazy src=\"{src}\">") } else { format!("<video controls preload=none playsinline src=\"{src}\"></video>") };
            html.push_str(&format!("<div class=c>{media}<p>{name}<br><small>{}</small> <a href=\"{src}\" download=\"{name}\">Download</a></p></div>", clips::size(c.bytes)));
        }
    }
    html + "</body></html>"
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// Tailscale gives each device an address in 100.64.0.0/10.
pub fn is_tailscale(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v) if v.octets()[0] == 100 && v.octets()[1] & 0xC0 == 64)
}

/// This PC's address on the home network, and its Tailscale address if it has one. Found by
/// asking Windows which address it would send from; nothing is sent. Tests only get 127.0.0.1,
/// so they never open the firewall prompt.
pub fn local_ips() -> Vec<IpAddr> {
    if cfg!(test) {
        return vec![Ipv4Addr::LOCALHOST.into()];
    }
    let from = |to: [u8; 4]| {
        let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        s.connect((Ipv4Addr::from(to), 80)).ok()?;
        s.local_addr().ok().map(|a| a.ip())
    };
    let mut ips: Vec<IpAddr> = from([8, 8, 8, 8]).into_iter().collect();
    if let Some(ts) = from([100, 100, 100, 100]).filter(|ip| is_tailscale(*ip) && !ips.contains(ip)) {
        ips.push(ts);
    }
    ips
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn get(addr: SocketAddr, path: &str, extra: &str) -> Vec<u8> {
        let mut s = TcpStream::connect(addr).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n{extra}\r\n").unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        out
    }

    fn head(resp: &[u8]) -> String {
        String::from_utf8_lossy(&resp[..resp.windows(4).position(|w| w == b"\r\n\r\n").unwrap()]).into_owned()
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 10)));
        assert_eq!(parse_range("bytes=90-", 100), Some((90, 100)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 100)));
        assert_eq!(parse_range("bytes=50-500", 100), Some((50, 100)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=9-3", 100), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
        assert!(is_tailscale("100.101.5.9".parse().unwrap()));
        assert!(!is_tailscale("100.1.5.9".parse().unwrap()) && !is_tailscale("192.168.1.2".parse().unwrap()));
    }

    /// A request that's still arriving after its deadline, or a download taken too slowly, is cut off.
    #[test]
    fn slow_clients_are_cut_off() {
        let late = Instant::now() - Duration::from_secs(1);
        let mut r = Deadline { inner: &b"GET / HTTP/1.1\r\n"[..], until: late };
        assert_eq!(r.read(&mut [0; 8]).unwrap_err().kind(), io::ErrorKind::TimedOut);
        let mut r = Deadline { inner: &b"GET"[..], until: Instant::now() + Duration::from_secs(5) };
        assert_eq!(r.read(&mut [0; 8]).unwrap(), 3);

        let long_ago = Instant::now() - Duration::from_secs(31);
        let mut w = Paced { inner: Vec::new(), since: long_ago, sent: 1000 };
        assert_eq!(w.write(b"x").unwrap_err().kind(), io::ErrorKind::TimedOut);
        let mut w = Paced { inner: Vec::new(), since: long_ago, sent: 1 << 20 };
        assert_eq!(w.write(b"x").unwrap(), 1);
        assert!(w.since.elapsed() < Duration::from_secs(1) && w.sent == 1, "a new 30 s window started");
    }

    /// Every connection gives its slot back, however serving ended.
    #[test]
    fn slots_come_back() {
        let s = Arc::new(Shared { key: KEY.into(), folder: String::new(), stop: AtomicBool::new(false), busy: AtomicUsize::new(1) });
        let slot = Slot(s.clone());
        let _ = std::thread::spawn(move || {
            let _slot = slot;
            panic!("serving failed");
        })
        .join();
        assert_eq!(s.busy.load(Relaxed), 0);
    }

    #[test]
    fn serves_only_listed_clips_to_the_key() {
        let dir = std::env::temp_dir().join("replay-vault-phone-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("VALORANT")).unwrap();
        let bytes: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        std::fs::write(dir.join("VALORANT").join("Ace & win.mp4"), &bytes).unwrap();
        std::fs::write(dir.join("Shot.png"), b"png").unwrap();
        std::fs::write(dir.join("secret.txt"), b"secret").unwrap();
        let server = start(&["127.0.0.1:0".parse().unwrap()], KEY, &dir.display().to_string()).unwrap();
        let addr = server.addrs[0];

        // Wrong or missing key.
        for path in ["/", "/nope/", &format!("/{}/", &KEY[1..]), "/favicon.ico"] {
            assert!(head(&get(addr, path, "")).starts_with("HTTP/1.1 404"), "{path}");
        }

        // The page lists both clips (names escaped) in their groups, and never the text file.
        let page = String::from_utf8(get(addr, &format!("/{KEY}/"), "")).unwrap();
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("text/html"));
        assert!(page.contains("Ace &amp; win.mp4") && page.contains("Shot.png") && page.contains("<h2>VALORANT</h2>"));
        assert!(!page.contains("secret"));
        let video = page.split("<video").nth(1).unwrap().split("src=\"").nth(1).unwrap().split('"').next().unwrap().to_string();

        // Whole file, then a part of it.
        let whole = get(addr, &video, "");
        assert!(head(&whole).contains("Content-Type: video/mp4") && whole.ends_with(&bytes));
        let part = get(addr, &video, "Range: bytes=1000-1099\r\n");
        let h = head(&part);
        assert!(h.starts_with("HTTP/1.1 206") && h.contains("Content-Range: bytes 1000-1099/5000") && h.contains("Content-Length: 100"), "{h}");
        assert!(part.ends_with(&bytes[1000..1100]) && part.len() == h.len() + 4 + 100);
        assert!(head(&get(addr, &video, "Range: bytes=6000-\r\n")).starts_with("HTTP/1.1 416"));

        // Paths instead of ids, and other methods.
        let txt = dir.join("secret.txt").display().to_string();
        for path in [format!("/{KEY}/../secret.txt"), format!("/{KEY}/secret.txt"), format!("/{KEY}/{txt}"), format!("/{KEY}/..%2Fsecret.txt"), format!("/{KEY}/VALORANT/Ace%20%26%20win.mp4")] {
            assert!(head(&get(addr, &path, "")).starts_with("HTTP/1.1 404"), "{path}");
        }
        let mut s = TcpStream::connect(addr).unwrap();
        write!(s, "POST {video} HTTP/1.1\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 405"));

        // Off means the port is closed.
        drop(server);
        assert!(TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
