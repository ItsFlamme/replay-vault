//! Support bundle: one zip with the logs, the settings (secrets taken out) and a short system
//! summary, to send along when asking for help.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::{self, Config};

/// Zips `files` (name in the zip, file; missing ones are skipped), the settings without their
/// secrets and `summary` into `out`. Every file's text goes through feedback::scrub, as a log
/// sent with feedback does: the secrets, share links, the Windows user folder (as ~) and user
/// name, URL query strings, email addresses and anything that looks like a key come out.
pub fn write(out: &Path, files: &[(&str, PathBuf)], cfg: &Config, summary: &str) -> anyhow::Result<()> {
    let secrets: Vec<&str> = [&cfg.recorder_password, &cfg.discord_webhook].into_iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let home = dirs::home_dir().map(|h| h.display().to_string());
    let user = std::env::var("USERNAME").ok();
    let mut zip = zip::ZipWriter::new(std::fs::File::create(out)?);
    let mut add = |name: &str, text: String| -> anyhow::Result<()> {
        let text = crate::feedback::scrub(&text, home.as_deref(), user.as_deref(), &secrets);
        zip.start_file(name, zip::write::SimpleFileOptions::default())?;
        zip.write_all(text.as_bytes())?;
        Ok(())
    };
    for (name, path) in files {
        if let Ok(bytes) = std::fs::read(path) {
            add(name, String::from_utf8_lossy(&bytes).into_owned())?;
        }
    }
    add("config.json", serde_json::to_string_pretty(&config::without_secrets(cfg))?)?;
    add("system.txt", summary.to_string())?;
    zip.finish()?;
    Ok(())
}

/// The text with every kappa.lol share link (older logs have them) replaced.
pub fn without_links(text: String) -> String {
    const HOST: &str = "https://kappa.lol/";
    if !text.contains(HOST) {
        return text;
    }
    let (mut out, mut rest) = (String::new(), text.as_str());
    while let Some(i) = rest.find(HOST) {
        out.push_str(&rest[..i]);
        out.push_str("(link removed)");
        let after = &rest[i..];
        rest = &after[after.find(char::is_whitespace).unwrap_or(after.len())..];
    }
    out + rest
}

/// Windows version, the app's version, GPU, RAM and monitors, one per line.
pub fn summary(hw: &crate::profile::Hardware, monitors: &[crate::display::Monitor]) -> String {
    let mut lines = vec![
        format!("Windows: {}", windows_version()),
        format!("Replay Vault: {}", env!("CARGO_PKG_VERSION")),
        format!("GPU: {} ({} MB)", hw.gpu_name, hw.vram_mb),
        format!("RAM: {} MB", hw.ram_mb),
    ];
    lines.extend(monitors.iter().map(|m| format!("Monitor: {}", m.label())));
    lines.join("\r\n")
}

/// Like "Windows 10 Pro 24H2 (build 26100)".
pub fn windows_version() -> String {
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    use windows::core::w;
    let key = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let read = |value| crate::startup::read_string(HKEY_LOCAL_MACHINE, key, value).unwrap_or_default();
    format!("{} {} (build {})", read(w!("ProductName")), read(w!("DisplayVersion")), read(w!("CurrentBuild")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// The zip (built in a temp folder from made-up files) has the logs and settings, and no
    /// secret anywhere: not in its bytes, not in any file in it.
    #[test]
    fn bundle_has_no_secrets() {
        let dir = std::env::temp_dir().join("replay-vault-support-test");
        let _ = std::fs::create_dir_all(&dir);
        let cfg = Config {
            recorder_password: "pw-0123456789abcdef".into(),
            discord_webhook: "https://discord.com/api/webhooks/1/hook-token-secret".into(),
            buffer_secs: 77,
            ..Config::default()
        };
        let log = dir.join("app.log");
        let home = dirs::home_dir().unwrap().display().to_string();
        std::fs::write(
            &log,
            format!(
                "started\r\nUploaded {home}\\Videos\\a.mp4 to https://kappa.lol/AbC12 ok\r\nfrom {}/Videos\r\nin {}\\x\r\nmail a.b@example.com, phone http://192.168.1.5:8080/clips?key=abc ok\r\n",
                home.replace('\\', "/"),
                home.to_lowercase()
            ),
        )
        .unwrap();
        let obs = dir.join("obs.txt");
        std::fs::write(&obs, format!("websocket password {}\nposting to {}\n", cfg.recorder_password, cfg.discord_webhook)).unwrap();
        let out = dir.join("support.zip");
        let files = [("app.log", log), ("obs.log", obs), ("tweaks.json", dir.join("missing.json"))];
        write(&out, &files, &cfg, "Windows: test").unwrap();

        let secrets = [cfg.recorder_password.as_str(), cfg.discord_webhook.as_str(), "hook-token-secret"];
        let raw = std::fs::read(&out).unwrap();
        for s in secrets {
            assert!(!raw.windows(s.len()).any(|w| w == s.as_bytes()), "the zip has a secret in it");
        }
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&raw)).unwrap();
        let mut names = Vec::new();
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).unwrap();
            let name = f.name().unwrap().to_string();
            let mut text = String::new();
            f.read_to_string(&mut text).unwrap();
            // The message names the file, never the secret (a failed test shouldn't print it).
            for (i, s) in secrets.iter().enumerate() {
                assert!(!text.contains(s), "{name} has secret #{i}");
            }
            if name == "app.log" {
                assert!(text.contains("Uploaded ~\\Videos\\a.mp4 to (link removed) ok") && text.contains("from ~/Videos"), "{text}");
                assert!(!text.contains(&home) && !text.contains("kappa.lol/AbC12"), "{text}");
                // The home folder in any case, emails and query strings go too, as in feedback.
                assert!(text.contains("in ~\\x") && text.contains("mail <email>, phone http://192.168.1.5:8080/clips?<removed> ok"), "{text}");
            }
            if name == "config.json" {
                assert!(text.contains("\"buffer_secs\": 77"));
            }
            names.push(name);
        }
        assert_eq!(names, ["app.log", "obs.log", "config.json", "system.txt"]);
        // The real summary reads this PC's Windows version.
        let text = summary(&crate::profile::detect(), &[]);
        assert!(text.starts_with("Windows: ") && !text.contains("(build )"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
