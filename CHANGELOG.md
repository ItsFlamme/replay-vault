# Changelog

Notable changes in each release. The app's What's new page is built from this file.

## 0.2.1

### Changed

- Evening out sound that's louder on one side is now an experimental option on the Audio page, off to start with. Clips are kept exactly as recorded unless you turn it on.
- Esc no longer hides the window. It still closes menus and dialogs and lets go of picked clips. The X or the show/hide key hides it.
- Deleting a clip with Del or Move to Recycle Bin now asks first, the same as for several clips. The question says how many clips and how much space they take. Ctrl+click or Shift+click picks several, Ctrl+A picks all.

### Fixed

- After the recorder restarts (saving settings, for example), typing goes to the window you were in again, instead of to the hidden recorder until you clicked something.
- Clicking a clip's name or the line under it now picks the clip, so Del asks to delete it. Before, that click only selected the text.
- Del now works on a clip picked in Recent clips too, not only in All clips.
- A full recording is finished and saved properly when you quit, remove the app or change settings, instead of being cut off and unplayable.
- Encoder effort Balanced and Max FPS now really lighten the NVIDIA encoder on current OBS versions.
- Clip sound follows you when you switch or unplug headphones, instead of going silent until the clip is reopened.
- Trimming or exporting a .mkv or .mov clip works: the copy is saved as an .mp4.
- The Clip saved popup is the right size on screens set to 150% or 200% scaling.
- The status at the bottom of the sidebar no longer runs off the window when Version X is out or Key clash shows.
- Opening the app again while it's running brings up its window, even with File Explorer open on its folder.
- GIFs of clips under 15 fps play at the right speed.
- A trim or GIF that finishes after you leave the trim screen says it was saved and shows up in your clips.
- Renaming a clip to the name of one you deleted outside the app no longer gives it the old clip's star, tags or note.
- If the private OBS folder moves, the firewall rule follows it, so other devices stay blocked.
- The support zip hides your user name, email addresses and link details, the same way feedback does.
- Portable updates check the download again right before putting it in place.
- The window no longer stutters every few seconds with a big clips folder or one on a NAS: the folder is read in the background.
- Setting a key no longer catches Alt+Tab or a key pressed in another window: switching away cancels it, and your other keys work again at once.
- After Ctrl+clicking clips, Enter and F2 act on the one that's highlighted, and Del asks about the clips picked.
- A send that fails on the Feedback page doesn't count toward the 2 a day, and it always includes the latest log lines.
- With no internet, the Clips page no longer asks Steam for game covers over and over (once every 10 minutes).
- A clip that couldn't be moved to the Recycle Bin (open in another app) stays in your picks and the montage.
- Export for sharing works on clips with no sound.
- After saving a trim, the clip plays with its usual sound again instead of the trim's track volumes.
- A playing clip pauses whenever the window hides (the X, the tray icon), not only with the show/hide key.
- If you turned instant replay off (or it paused with no game open), it stays off when the recorder restarts or you save settings.
- A recorder that can't start (its drive unplugged) is retried every 20 seconds instead of every 2, and Remove from this PC no longer starts it again.
- Settings and share links are saved so a crash can't leave the file half written.
- Post to Discord uses the saved webhook, not one typed on General but not saved.
- When a clip's sound was louder on one side and your mic was on the same track, evening it out also pushed your voice to one side. With the option on, your voice now stays in the middle.
- Stars, tags and notes are saved so that a crash or a full disk can't leave the file half written, and yesterday's copy is kept to fall back on.
- The covers folder no longer fills with a small file for every window you had open. Those files are cleaned up when the app starts.

## 0.2.0

### New

- The tray menu has Count last window as a game: for a game in a window that isn't full screen, so Pause instant replay when no game is open keeps recording while you play it.
- After an update, the app opens on What's new once, so you can see what changed.
- A key another program has taken shows in the sidebar (Key clash), wherever you are in the app. Click it for Keybinds, which says who has it.
- The app reopens maximized if you left it maximized.
- Drag out in the player and while trimming: drag it into Discord, a browser or a folder, or click it to copy the clip.
- If instant replay stops by itself (the recorder closed or OBS turned it off), the app tells you and turns it back on, up to 3 tries, then says why if it can't. A clip that didn't save says so too.
- Every saved clip is checked: if the file is empty, can't be opened, is missing sound tracks or has no sound at all, a popup says so.
- A key NVIDIA's overlay holds can still be a Replay Vault key: both apps react to it.
- Keybinds has a Graphics card overlay switch: turn off NVIDIA's overlay (so its own Instant Replay doesn't save a second clip on Alt+F10), or AMD Software's Instant Replay and hotkeys (untested). NVIDIA's overlay comes back on whenever the app closes; Remove from this PC puts both back.
- General's settings are split into tabs: Clips, Storage, Appearance, Startup, Sharing and Backup.

### Changed

- Laptops and PCs with graphics built into the processor get lighter recommended settings: 30 fps instead of 60, since the game and the recorder share one small chip. Built-in AMD graphics are now recognized as built in (they used to count as a gaming card). Only setup's recommendation and the quality choices change; settings you already have stay.
- The replay never takes more than a quarter of the PC's RAM. A long replay at high quality that needs more keeps its newest part, and the Video page says how much of it fits. The replay length choices under General stop at the longest that fits on your PC at your video settings.
- The grey explanations under settings show only while Show tips and explanations is on (General, Appearance).
- Show in folder keeps the app open behind File Explorer instead of closing it.
- New installs record your Windows default microphone on track 1 along with game and PC sound. Settings you already have stay.
- Clips have even sound on both sides, whatever your own balance is. If a saved clip comes out louder on one side all the way through (a balance setting in Windows or another app), its quiet side is raised to match, so it plays centered for whoever you share it with. It stays stereo. Mics are recorded in mono, since a voice is one channel and some audio interfaces put it on the left only.
- Clip length choices are 1:00, 2:00, 3:00 and 5:00 (Custom still goes from 0:10 to 20:00). New installs start at 3:00. Your current length stays.
- The save key is called Save clip on Keybinds and in setup.
- Clips recorded at a shape other than 16:9 (ultrawide, 4:3, tall) play and trim in a player shaped like the clip, and their thumbnails keep their shape instead of being stretched.
- The recorder's copy of OBS takes about 80 MB less disk: its debug files, which OBS never loads, are left out.
- The popup for a new version waits until the PC has had a minute without keyboard or mouse input, so it can't open over a game in a window or while you're typing.
- Lighter on low-end PCs: checking and centering a saved clip's sound runs at background priority (one pass over the sound instead of up to three), and the start-up check for built-in graphics only runs where it's needed.

### Removed

- The Save the last 0:30 and Save the last 1:00 keys. Save clip and a shorter clip length do the same job.

### Fixed

- Starting with Windows could crash the app, or stop instant replay, when it opened within a minute of the PC turning on.
- A key pressed while you set a new key on Keybinds no longer also does what it's set to.
- Updated the app's libraries to their latest fixes.

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
