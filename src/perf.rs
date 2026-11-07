//! How much the hidden recorder (and this app) costs while you play: CPU, GPU, the GPU's video
//! encoder and memory. Only our own processes are read, never the game: process times and memory
//! come from the process itself, GPU use from Windows' "GPU Engine" performance counters (the same
//! numbers Task Manager shows).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::core::w;

/// One process's use, as shares of the whole PC (CPU: all cores together).
#[derive(Clone, Copy, Default, Debug)]
pub struct Usage {
    pub cpu: f64,
    pub gpu: f64,
    pub encoder: f64,
    pub ram_mb: f64,
}

/// Average and peak recorder use over a session, kept as running totals.
#[derive(Clone, Copy, Default, Debug)]
pub struct Load {
    samples: u32,
    sum: Usage,
    peak: Usage,
}

impl Load {
    pub fn add(&mut self, u: Usage) {
        self.samples += 1;
        for (sum, peak, v) in [(&mut self.sum.cpu, &mut self.peak.cpu, u.cpu), (&mut self.sum.gpu, &mut self.peak.gpu, u.gpu), (&mut self.sum.encoder, &mut self.peak.encoder, u.encoder)] {
            *sum += v;
            *peak = peak.max(v);
        }
    }

    /// For the session report, e.g. "avg CPU 2.0%, GPU 1.5%, video encoder 10.0%; peak 4.0%, 3.0%, 20.0%".
    pub fn summary(&self) -> String {
        if self.samples == 0 {
            return "not measured".into();
        }
        let n = self.samples as f64;
        let (s, p) = (self.sum, self.peak);
        format!(
            "avg CPU {:.1}%, GPU {:.1}%, video encoder {:.1}%; peak {:.1}%, {:.1}%, {:.1}%",
            s.cpu / n,
            s.gpu / n,
            s.encoder / n,
            p.cpu,
            p.gpu,
            p.encoder
        )
    }
}

/// Samples once a second while the Logs page is open; dropped when it closes.
pub struct Sampler {
    query: PDH_HQUERY,
    engines: PDH_HCOUNTER,
    /// Per pid: CPU time used so far (100 ns units) and when it was read.
    cpu_before: Vec<(u32, u64, Instant)>,
    last: Instant,
    pub recorder: Option<Usage>,
    pub app: Option<Usage>,
}

impl Sampler {
    pub fn new() -> Self {
        let mut query = PDH_HQUERY::default();
        let mut engines = PDH_HCOUNTER::default();
        unsafe {
            if PdhOpenQueryW(None, 0, &mut query) == 0 {
                PdhAddEnglishCounterW(query, w!("\\GPU Engine(*)\\Utilization Percentage"), 0, &mut engines);
                PdhCollectQueryData(query);
            }
        }
        Self { query, engines, cpu_before: Vec::new(), last: Instant::now() - Duration::from_secs(2), recorder: None, app: None }
    }

    /// Takes a new sample if a second has passed. `recorder` is the hidden OBS's process id.
    pub fn tick(&mut self, recorder: Option<u32>) {
        if self.last.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last = Instant::now();
        let engines = self.engines();
        let me = std::process::id();
        self.app = self.usage(me, &engines);
        self.recorder = recorder.and_then(|pid| self.usage(pid, &engines));
    }

    fn usage(&mut self, pid: u32, engines: &[(String, f64)]) -> Option<Usage> {
        let (cpu_time, ram_mb) = process_stats(pid)?;
        let now = Instant::now();
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
        let cpu = match self.cpu_before.iter().find(|(p, ..)| *p == pid) {
            Some(&(_, before, at)) => {
                let wall = now.duration_since(at).as_secs_f64() * 1e7;
                (cpu_time.saturating_sub(before) as f64 / wall / cores * 100.0).clamp(0.0, 100.0)
            }
            None => 0.0,
        };
        self.cpu_before.retain(|(p, ..)| *p != pid);
        self.cpu_before.push((pid, cpu_time, now));
        // Instances look like "pid_1234_luid_..._phys_0_eng_3_engtype_3D".
        let tag = format!("pid_{pid}_");
        let sum = |kind: &str| -> f64 {
            engines.iter().filter(|(n, _)| n.starts_with(&tag) && n.ends_with(kind)).map(|(_, v)| v).sum::<f64>().min(100.0)
        };
        Some(Usage { cpu, gpu: sum("engtype_3D"), encoder: sum("engtype_VideoEncode"), ram_mb })
    }

    /// Every GPU engine instance and its use, in percent.
    fn engines(&self) -> Vec<(String, f64)> {
        unsafe {
            if self.query.is_invalid() || PdhCollectQueryData(self.query) != 0 {
                return Vec::new();
            }
            let (mut size, mut count) = (0u32, 0u32);
            PdhGetFormattedCounterArrayW(self.engines, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if size == 0 {
                return Vec::new();
            }
            // The items and the names they point to share this buffer.
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(self.engines, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
                return Vec::new();
            }
            std::slice::from_raw_parts(items, count as usize)
                .iter()
                .filter(|i| i.FmtValue.CStatus == 0)
                .filter_map(|i| Some((i.szName.to_string().ok()?, i.FmtValue.Anonymous.doubleValue)))
                .collect()
        }
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        if !self.query.is_invalid() {
            unsafe { PdhCloseQuery(self.query) };
        }
    }
}

/// CPU time used so far (kernel + user, 100 ns units) and private memory in MB.
fn process_stats(pid: u32) -> Option<(u64, f64)> {
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let times = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).ok();
        let mut mem = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
        let mem_ok = K32GetProcessMemoryInfo(h, &mut mem as *mut _ as *mut PROCESS_MEMORY_COUNTERS, mem.cb).as_bool();
        let _ = CloseHandle(h);
        times?;
        let ft = |f: FILETIME| (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64;
        let ram = if mem_ok { mem.PrivateUsage as f64 / (1024.0 * 1024.0) } else { 0.0 };
        Some((ft(k) + ft(u), ram))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_keeps_average_and_peak() {
        let mut l = Load::default();
        assert_eq!(l.summary(), "not measured");
        l.add(Usage { cpu: 1.0, gpu: 4.0, encoder: 10.0, ram_mb: 900.0 });
        l.add(Usage { cpu: 3.0, gpu: 2.0, encoder: 30.0, ram_mb: 900.0 });
        assert_eq!(l.summary(), "avg CPU 2.0%, GPU 3.0%, video encoder 20.0%; peak 3.0%, 4.0%, 30.0%");
    }

    /// Reads a running recorder's numbers (pid in RECORDER_PID), only reading. Run on demand:
    /// cargo test reads_a_recorder -- --ignored --nocapture
    #[test]
    #[ignore]
    fn reads_a_recorder() {
        let pid: u32 = std::env::var("RECORDER_PID").unwrap().parse().unwrap();
        let mut s = Sampler::new();
        s.tick(Some(pid));
        std::thread::sleep(Duration::from_millis(1500));
        s.tick(Some(pid));
        let r = s.recorder.expect("recorder readable");
        println!("{r:?}");
        assert!(r.ram_mb > 100.0 && r.cpu > 0.0);
    }

    /// Reads this test process's own numbers twice, a second apart, while it keeps a core busy.
    #[test]
    fn samples_its_own_process() {
        let mut s = Sampler::new();
        s.tick(None);
        let stop = Instant::now() + Duration::from_millis(1100);
        let mut x = 0u64;
        while Instant::now() < stop {
            x = x.wrapping_mul(31).wrapping_add(1);
        }
        std::hint::black_box(x);
        s.tick(None);
        let app = s.app.expect("own process readable");
        assert!(app.cpu > 0.0, "{app:?}");
        assert!(app.ram_mb > 1.0, "{app:?}");
        assert!(s.recorder.is_none());
        // The GPU counters exist on this PC (any process using the GPU shows up).
        assert!(!s.engines().is_empty());
    }
}
