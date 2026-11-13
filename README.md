<p align="center">
  <img src="docs/banner.gif" alt="Replay Vault: press a key, keep the play" width="100%">
</p>

<p align="center">
  <b>Free, open-source instant replay for Windows.</b><br>
  It keeps the last few minutes of your game in memory. Press <kbd>Alt</kbd> + <kbd>F10</kbd> and they're saved as a clip.
</p>

<p align="center">
  <a href="../../releases/latest"><img src="https://img.shields.io/badge/Download-for%20Windows-8448f0?style=for-the-badge" alt="Download for Windows"></a>
  <a href="https://replayvault.app"><img src="https://img.shields.io/badge/replayvault.app-211a2d?style=for-the-badge" alt="Website"></a>
  <a href="https://discord.gg/n2KYWUPqGY"><img src="https://img.shields.io/badge/Discord-211a2d?style=for-the-badge&logo=discord&logoColor=a678ff" alt="Discord"></a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/license-Apache%202.0-352a47" alt="License: Apache 2.0">
  <img src="https://img.shields.io/badge/Windows-10%20%7C%2011-352a47" alt="Windows 10 and 11">
</p>

<p align="center">
  <img src="docs/tour.gif" alt="A tour of Replay Vault: saving a clip, scrubbing, trimming, sharing, tags, audio tracks, setup and keys">
</p>

## How it works

| 1. It keeps rolling | 2. You press the key | 3. It's a clip |
|---|---|---|
| Your screen and sound are recorded into memory, round and round. Nothing is written to disk yet. | Right after the moment, from inside the game: <kbd>Alt</kbd> + <kbd>F10</kbd>, or your own key. | The last 3 minutes (or however long you set, up to 20) land in your clips folder, ready to watch, trim or share. |

Recording is done by a private, hidden copy of [OBS Studio](https://obsproject.com) that the app downloads and manages, so you get OBS's GPU encoding without setting up OBS. If you already use OBS, your own install and its settings are not changed.

## What's on the tapes

**Recording**
- Instant replay from 10 seconds to 20 minutes, 3 minutes to start with. It never takes more than a quarter of your RAM; a long replay at high quality keeps its newest part.
- Full recording and screenshots on their own keys
- Desktop capture by default, which is safe with anti-cheat. Game capture is there if you want it.
- Game and mic on separate tracks, noise suppression, or record only the apps you pick
- Even sound on both sides (experimental, off to start with, on the Audio page): a clip that comes out louder on one side (your balance in Windows or another app) is evened after saving, still in stereo, with your mic kept in the middle
- Optional pause when no game is open, back on by itself when one is. A game in a window counts too: one as big as the screen, one you saved a clip from in the last hour, or one you pick from the tray.
- If instant replay stops by itself, you're told and it's turned back on. If it won't come back on, the recorder is restarted and it keeps trying. Every saved clip is checked, and you're told if it came out empty or without sound.
- A full recording stays playable if the app or the PC stops in the middle of it (with the OBS the app installs, 30.2 or later). One still running when Windows shuts down or the app is uninstalled is finished and saved first, as far as Windows gives it time.

**Clips and trim**
- A clips page grouped by day, with your games in the sidebar and as chips, a folder per game, search, stars and tags
- Hover over a clip to scrub through it, or play it in the built-in player
- Trim with an audio waveform and a volume per track
- Copies sized for Discord, 720p or vertical 9:16, GIFs and a montage maker

**Sharing**
- Drag a clip into Discord, a browser or a folder, or play and copy the last clip from the tray
- Post to your Discord channel through your own webhook
- Upload for a share link, and keep the link that deletes it later. Links follow their clips, and links of deleted clips are listed on General > Sharing.
- Clean up old clips by size or age, to the Recycle Bin only, and move old ones to an archive drive. Both stay off in a shared folder like Videos, Desktop or the top of a drive unless you confirm it on General > Storage.

## Requirements

- Windows 10 or 11, 64-bit
- A graphics card with a hardware video encoder is recommended. Only NVIDIA cards have been tested so far. AMD and Intel graphics, and CPU-only recording, are untested.

## Install

1. Download the latest installer (`ReplayVault-<version>-setup.exe`) or the portable zip from [Releases](../../releases). The installer does not need administrator rights.
2. Run it. A short setup downloads OBS Studio (about 225 MB), checks it against its published SHA-256 hash, and asks where it and your clips should go.
3. Setup then picks a quality that suits your PC, or uses the video and sound settings from your own OBS if you have one. Keys, sound and everything else can be changed later.

After that the app updates itself: when a new version is out it asks, and **Update now** downloads it, checks it and puts it in place. The download can be cancelled. Settings and clips stay. Only one copy of the app runs at a time.

Each release includes `SHA256SUMS.txt`. To verify a download, compare it with the output of `Get-FileHash <file>`.

> [!NOTE]
> Releases are not code-signed yet, so Windows SmartScreen may show a warning the first time you run the app.

## Default keys

| Action | Key |
|---|---|
| Save clip | <kbd>Alt</kbd> + <kbd>F10</kbd> |
| Show or hide the window | <kbd>Alt</kbd> + <kbd>X</kbd> |
| Start or stop a recording | <kbd>Alt</kbd> + <kbd>F11</kbd> |
| Screenshot | <kbd>Alt</kbd> + <kbd>F12</kbd> |
| Turn instant replay on or off | <kbd>Alt</kbd> + <kbd>Shift</kbd> + <kbd>F11</kbd> |

All keys can be changed on the Keybinds page, and a changed key takes effect when you Save. Save is <kbd>Alt</kbd> + <kbd>F10</kbd> on purpose, the same key as NVIDIA's instant replay, so ShadowPlay players keep the habit; if NVIDIA's own instant replay is also on, it saves a clip too (the Keybinds page can turn NVIDIA's overlay off while the app runs). The other keys avoid NVIDIA's and AMD's default shortcuts, which those apps keep even with their overlays off. A key NVIDIA's overlay holds still works for Replay Vault too, and both apps react to it. If another app already uses a key you pick, the Keybinds page says which one.

## Privacy

Replay Vault has no accounts, tracking or analytics. It connects to the internet only to:

- download and update OBS Studio and the Microsoft Visual C++ runtime it needs
- look up game cover art on Steam by the game's name (can be turned off)
- check GitHub once a day for a newer version of Replay Vault (can be turned off), and download it only when you click Update now
- send feedback, upload a share link or post to Discord, and only when you use those features

Clips, settings and logs stay on your PC: clips in `Videos\Replay Vault Clips` and the private OBS copy in `%LOCALAPPDATA%\replay-vault` by default (setup can put both on another drive), settings and logs in `%APPDATA%\replay-vault`.

## Uninstall

Use **General > Backup > Remove from this PC** in the app. It undoes the changes the app made (startup entry, firewall rule, GPU preference and any Windows settings you turned off), moves the private OBS copy and the app's settings to the Recycle Bin, then uninstalls the app and its shortcuts. Your clips are kept.

## Building from source

You need [Rust](https://rustup.rs) (stable) on Windows 10 or 11.

```
cargo build --release
cargo test
```

The executable is written to `target\release\replay-vault.exe`. Release packages are built by `tools\release.ps1`, which also needs [Inno Setup 6](https://jrsoftware.org/isinfo.php) for the installer. Pushing a `v*` tag builds a draft release with GitHub Actions.

## Code signing policy

Windows releases will be signed through the [SignPath Foundation](https://signpath.org) program, with a certificate issued to SignPath Foundation. Until then, releases are unsigned.

- Only builds made by this repository's GitHub Actions release workflow from a tagged commit are signed.
- Committer, reviewer and approver: [ItsFlamme](https://github.com/ItsFlamme). Every signing request is approved manually.

## Support

- Bugs and feature requests: [GitHub Issues](../../issues)
- Help and community: [Discord](https://discord.gg/n2KYWUPqGY)
- Security issues: see [SECURITY.md](SECURITY.md)

## License

Licensed under the [Apache License 2.0](LICENSE). Redistributions must keep the [NOTICE](NOTICE) file.

OBS Studio is licensed under GPL-2.0-or-later. It is downloaded from the official release and run unmodified, not bundled. Other third-party components are listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md). Replay Vault is not affiliated with the OBS Project, NVIDIA, AMD or Intel.
