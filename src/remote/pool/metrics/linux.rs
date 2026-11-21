//! Linux resource sampling, including cgroup v2 memory limits.
use super::Metrics;
use std::fs;

#[derive(Default)]
pub(in crate::remote::pool) struct Sampler {
    previous: Option<(u64, u64)>,
}

impl Sampler {
    pub async fn read(&mut self) -> Metrics {
        let load = fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|l| l.split_whitespace().next()?.parse().ok());
        let cpu = fs::read_to_string("/proc/stat").ok().and_then(|s| {
            let values: Vec<u64> = s
                .lines()
                .next()?
                .split_whitespace()
                .skip(1)
                .take(8)
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            Some((values.iter().sum::<u64>(), values.get(3)? + values.get(4)?))
        });
        let busy = cpu.and_then(|(total, idle)| {
            let (old, old_idle) = self.previous.replace((total, idle))?;
            let elapsed = total.checked_sub(old)?;
            (elapsed > 0).then(|| {
                (100 * elapsed.saturating_sub(idle.saturating_sub(old_idle)) / elapsed) as u8
            })
        });
        Metrics {
            cpus: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            available_mib: available_mib(),
            load,
            busy,
        }
    }
}
fn available_mib() -> u64 {
    let mem = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let kb = mem.lines().find_map(|l| {
        l.strip_prefix("MemAvailable:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    });
    let mut bytes = kb.unwrap_or(0).saturating_mul(1024);
    let cgroup = |name| {
        fs::read_to_string(format!("/sys/fs/cgroup/{name}"))
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
    };
    if let Some((limit, used)) = cgroup("memory.max").zip(cgroup("memory.current")) {
        bytes = bytes.min(limit.saturating_sub(used));
    }
    bytes / (1024 * 1024)
}
