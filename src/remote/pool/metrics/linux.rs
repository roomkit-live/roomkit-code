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
    let bytes = kb.unwrap_or(0).saturating_mul(1024);
    let membership = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    capped_memory(bytes, std::path::Path::new("/sys/fs/cgroup"), &membership) / (1024 * 1024)
}

fn capped_memory(mut bytes: u64, mount: &std::path::Path, membership: &str) -> u64 {
    use std::path::{Component, Path};
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::/"))
        .map(Path::new)
        .filter(|path| {
            path.components()
                .all(|part| matches!(part, Component::Normal(_)))
        })
        .unwrap_or(Path::new(""));
    // A service can have an unlimited leaf under a bounded systemd slice.
    for directory in mount
        .join(relative)
        .ancestors()
        .take_while(|dir| dir.starts_with(mount))
    {
        let read = |name| {
            fs::read_to_string(directory.join(name))
                .ok()?
                .trim()
                .parse::<u64>()
                .ok()
        };
        if let Some((limit, used)) = read("memory.max").zip(read("memory.current")) {
            bytes = bytes.min(limit.saturating_sub(used));
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accounts_for_both_service_and_parent_limits() -> std::io::Result<()> {
        let root = tempfile::tempdir()?;
        let slice = root.path().join("user.slice");
        let service = slice.join("worker.service");
        fs::create_dir_all(&service)?;
        for (path, limit, used) in [(&slice, "900", "750"), (&service, "500", "100")] {
            fs::write(path.join("memory.max"), limit)?;
            fs::write(path.join("memory.current"), used)?;
        }
        assert_eq!(
            capped_memory(4000, root.path(), "0::/user.slice/worker.service"),
            150
        );
        fs::write(slice.join("memory.max"), "max")?;
        assert_eq!(
            capped_memory(4000, root.path(), "0::/user.slice/worker.service"),
            400
        );
        assert_eq!(capped_memory(4000, root.path(), "0::/../outside"), 4000);
        Ok(())
    }
}
