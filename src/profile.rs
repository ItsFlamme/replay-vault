//! Recommended settings for this PC, worked out from its GPU, RAM and monitor.

use crate::config::{Config, DeviceSel};
use crate::display::Monitor;
use crate::recorder::{self, Gpu};

pub struct Hardware {
    pub gpu: Gpu,
    pub gpu_name: String,
    pub vram_mb: u64,
    pub ram_mb: u64,
}

pub fn detect() -> Hardware {
    let gpu = recorder::gpu();
    // The encoding GPU; with two of the same make, the one with more memory.
    let adapter = recorder::adapters().into_iter().filter(|a| a.vendor == gpu.vendor()).max_by_key(|a| a.vram_mb);
    Hardware {
        gpu,
        gpu_name: adapter.as_ref().map_or("No supported GPU".into(), |a| a.name.clone()),
        vram_mb: adapter.map_or(0, |a| a.vram_mb),
        ram_mb: ram_mb(),
    }
}

fn ram_mb() -> u64 {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    if unsafe { GlobalMemoryStatusEx(&mut m) }.is_ok() { m.ullTotalPhys / (1024 * 1024) } else { 8 * 1024 }
}

impl Hardware {
    pub fn summary(&self) -> String {
        let vram = if self.vram_mb >= 1024 { format!(" ({} GB)", (self.vram_mb + 512) / 1024) } else { String::new() };
        format!("{}{vram}, {} GB RAM", self.gpu_name, (self.ram_mb + 512) / 1024)
    }

    fn tier(&self) -> &'static str {
        match (self.gpu, self.vram_mb) {
            (Gpu::None, _) => "CPU only",
            (_, v) if v < 2048 => "Integrated GPU",
            (_, v) if v < 6144 => "Entry GPU",
            _ => "Gaming GPU",
        }
    }
}

/// Sets capture, quality, clip length and audio to suit this PC. Returns a one-line summary.
pub fn apply(cfg: &mut Config, hw: &Hardware, mon: &Monitor) -> String {
    // Clip height cap, frame rate and quality (0 Highest .. 3 Low) by how much the GPU can take.
    let (max_h, fps, quality) = match hw.tier() {
        "CPU only" => (720, 30, 2),
        "Integrated GPU" => (1080, 60, 2),
        "Entry GPU" => (1080, 60, 1),
        _ if hw.vram_mb >= 12 * 1024 => (2160, 60, 1),
        _ => (1440, 60, 1),
    };
    cfg.out_height = if mon.height > max_h { max_h } else { 0 };
    cfg.fps = fps;
    cfg.quality = quality;
    // H.264 plays everywhere: Discord, phones, editors.
    cfg.codec = "h264".into();
    // Longest clip whose replay buffer fits in an eighth of the RAM.
    let budget = hw.ram_mb / 8;
    for secs in [300, 180, 120, 60, 30] {
        cfg.buffer_secs = secs;
        if recorder::plan(cfg, mon).mb <= budget {
            break;
        }
    }
    // Game and desktop sound on track 1; the mic stays the user's choice.
    let out = cfg
        .devices
        .entry("out:default".into())
        .or_insert_with(|| DeviceSel { name: "Default playback device".into(), ..Default::default() });
    out.tracks[0] = true;

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
        Monitor { id: String::new(), width: w, height: h, hz: 144, x: 0, y: 0, primary: true, hdr: None }
    }

    fn hw(gpu: Gpu, vram_gb: u64, ram_gb: u64) -> Hardware {
        Hardware { gpu, gpu_name: String::new(), vram_mb: vram_gb * 1024, ram_mb: ram_gb * 1024 }
    }

    #[test]
    fn tiers() {
        let mut c = Config::default();
        apply(&mut c, &hw(Gpu::None, 0, 8), &mon(2560, 1440));
        assert_eq!((c.out_height, c.fps, c.quality), (720, 30, 2));

        apply(&mut c, &hw(Gpu::Intel, 0, 16), &mon(1920, 1080));
        assert_eq!((c.out_height, c.fps), (0, 60)); // native 1080p

        apply(&mut c, &hw(Gpu::Nvidia, 8, 32), &mon(3840, 2160));
        assert_eq!(c.out_height, 1440);
        assert_eq!(c.buffer_secs, 300);

        apply(&mut c, &hw(Gpu::Amd, 16, 32), &mon(3840, 2160));
        assert_eq!(c.out_height, 0); // native 4K
    }

    #[test]
    fn clip_length_fits_ram() {
        let mut c = Config::default();
        let m = mon(2560, 1440);
        apply(&mut c, &hw(Gpu::Nvidia, 8, 4), &m);
        assert!(recorder::plan(&c, &m).mb <= 512 || c.buffer_secs == 30);
        assert!(c.buffer_secs < 300);
    }
}
