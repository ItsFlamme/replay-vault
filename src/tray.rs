//! The tray icon: its menu, its three looks and what each item does.

use crate::*;

/// Instant replay is on, for the tray menu's Pause / Resume (its handler can't reach the app).
pub(crate) static REPLAY_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The tray menu changed Start with Windows; the app's checkbox re-reads it.
pub(crate) static STARTUP_TOGGLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The tray menu items that follow the app's state, and what they last showed (status, save
/// text, connected, replay on, start with Windows). Kept in the app: menu items aren't Send.
pub(crate) struct TrayMenu {
    pub(crate) status: MenuItem,
    pub(crate) save: MenuItem,
    pub(crate) pause: MenuItem,
    pub(crate) startup: CheckMenuItem,
    pub(crate) shown: Option<(String, String, bool, bool, bool)>,
}

/// Tray icon: left click toggles the overlay, right click opens the menu, a quick panel with the
/// recorder's state. The handlers run even while the window is hidden, so they act directly
/// instead of waiting for a frame.
pub(crate) fn make_tray(ctx: &egui::Context, cmds: Sender<Cmd>) -> anyhow::Result<(TrayIcon, TrayMenu)> {
    let status = MenuItem::new("Recorder not running", false, None);
    let save = MenuItem::new("Save clip", false, None);
    let pause = MenuItem::new("Pause replay", false, None);
    let open = MenuItem::new("Open Replay Vault", true, None);
    let folder = MenuItem::new("Open clips folder", true, None);
    let play_last = MenuItem::new("Play last clip", true, None);
    let copy_last = MenuItem::new("Copy last clip", true, None);
    let count_game = MenuItem::new("Count last window as a game", true, None);
    let startup = CheckMenuItem::new("Start with Windows", true, startup::is_enabled(), None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[&status, &PredefinedMenuItem::separator(), &save, &pause, &open, &folder, &play_last, &copy_last, &count_game, &startup, &PredefinedMenuItem::separator(), &quit])?;
    let (save_id, pause_id, open_id, folder_id, startup_id, quit_id) =
        (save.id().clone(), pause.id().clone(), open.id().clone(), folder.id().clone(), startup.id().clone(), quit.id().clone());

    let (play_id, copy_id, count_id) = (play_last.id().clone(), copy_last.id().clone(), count_game.id().clone());

    let c = ctx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if e.id == open_id {
            overlay::show();
            c.request_repaint();
        } else if e.id == save_id {
            let _ = cmds.send(Cmd::Save);
        } else if e.id == pause_id {
            let _ = cmds.send(Cmd::SetReplay(!REPLAY_ON.load(std::sync::atomic::Ordering::Relaxed)));
        } else if e.id == folder_id {
            clips::open(std::path::Path::new(&Config::load().folder), false);
        } else if e.id == play_id || e.id == copy_id {
            match last_clip() {
                Some(p) if e.id == play_id => clips::open(&p, false),
                Some(p) => {
                    if let Err(e) = clips::copy_file(&p) {
                        log::write(&format!("Couldn't copy the last clip: {e:#}"));
                    }
                }
                None => log::write("No clip to play or copy yet."),
            }
        } else if e.id == count_id {
            // For auto-pause: a game in a window that isn't full screen.
            match autopause::count_last_window(std::path::Path::new(&Config::load().folder)) {
                Ok(name) => {
                    log::write(&format!("{name} now counts as a game."));
                    toast::show("Counts as a game now", format!("{name}: replay stays on while it's in front"));
                }
                Err(e) => toast::show("Couldn't count it as a game", format!("{e:#}")),
            }
        } else if e.id == startup_id {
            if let Err(e) = startup::set_enabled(!startup::is_enabled()) {
                log::write(&format!("Couldn't change the startup setting: {e}"));
            }
            STARTUP_TOGGLED.store(true, std::sync::atomic::Ordering::Relaxed);
            c.request_repaint();
        } else if e.id == quit_id {
            shutdown();
            std::process::exit(0);
        }
    }));
    let c = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            overlay::toggle();
            c.request_repaint();
        }
    }));

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(TrayLook::Off.tooltip())
        .with_icon(tray_icon_image(TrayLook::Off)?)
        .build()?;
    Ok((tray, TrayMenu { status, save, pause, startup, shown: None }))
}

/// The newest video in the clips folder (screenshots and GIFs aside), for Play and Copy last clip.
fn last_clip() -> Option<std::path::PathBuf> {
    clips::list(&Config::load().folder, usize::MAX).into_iter().find(|c| !c.is_picture).map(|c| c.path)
}

/// What the tray icon shows: replay off (or no recorder), replay on, or a full recording running.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum TrayLook {
    Off,
    Buffer,
    Recording,
}

impl TrayLook {
    pub(crate) fn of(connected: bool, buffer: bool, recording: bool) -> TrayLook {
        match (connected, buffer, recording) {
            (true, _, true) => TrayLook::Recording,
            (true, true, false) => TrayLook::Buffer,
            _ => TrayLook::Off,
        }
    }

    /// The app's title (so a dev build says so) and what it's doing.
    pub(crate) fn tooltip(self) -> String {
        let doing = match self {
            TrayLook::Off => "instant replay off",
            TrayLook::Buffer => "instant replay on",
            TrayLook::Recording => "recording",
        };
        format!("{}: {doing}", crate::APP_TITLE)
    }
}

pub(crate) const TRAY_N: u32 = 32;

pub(crate) fn tray_icon_image(look: TrayLook) -> anyhow::Result<tray_icon::Icon> {
    Ok(tray_icon::Icon::from_rgba(tray_rgba(look), TRAY_N, TRAY_N)?)
}

/// A record dot with a dark ring, drawn in code: the accent colour while replay is on, gray when
/// off, red while recording.
pub(crate) fn tray_rgba(look: TrayLook) -> Vec<u8> {
    const N: u32 = TRAY_N;
    // The light accent whatever the mode: the taskbar is dark or light on its own.
    let on = accent_of().1;
    let dot = match look {
        TrayLook::Off => [120, 120, 128, 255],
        TrayLook::Buffer => [on.r(), on.g(), on.b(), 255],
        TrayLook::Recording => [235, 60, 60, 255],
    };
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f32 - 15.5).powi(2) + (y as f32 - 15.5).powi(2)).sqrt();
            let px = if d < 9.5 {
                dot
            } else if d < 14.5 {
                [30, 30, 34, 255]
            } else if d < 15.5 {
                [30, 30, 34, ((15.5 - d) * 255.0) as u8]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&px);
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    /// On this PC, read only: the clip Play/Copy last clip would use, and whether a game is in
    /// front right now. Run by hand with `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_last_clip_and_game() {
        let folder = crate::config::Config::load().folder;
        println!("clips folder: {folder}");
        println!("last clip: {:?}", super::last_clip());
        println!("game in front: {}", crate::autopause::game_in_front(std::path::Path::new(&folder)));
    }
}
