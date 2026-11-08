# Replay Vault

**Free, open-source instant replay for Windows. Press a key, keep the last few minutes.**

![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue) ![Windows 10 / 11](https://img.shields.io/badge/Windows-10%20%7C%2011-0078D6) ![Rust](https://img.shields.io/badge/made%20with-Rust-B7410E)

[Install](#install) · [Features](#features) · [Hotkeys](#hotkeys) · [Where your files go](#where-your-files-go) · [Building from source](#building-from-source)

## About

Replay Vault keeps a rolling recording of your screen and sound in memory. Nothing touches the disk
until you press the save hotkey, and then the last minute (or up to 20) is saved as a clip.

It runs a private, hidden copy of [OBS Studio](https://obsproject.com) as the recorder, so you get
OBS's GPU encoding and audio capture without ever opening OBS. If you already use OBS, your own copy
and its settings are never touched. The app itself lives in the tray, and opens over your game when
you want to watch, trim or share a clip.

## Features

**Recording**
- **Instant replay** from 10 seconds to 20 minutes, saved with one hotkey (or the last 30 / 60 seconds).
- **Full recording** and **screenshots** on their own hotkeys.
- **Desktop or game capture.** Desktop is the default and is safe with anti-cheat. Game capture is optional and comes with a warning.
- **Separate audio tracks** for game and mic, mic mute hotkey, noise suppression, or **only the apps you pick** (game and Discord, not Spotify).
- **Follow the mouse** across monitors, and HDR monitors recorded without a washed-out look.
- **Encoder hint** when the recorder starts skipping frames.

**Clips**
- **Clips page** with recent clips, per-game folders (with Steam cover art), search, stars and tags.
- **Hover to scrub** through a clip's thumbnail, and a built-in player.
- **Trim** with an audio waveform, loud-moment markers, frame stepping, and several parts joined into one clip.
- **Export for sharing:** fit for Discord (up to 10 or 50 MB), 720p, or vertical 9:16 for TikTok / Shorts.
- **Save as GIF**, and a **montage maker** with fades and optional music.

**Sharing and storage**
- **Post to Discord** through a webhook you set up, or upload for a **share link** (kappa.lol, with a delete link).
- **Auto-cleanup** by size or age (stars are never cleaned up), and an **archive** drive or NAS for old clips.
- **Move to a new PC:** export and import your settings in one file.

## Install

There's no installer yet.

1. Download `replay-vault.exe` from [Releases](../../releases), or build it yourself (see below).
2. Run it. A short setup picks your clips folder, hotkey, and screen and sound.
3. The first run downloads OBS Studio (about 225 MB) into the app's own folder. The download is checked against the published SHA-256.

> [!NOTE]
> The exe isn't code-signed yet, so Windows SmartScreen may warn about it the first time. Signing is planned.

## Hotkeys

| Action | Default |
|---|---|
| Save the replay | `Alt+F10` |
| Start or stop recording | `Alt+F9` |
| Screenshot | `Alt+F1` |
| Instant replay on or off | `Alt+Shift+F10` |
| Show or hide the window | `Alt+Z` |
| Save the last 30 s / 60 s, mic mute | Not set (pick them on the Keybinds page) |

The defaults are the same keys NVIDIA's ShadowPlay uses, so they feel familiar. NVIDIA's overlay would react to them too, so on NVIDIA PCs a switch on the Keybinds page turns its overlay off while Replay Vault runs, and turns it back on when you quit. If AMD Software uses the same keys, the Keybinds page tells you how to change them. Every key can be changed on the Keybinds page.

## Where your files go

| What | Where |
|---|---|
| Clips and screenshots | `Videos\Replay Vault` (you can change it), in a folder per game if you turn that on |
| Settings, stars, tags, logs | `%APPDATA%\replay-vault` |
| The private OBS copy | `%LOCALAPPDATA%\replay-vault` |

## Sharing clips

- **Export for sharing** makes a smaller copy next to the original. The original is never changed.
- **Share link** uploads one clip to kappa.lol only when you click and confirm. The app keeps the delete link, so you can take it down later. kappa.lol is a free third-party host; treat anything you upload there as public.
- **Post to Discord** sends the clip to a channel through a webhook you paste in. The webhook stays on your PC and is never logged.

## What it changes on your PC

- **Recorder:** OBS runs hidden, tied to the app, and is closed with it. Its WebSocket listens on port 4466, protected by a random password.
- **Firewall:** a rule for the private OBS, only if you allow it.
- **GPU preference:** the private OBS is set to run on the high-performance GPU (for laptops and PCs with two GPUs). On by default; untick it to put your setting back.
- **Game Bar:** turning off Xbox Game Bar capture is optional and off by default. Unticking restores your own setting.
- **Startup:** "Start with Windows" adds the app to your user's Run list.
- **Removing it:** **General > Remove from this PC** undoes all of the above, moves the private OBS to the Recycle Bin, and keeps your clips.

## Good to know

- **Anti-cheat:** Desktop capture works with every game. Game capture hooks into the game, so anti-cheats like Vanguard, FACEIT or ESEA may block or flag it. Use Desktop for those.
- **Protected video:** DRM-protected video (some streaming sites) records as black. That's Windows, not the app.
- **GPUs:** mostly tested on NVIDIA. AMD and Intel encoders should work through OBS, but they've had little testing.
- **Windows only.** OBS runs on Linux and macOS, but this app uses Windows APIs throughout.

## Building from source

You need [Rust](https://rustup.rs) (stable) on Windows 10 or 11.

```
cargo build --release
```

The exe is at `target\release\replay-vault.exe`. Run the tests with `cargo test`. The tests never play
sound and never go online; a few slow or online ones are marked `#[ignore]`.

## License

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). You can use, change and share the
code, but anything built on it has to keep the NOTICE file, which credits ItsFlamme.

OBS Studio is GPL-2.0-or-later and is downloaded and run unmodified, not bundled. See
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md). Not affiliated with the OBS Project or NVIDIA.
