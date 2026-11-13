//! Website demo loops: frame sequences of the real UI, written to
//! `SCREENS_OUT\demos\<name>\0000.png ...` for ffmpeg to turn into short videos. Reads the clips in
//! `SCREENS_FOLDER` and changes nothing (exports and Discord posts are only shown, never run).
//! Run on demand: `cargo test --bin replay-vault -- --ignored --exact tests::demos::demos` with both
//! variables set (see the vault's Website note for the clips and the ffmpeg lines).
use super::*;
use egui_kittest::kittest::Queryable;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

const SIZE: [f32; 2] = [1100.0, 700.0];

/// Default settings (so default keys and no personal devices), pointed at the demo clips.
fn demo_app(cc: &eframe::CreationContext, folder: &str) -> App {
    let mut app = App::preview(cc);
    let cfg = Config {
        folder: folder.into(),
        setup_done: true,
        discord_webhook: "https://discord.com/api/webhooks/123/made-up-token".into(),
        ..Config::default()
    };
    app.cfg = cfg.clone();
    app.saved = cfg;
    app.applied = recorder_settings(&app.cfg);
    app.tab = Tab::Clips;
    app
}

struct Frames {
    dir: PathBuf,
    n: usize,
}

impl Frames {
    fn new(out: &Path, name: &str) -> Self {
        let dir = out.join("demos").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Frames { dir, n: 0 }
    }

    /// Steps the UI a little (thumbnails and hover frames decode on other threads) and saves a frame.
    fn shot(&mut self, h: &mut egui_kittest::Harness<'_, App>) {
        for _ in 0..2 {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        h.render().unwrap().save(self.dir.join(format!("{:04}.png", self.n))).unwrap();
        self.n += 1;
    }

    fn hold(&mut self, h: &mut egui_kittest::Harness<'_, App>, frames: usize) {
        for _ in 0..frames {
            self.shot(h);
        }
    }
}

/// Lets thumbnails and clip lengths arrive before the first frame.
fn warm_up(h: &mut egui_kittest::Harness<'_, App>) {
    for _ in 0..60 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
#[ignore]
fn demos() {
    unsafe { std::env::set_var("WGPU_BACKEND", "dx12") };
    let out = PathBuf::from(std::env::var("SCREENS_OUT").expect("SCREENS_OUT"));
    let folder = std::env::var("SCREENS_FOLDER").expect("SCREENS_FOLDER");
    let newest = clips::list(&folder, 1).first().expect("a demo clip").path.clone();

    // Clips page: still for the hero, then hover-scrubbing across the first two cards.
    {
        let f = folder.clone();
        let mut h = egui_kittest::Harness::builder().with_size(SIZE).with_accessibility_check(false).wgpu().build_eframe(move |cc| demo_app(cc, &f));
        warm_up(&mut h);
        h.render().unwrap().save(out.join("home.png")).unwrap();
        let mut fr = Frames::new(&out, "scrub");
        fr.hold(&mut h, 8);
        for (from, to) in [(262.0, 500.0), (542.0, 780.0)] {
            for k in 0..=40 {
                h.hover_at(egui::pos2(from + (to - from) * k as f32 / 40.0, 240.0));
                fr.shot(&mut h);
            }
            fr.hold(&mut h, 6);
        }
        h.hover_at(egui::pos2(1080.0, 680.0));
        fr.hold(&mut h, 8);
    }

    // Sharing: the clip menu (Post to Discord, Export for sharing), then a Discord-size export
    // running and finishing. Nothing is encoded or sent.
    {
        let f = folder.clone();
        let mut h = egui_kittest::Harness::builder().with_size(SIZE).with_accessibility_check(false).wgpu().build_eframe(move |cc| demo_app(cc, &f));
        warm_up(&mut h);
        let mut fr = Frames::new(&out, "share");
        fr.hold(&mut h, 8);
        h.query_all_by_label(&icon::MORE.to_string()).nth(1).unwrap().click();
        fr.hold(&mut h, 4);
        h.get_by_label("Post to Discord").hover();
        fr.hold(&mut h, 18);
        h.get_by_label("Export for sharing").hover();
        fr.hold(&mut h, 22);
        h.key_press(egui::Key::Escape);
        fr.hold(&mut h, 3);
        let job = Arc::new(ExportJob::default());
        h.state_mut().exports.insert(newest.clone(), job.clone());
        for pct in (0..=100).step_by(4) {
            job.pct.store(pct, Ordering::Relaxed);
            fr.shot(&mut h);
        }
        let done = clips::copy_name(&newest, "Discord 10MB");
        let msg = format!("Saved {} (8.6 MB).", done.file_name().unwrap().to_string_lossy());
        let app = h.state_mut();
        app.exports.remove(&newest);
        app.clips_msg = Some(msg.clone());
        app.exported = Some((msg, done));
        fr.hold(&mut h, 36);
    }

    // Trim bar on the loudest demo clip's sound: a selection is dragged out, then played through.
    {
        let wave = clips::list(&folder, 10)
            .iter()
            .filter_map(|c| media::peaks(&c.path, || true).ok())
            .max_by(|a, b| a.iter().sum::<f32>().total_cmp(&b.iter().sum::<f32>()))
            .unwrap();
        let loud = media::loud_moments(&wave);
        let len = wave.len() as f64 / media::PEAKS_PER_SEC as f64;
        let state = Arc::new(std::sync::Mutex::new((2.0_f64, 2.5_f64, 2.0_f64)));
        let s = state.clone();
        let mut h = egui_kittest::Harness::builder().with_size([SIZE[0] - 260.0, 100.0]).wgpu().build_ui(move |ui| {
            apply_theme(ui.ctx());
            let (mut start, mut end, playhead) = *s.lock().unwrap();
            let mut view = (0.0, len);
            egui::Frame::new().fill(pal().card).inner_margin(12).show(ui, |ui| {
                range_bar(ui, len, &mut view, &mut start, &mut end, playhead, false, &wave, &loud, &[]);
            });
        });
        let dir = out.join("demos").join("trim");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut n = 0;
        let mut frame = |h: &mut egui_kittest::Harness<'_>| {
            h.step();
            h.render().unwrap().save(dir.join(format!("{n:04}.png"))).unwrap();
            n += 1;
        };
        for _ in 0..8 {
            frame(&mut h);
        }
        for k in 0..=30 {
            state.lock().unwrap().1 = 2.5 + (len * 0.55 - 2.5) * k as f64 / 30.0;
            frame(&mut h);
        }
        let end = state.lock().unwrap().1;
        for k in 0..=45 {
            state.lock().unwrap().2 = 2.0 + (end - 2.0) * k as f64 / 45.0;
            frame(&mut h);
        }
        for _ in 0..10 {
            frame(&mut h);
        }
    }

    // Setup, one picture per step (ffmpeg holds each for a moment).
    let dir = out.join("demos").join("setup");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for step in 0..STEPS.len() {
        let f = folder.clone();
        let mut h = egui_kittest::Harness::builder().with_size(SIZE).with_accessibility_check(false).wgpu().build_eframe(move |cc| {
            let mut app = demo_app(cc, &f);
            // The paths a new user would see, not this PC's.
            app.cfg.folder = r"C:\Users\You\Videos\Replay Vault".into();
            app.cfg.obs_dir = r"C:\Users\You\AppData\Local\replay-vault\obs-studio".into();
            app.saved = app.cfg.clone();
            obs_install::set_install_root(r"C:\Users\You\AppData\Local\replay-vault");
            // A typical gaming PC, not this one.
            (app.hw.gpu_name, app.hw.vram_mb, app.hw.ram_mb) = ("NVIDIA GeForce RTX 4070".into(), 12288, 32768);
            app.monitors.iter_mut().for_each(|m| {
                m.id = String::new();
                m.name = "Monitor".into();
                (m.width, m.height, m.hz) = (2560, 1440, 165);
            });
            app.wizard = Some(step);
            app
        });
        for _ in 0..20 {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        h.render().unwrap().save(dir.join(format!("{step:04}.png"))).unwrap();
    }
}
