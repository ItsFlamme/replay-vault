# Replay Vault plan

## To do

- [x] **Basic / Advanced view for Video and Audio.** A switch at the top of each page. Basic shows only
      what most players need, in plain words (e.g. Video: a quality level and clip length; Audio: which
      speakers and which mic, and "separate my mic" on/off), with the right settings picked behind the
      scenes. Advanced shows today's full pages (resolution, frame rate, encoder, effort, the per-track
      device grid, noise suppression). Aimed at players who aren't tech or audio savvy.

## Ideas (not decided)

### Clips
- [x] **Favorites.** Star a clip; starred clips sort first and are never auto-cleaned.
- [x] **Auto-cleanup.** Keep the clips folder under a size you pick (or drop clips older than N days),
      oldest first, to the Recycle Bin, skipping favorites.
- [x] **Rename** a clip from its card, and a **search** box on the Clips page.
- [x] **Copy clip** button: puts the file on the clipboard, ready to paste into Discord.
- [x] **Share-size export.** "Fit for Discord" (10 / 50 MB), 720p, or a vertical 9:16 crop for
      TikTok / Shorts, re-encoded on the GPU. Whole clip, track 1's sound; 10 MB fits about 80 s
      (longer asks for a trim). Scaling runs on the CPU; quiet footage comes out under the size.
- [x] **Hover scrubbing** on clip thumbnails (move the mouse across to preview). 16 steps per card;
      shows the keyframe before each spot (up to 2 s early) since exact frames take over a second.
- [x] **Montage maker**: pick clips, order them, simple fades and optional music, export one video.
      Montage button on the Clips page: click cards (or arrow keys + Space) to add them, reorder in
      the panel, fades on/off, an MP3/M4A/WAV looped under the sound at a chosen volume. Whole clips,
      track 1's sound, re-encoded at the first clip's size and frame rate (other sizes are stretched,
      not letterboxed). Saved as "Montage <date time>.mp4" in the clips folder.
- [x] **GIF export** of a short trimmed part. "Save as GIF" in the trim tools, up to 15 s, 480 px wide
      at 15 fps, looping, no sound. Listed with the screenshots (opens in the default app).
- [x] **Tags / collections** ("Aces", "Funny") beyond the per-day groups. Tags menu on the card,
      chips on the Clips page filter by tag, search matches tags. Kept by path in tags.json, so a
      clip moved or renamed outside the app loses its tags (auto-cleanup leaves stale entries).
- [x] **Post to Discord** in one click through a webhook you set up. Webhook on the General page
      (masked), "Post to Discord" in the card menu. Up to 10 MB (non-boosted servers); bigger
      clips are pointed to Export for sharing. Only tested against a local server, not real Discord.
- [x] **Watch clips from your phone** over the home network or Tailscale. General page toggle; a
      read-only page on port 4480 behind a random key (New key cuts old links). Range requests for
      seeking. Addresses are checked once a minute, so a new LAN IP or Tailscale coming up later is
      picked up on its own. Tested on 127.0.0.1 only, not from a real phone.
- [x] **Keyboard shortcuts** on the Clips page: Delete, F2 rename, Enter play. Click a card or use the
      arrow keys to pick a clip first. Not in the Games view (no clip cards there).

### Trimming
- [x] **Audio waveform** under the trim timeline, so a shout or a shot is easy to find.
- [x] **Frame stepping** (, and . keys) and J / K / L playback speed. (No reverse play: Windows'
      player can't play these clips backwards, so J is slow motion. Sound only at normal speed.)
- [x] **Several cuts in one clip**, and joining two clips together. Trim: Add part keeps the
      selection (green on the timeline), Save joins the parts in time order, re-encoded so each cut is
      exact. Joining two clips is a montage of two with fades off (tested: length adds up, first
      clip's size and frame rate).
- [x] **Loud moments**: marks on the timeline where the clip's audio spikes (read from the file only).
      Track 1 only, up to 10, well above the clip's median loudness. Click a mark or press M for the next.

### Recording
- [x] **Encoder warning.** If the health check sees skipped frames, suggest the next lighter
      Encoder effort (never change it on its own). Note on the Video page (Basic and Advanced) once
      1% or more (at least 30) frames are skipped in a recorder run; once per effort per session.
- [x] Performance card **history**: average and peak recorder load per session, in the session report.
      CPU, GPU and video encoder, sampled every 5 s; also in the hourly check line.
- [x] Popup when a clip is saved shows its **thumbnail** and length. Waits up to 0.4 s for the
      thumbnail (about 0.25 s in a debug test), then shows without it.
- [x] **Record only the apps you pick** (e.g. game + Discord, not Spotify), using OBS's application
      audio capture (a Windows feature; nothing is injected into the game). Audio page, Advanced:
      "App sound". Apps are picked from open windows and matched by exe name; the exe comes from
      Windows' process list snapshot, so no process is opened. An app minimized to the tray (no
      window) can't be picked. Needs Windows 10 2004+; not tried live with OBS yet.
- [x] **Follow the mouse** across monitors: record the monitor you're playing on. Video page,
      Monitor: "Follow the mouse" (Desktop mode only). The recorder checks the pointer every second
      and moves the capture once it has stayed on another monitor for 2 s. The clip size stays
      that of the chosen monitor; others are scaled to fit (OBS bounds). Not tried live with OBS yet.
- [x] **HDR handling** so clips from an HDR monitor don't look washed out. HDR is detected with
      DXGI (HDR10 color space); then OBS gets SDR output (NV12, Rec. 709, partial range) and its
      SDR white level set to Windows' own SDR brightness for that monitor, so its display capture
      tone-maps to match the screen. Video page says so. No HDR monitor here: not tried live.
- [x] **Tray icon shows the state**: purple buffer on, gray off, red while a full recording runs.
      The tooltip says it too. Changes only when the state does.

### Storage
- [x] **Archive drive**: move clips older than N days to another drive or a NAS on its own. (Every 5 minutes; the archive folder must be outside the clips folder; archived clips no longer show on the Clips page.)
- [x] **Cloud backup folder**: copy clips you pick (Back up to cloud, the Cloud tag) into a OneDrive / Google Drive folder; stars only mean keep. (Every 5 minutes; a backed-up clip that changed size is copied over its old copy.)
- [x] **Space estimate**: about how many GB an hour of saved clips takes, on the Video page. (From the planned bitrate; every quality setting is a set VBR bitrate, so no estimate from real clips was needed.)

### Setup and settings
- [x] **Export / import settings** (one file), for a new PC or a reinstall. General > Move to a new PC.
      Secrets (recorder password, phone key, webhook) stay out; stars and tags merge in on import.
- [x] **First-run tips**: short hints the first time each page is opened. One line per page until its x
      is clicked (config tips_seen, kept without Save).
- [x] Accent color choice. General > Behavior: purple, blue, green, orange, pink, red; previews before
      Save. The tray icon and the saved-clip popup follow it too.
- [x] **Support bundle**: one button that zips logs and settings (no passwords) for troubleshooting.
      Logs > Make support zip, onto the Desktop (overwrites the last one), shown in Explorer.
- [ ] **Translations**, if it goes public.

### Release
- [ ] Installer and code signing (SignPath Foundation once it's public on GitHub).
- [ ] App self-update (like the OBS updater it already has).
- [ ] Test on other PCs: AMD and Intel encoders, other anti-cheats, a clean Windows VM.
- [ ] Check the features never tested end to end: share links (kappa.lol upload/delete), Remove from
      this PC, sort clips by game, game capture mode, the firewall rule, the player on other GPUs.

## Later

- [ ] Linux and macOS versions, if needed. Most of the app is Windows-only today (Media Foundation,
      WASAPI, Win32 hotkeys and window, DXGI, registry); OBS itself runs on all three.
