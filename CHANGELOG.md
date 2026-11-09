# Changelog

Notable changes in each release. The app's What's new page is built from this file.

## 0.1.3

### New

- Updates install from inside the app on every copy. The portable version used to send you to GitHub; now Update now downloads the new version, checks it, swaps it in and reopens. Settings, notes, tags and clips stay; the old program files are deleted, not left in the Recycle Bin.
- A new version now comes to you: when one is out, the app opens from the tray with Update now and Update later, never over a game or a recording. Update later asks again the next day.
- Drag a clip's picture out of the app into Discord, a browser or a folder to share or copy it. Dragging one of several picked clips takes them all.
- The tray menu has Play last clip and Copy last clip (pastes into Discord or a folder as the file).
- Pause instant replay when no game is open (General, off by default): after 2 minutes with no game in front, replay stops, and it starts again when a game is in front. A game is anything full screen or a window you've saved clips from before; the game itself is never touched. Replay you turned off yourself stays off.
- The app opens where you left it: same size, same place and the same page. If that screen is gone, it opens in the middle as before.

### Fixed

- Updates clean up after themselves for good: an old copy of OBS that was still in use during an update is deleted at the next start, and an app update clears the old debug.log. Nothing from an update goes to the Recycle Bin.

## 0.1.2

### Changed

- Keybinds now finds keys another program or Windows already uses, on its own, and says who (NVIDIA's and AMD's shortcuts by name) and what happens when you press it. You can still pick a key that clashes.
- Picking a key that another action has moves it to the new one instead of refusing. The save key's key stays put.

### Fixed

- Pressing Esc while setting a key cancels it without hiding the app.
- Remove from this PC also deletes the small debug.log that could keep the app's folder from going away, and a portable copy deletes its README too.

## 0.1.1

### Changed

- Setup has one Browse instead of two: picking where the recorder goes puts it in Replay Vault OBS and your clips beside it in Replay Vault Clips. The clips folder can still be moved under General.
- Remove from this PC now uninstalls the app too, with its shortcuts, and clears everything it kept (private OBS, a waiting update, settings). Only your clips stay.
- Custom sound: track 1 isn't locked any more, so a device whose sound is already in another (like a GoXLR's stream mix) can stay off it and nothing is heard twice. Custom no longer has a separate Discord voice chat row, since Discord is already in the game and PC sound; "Game, mic and chat separate" still gives it a track of its own.

### New

- The app has its own icon, the Replay Vault cassette, in the taskbar, Start menu and on the desktop shortcut.

## 0.1.0

### New

- Instant replay from 10 seconds to 20 minutes, saved with one key, plus keys for the last 30 or 60 seconds.
- Full recording and screenshots on their own keys.
- Desktop or game capture. Desktop is the default and is safe with anti-cheat.
- Audio tracks set up like OBS: each sound on the tracks you pick, more than one microphone, a mic mute key and noise suppression.
- Clips page with a folder per game, Steam covers, search, stars and tags, hover to scrub and a built-in player.
- Trim with a waveform and a volume slider per track, so you can drop your mic or the game from a clip.
- Export for Discord, 720p or vertical 9:16, GIFs and a montage maker.
- Post to Discord through your own webhook, or upload for a share link you can delete later.
- Auto-cleanup by size or age, and an archive drive for old clips.
- Send feedback from the app, and this What's new page.
- Installer and portable zip.
- Default keys stay clear of NVIDIA's and AMD's own shortcuts, and if one of them holds a key you pick, Keybinds says which shortcut uses it.
- Tells you when a new version is out and installs it with one click (Install now), after checking the download against its published checksum. You can turn the daily check off under General, or check any time with Check for updates.

Recording is tested on NVIDIA. AMD, Intel and CPU recording are untested.
