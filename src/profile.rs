//! Recommended settings for this PC, worked out from its GPU, RAM and monitor.

use crate::config::{Config, DeviceSel};
use crate::display::Monitor;
use crate::recorder::{self, Gpu};

pub struct Hardware {
    pub gpu: Gpu,
    pub gpu_name: String,
    pub vram_mb: u64,
    pub ram_mb: u64,
    /// Built into the processor, sharing the PC's RAM (most laptops without a gaming GPU).
    pub integrated: bool,
}

pub fn detect() -> Hardware {
    let gpu = recorder::gpu();
    // The encoding GPU; with two of the same make, the one with more memory.
    let adapter = recorder::adapters().into_iter().filter(|a| a.vendor == gpu.vendor()).max_by_key(|a| a.vram_mb);
    // Little graphics memory of its own means built in. AMD's built-in graphics report the RAM set
    // aside for them, so for AMD it's asked of Direct3D 12 (a device made just for that, which
    // costs a moment at start, so only when needed).
    let integrated = adapter.as_ref().is_some_and(|a| {
        let small = a.vram_mb < 2048;
        if gpu == recorder::Gpu::Amd { recorder::shares_ram(a.index).unwrap_or(small) } else { small }
    });
    Hardware {
        gpu,
        gpu_name: adapter.as_ref().map_or("No supported GPU".into(), |a| a.name.clone()),
        vram_mb: adapter.map_or(0, |a| a.vram_mb),
        ram_mb: ram_mb(),
        integrated,
    }
}

pub(crate) fn ram_mb() -> u64 {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    static RAM: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *RAM.get_or_init(|| {
        let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut m) }.is_ok() { m.ullTotalPhys / (1024 * 1024) } else { 8 * 1024 }
    })
}

impl Hardware {
    pub fn summary(&self) -> String {
        let vram = if self.vram_mb >= 1024 { format!(" ({} GB)", (self.vram_mb + 512) / 1024) } else { String::new() };
        format!("{}{vram}, {} GB RAM", self.gpu_name, (self.ram_mb + 512) / 1024)
    }

    fn tier(&self) -> &'static str {
        match (self.gpu, self.vram_mb) {
            (Gpu::None, _) => "CPU only",
            _ if self.integrated => "Integrated GPU",
            (_, v) if v < 6144 => "Entry GPU",
            _ => "Gaming GPU",
        }
    }
}

/// The Video page's quality choices, lightest to best.
pub const PRESETS: [&str; 3] = ["Light", "Balanced", "High"];

/// Clip height cap, frame rate and quality (0 Highest .. 3 Low) for a quality choice (index into
/// `PRESETS`), scaled to what this PC's GPU can take: High is the most it handles well, Balanced
/// caps at 1080p, Light is 720p at 30 fps.
fn preset_values(hw: &Hardware, mon: &Monitor, level: usize) -> (u32, u32, u8) {
    let (max_h, fps, quality) = match hw.tier() {
        "CPU only" => (720, 30, 2),
        // The game and the recorder share one small chip: 30 fps halves what capture and the
        // encoder take from it (720p at 30 is what Medal suggests for weak PCs; Light gives that).
        "Integrated GPU" => (1080, 30, 2),
        "Entry GPU" => (1080, 60, 1),
        _ if hw.vram_mb >= 12 * 1024 => (2160, 60, 1),
        _ => (1440, 60, 1),
    };
    let (max_h, fps, quality) = match level {
        0 => (720, 30, 3),
        1 => (max_h.min(1080), fps, quality.max(2)),
        _ => (max_h, fps, quality),
    };
    (if mon.height > max_h { max_h } else { 0 }, fps, quality)
}

/// Sets the size, frame rate and quality for a quality choice.
pub fn set_preset(cfg: &mut Config, hw: &Hardware, mon: &Monitor, level: usize) {
    (cfg.out_height, cfg.fps, cfg.quality) = preset_values(hw, mon, level);
}

/// Which quality choice the settings match, best first; None if they were set by hand (under
/// More options, or imported from OBS).
pub fn preset_of(cfg: &Config, hw: &Hardware, mon: &Monitor) -> Option<usize> {
    (0..PRESETS.len()).rev().find(|&l| preset_values(hw, mon, l) == (cfg.out_height, cfg.fps, cfg.quality))
}

/// Sets capture, quality, clip length and audio to suit this PC. Returns a one-line summary.
pub fn apply(cfg: &mut Config, hw: &Hardware, mon: &Monitor) -> String {
    set_preset(cfg, hw, mon, PRESETS.len() - 1);
    // H.264 plays everywhere: Discord, phones, editors.
    cfg.codec = "h264".into();
    // 3:00 clips, or the longest shorter one whose replay fits in an eighth of the RAM.
    let budget = hw.ram_mb / 8;
    for secs in [180, 120, 60, 30] {
        cfg.buffer_secs = secs;
        if recorder::plan(cfg, mon).mb <= budget {
            break;
        }
    }
    // Game and desktop sound on track 1, unless a playback device is recorded already (a headset
    // picked under More options): ticking the default one too could record it twice. The mic
    // stays the user's choice.
    let n = cfg.tracks();
    if !cfg.devices.iter().any(|(k, d)| k.starts_with("out:") && d.tracks[..n].iter().any(|&t| t)) {
        let out = cfg
            .devices
            .entry("out:default".into())
            .or_insert_with(|| DeviceSel { name: "Default playback device".into(), ..Default::default() });
        out.tracks[0] = true;
    }

    let p = recorder::plan(cfg, mon);
    format!(
        "{}: {}p at {} fps, {} quality, {}:{:02} clips (up to {:.1} GB of RAM).",
        hw.tier(),
        p.out_h,
        p.fps,
        ["Highest", "High", "Medium", "Low"][cfg.quality as usize],
        cfg.buffer_secs / 60,
        cfg.buffer_secs % 60,
        p.mb as f64 / 1024.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(w: u32, h: u32) -> Monitor {
        Monitor { id: String::new(), width: w, height: h, hz: 144, x: 0, y: 0, primary: true, hdr: None, name: String::new() }
    }

    fn hw(gpu: Gpu, vram_gb: u64, ram_gb: u64) -> Hardware {
        Hardware { gpu, gpu_name: String::new(), vram_mb: vram_gb * 1024, ram_mb: ram_gb * 1024, integrated: gpu != Gpu::None && vram_gb < 2 }
    }

    /// On this PC, read only: each adapter and whether Direct3D 12 says it shares RAM, and the
    /// tier picked. Run by hand with `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_adapters() {
        for a in recorder::adapters() {
            println!("{} (vendor {:04X}, {} MB): shares RAM {:?}", a.name, a.vendor, a.vram_mb, recorder::shares_ram(a.index));
        }
        let h = detect();
        println!("picked: {}, tier {}", h.summary(), h.tier());
    }

    /// Built-in graphics get 30 fps, whatever graphics memory they report: an AMD laptop chip
    /// with 2 GB of RAM set aside isn't a gaming GPU.
    #[test]
    fn built_in_graphics_stay_light() {
        let mut c = Config::default();
        let amd_laptop = Hardware { integrated: true, ..hw(Gpu::Amd, 2, 16) };
        apply(&mut c, &amd_laptop, &mon(1920, 1080));
        assert_eq!((c.out_height, c.fps, c.quality), (0, 30, 2), "native 1080p at 30 fps");
        apply(&mut c, &hw(Gpu::Amd, 2, 16), &mon(1920, 1080));
        assert_eq!(c.fps, 60, "the same memory on a real graphics card is an entry GPU");
        apply(&mut c, &Hardware { integrated: true, ..hw(Gpu::Intel, 0, 8) }, &mon(2560, 1440));
        assert_eq!((c.out_height, c.fps), (1080, 30));
    }

    #[test]
    fn tiers() {
        let mut c = Config::default();
        apply(&mut c, &hw(Gpu::None, 0, 8), &mon(2560, 1440));
        assert_eq!((c.out_height, c.fps, c.quality), (720, 30, 2));

        apply(&mut c, &hw(Gpu::Intel, 0, 16), &mon(1920, 1080));
        assert_eq!((c.out_height, c.fps), (0, 30)); // native 1080p, at 30 fps on built-in graphics

        apply(&mut c, &hw(Gpu::Nvidia, 8, 32), &mon(3840, 2160));
        assert_eq!(c.out_height, 1440);
        assert_eq!(c.buffer_secs, 180);

        apply(&mut c, &hw(Gpu::Amd, 16, 32), &mon(3840, 2160));
        assert_eq!(c.out_height, 0); // native 4K
    }

    #[test]
    fn presets_scale_to_the_pc() {
        let mut c = Config::default();
        let (h, m) = (hw(Gpu::Nvidia, 8, 32), mon(2560, 1440));
        for level in 0..PRESETS.len() {
            set_preset(&mut c, &h, &m, level);
            assert_eq!(preset_of(&c, &h, &m), Some(level));
        }
        set_preset(&mut c, &h, &m, 0);
        assert_eq!((c.out_height, c.fps, c.quality), (720, 30, 3));
        set_preset(&mut c, &h, &m, 1);
        assert_eq!((c.out_height, c.fps, c.quality), (1080, 60, 2));
        set_preset(&mut c, &h, &m, 2);
        assert_eq!((c.out_height, c.fps, c.quality), (0, 60, 1)); // native 1440p
        c.fps = 120;
        assert_eq!(preset_of(&c, &h, &m), None);
    }

    /// Recommended settings tick the default playback device only when no playback device is
    /// recorded, so a headset picked by hand isn't recorded a second time through it.
    #[test]
    fn recommended_keeps_a_picked_playback_device() {
        let (h, m) = (hw(Gpu::Nvidia, 8, 32), mon(2560, 1440));
        let mut c = Config::default();
        c.devices.clear();
        apply(&mut c, &h, &m);
        assert!(c.devices["out:default"].tracks[0], "nothing recorded: the default device");
        c.devices.clear();
        c.devices.insert("out:{headset}".into(), DeviceSel { name: "Headset".into(), tracks: [true, false, false, false, false, false] });
        c.devices.insert("out:default".into(), DeviceSel { name: "Default playback device".into(), ..Default::default() });
        apply(&mut c, &h, &m);
        assert!(!c.devices["out:default"].tracks.iter().any(|&t| t));
        assert!(c.devices["out:{headset}"].tracks[0]);
        // A tick past the track count records nothing, so the default device is ticked.
        c.devices.get_mut("out:{headset}").unwrap().tracks = [false, false, false, true, false, false];
        c.track_count = 1;
        apply(&mut c, &h, &m);
        assert!(c.devices["out:default"].tracks[0]);
    }

    #[test]
    fn clip_length_fits_ram() {
        let mut c = Config::default();
        let m = mon(2560, 1440);
        apply(&mut c, &hw(Gpu::Nvidia, 8, 4), &m);
        assert!(recorder::plan(&c, &m).mb <= 512 || c.buffer_secs == 30);
        assert!(c.buffer_secs < 180);
    }
}
