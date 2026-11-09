# Replay Vault

Free, open-source instant replay for Windows. It keeps the last few minutes of your game in memory, and one key press saves them as a clip.

![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue) ![Windows 10 and 11](https://img.shields.io/badge/Windows-10%20%7C%2011-0078D6)

![A tour of Replay Vault: the clips library, sharing, tags, montage and settings](docs/tour.gif)

## Overview

Replay Vault records your screen and sound into a rolling buffer. Nothing is written to disk until you press the save key. Recording is done by a private, hidden copy of [OBS Studio](https://obsproject.com) that the app downloads and manages, so you get OBS's GPU encoding without setting up OBS. If you already use OBS, your own install and its settings are not changed.

Main features:

- Instant replay from 10 seconds to 20 minutes, plus full recordings and screenshots
- Separate audio tracks for game and microphone, or only the apps you choose
- A clips library with per-game folders, search, stars and tags
- Trimming, GIF export, and smaller copies sized for Discord or vertical video
- Optional automatic cleanup by size or age, and archiving to another drive

## Requirements

- Windows 10 or 11, 64-bit
- A graphics card with a hardware video encoder is recommended. Only NVIDIA cards have been tested so far. AMD and Intel graphics, and CPU-only recording, are untested.

## Install

1. Download the latest installer (`ReplayVault-<version>-setup.exe`) or the portable zip from [Releases](../../releases). The installer does not need administrator rights.
2. Run it. A short setup asks for your clips folder, save key, screen and sound devices.
3. On first run the app downloads OBS Studio (about 225 MB) into its own folder and checks it against the published SHA-256 hash.

Each release includes `SHA256SUMS.txt`. To verify a download, compare it with the output of `Get-FileHash <file>`.

> [!NOTE]
> Releases are not code-signed yet, so Windows SmartScreen may show a warning the first time you run the app.

## Default keys

| Action | Key |
|---|---|
| Save the replay | `Alt+F10` |
| Show or hide the window | `Alt+Z` |
| Start or stop a recording | `Alt+F9` |
| Screenshot | `Alt+F1` |
| Turn instant replay on or off | `Alt+Shift+F10` |

All keys can be changed on the Keybinds page.

## Privacy

Replay Vault has no accounts, tracking or analytics. It connects to the internet only to:

- download and update OBS Studio and the Microsoft Visual C++ runtime it needs
- look up game cover art on Steam by the game's name (can be turned off)
- check GitHub once a day for a newer version of Replay Vault (can be turned off), and download it only when you click Install now
- send feedback, upload a share link or post to Discord, and only when you use those features

Clips, settings and logs stay on your PC: clips in `Videos\Replay Vault Clips` by default, settings and logs in `%APPDATA%\replay-vault`, and the private OBS copy in `%LOCALAPPDATA%\replay-vault`.

## Uninstall

Use **General > Remove from this PC** in the app first. It undoes the changes the app made (startup entry, firewall rule, GPU preference and any Windows or NVIDIA settings you turned off) and moves the private OBS copy to the Recycle Bin. Your clips are kept. Then uninstall it from Windows Settings, or delete the portable folder.

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
