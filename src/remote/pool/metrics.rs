//! Resource samples for Linux workers, respecting cgroup v2 memory limits.
use std::fs;

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Metrics {
    pub cpus: usize,
    pub available_mib: u64,
    pub load: Option<f64>,
    pub busy: Option<u8>,
}

impl Metrics {
    pub(super) fn compare_load(&self, other: &Self) -> std::cmp::Ordering {
        let per_cpu =
            |sample: &Self| sample.load.unwrap_or(f64::INFINITY) / sample.cpus.max(1) as f64;
        per_cpu(self).total_cmp(&per_cpu(other)).then_with(|| {
            self.busy
                .unwrap_or(u8::MAX)
                .cmp(&other.busy.unwrap_or(u8::MAX))
        })
    }
}

#[derive(Default)]
pub(super) struct Sampler {
    previous: Option<(u64, u64)>,
}

impl Sampler {
    pub fn read(&mut self) -> Metrics {
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

#[cfg(test)]
mod tests {
    use super::Metrics;
    #[test]
    fn placement_prefers_lower_normalized_load_then_lower_cpu_utilization() {
        let samples = [
            Metrics {
                cpus: 8,
                load: Some(2.0),
                busy: Some(20),
                ..Default::default()
            },
            Metrics {
                cpus: 4,
                load: Some(1.0),
                busy: Some(30),
                ..Default::default()
            },
            Metrics {
                cpus: 2,
                load: Some(1.0),
                busy: Some(10),
                ..Default::default()
            },
            Metrics::default(),
        ];
        for pair in samples.windows(2) {
            assert!(pair[0].compare_load(&pair[1]).is_lt());
        }
    }
}
