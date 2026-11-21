//! Sample the macOS kernel using the system-provided iostat and vm_stat tools.

#[cfg(target_os = "macos")]
#[derive(Default)]
pub(in crate::remote::pool) struct Sampler {}

#[cfg(target_os = "macos")]
impl Sampler {
    pub async fn read(&mut self) -> super::Metrics {
        let (cpu, memory) = tokio::join!(
            output("/usr/sbin/iostat", &["-n", "0", "-C", "-c", "2", "-w", "1"]),
            output("/usr/bin/vm_stat", &[]),
        );
        let (busy, load) = cpu.as_deref().and_then(cpu_sample).unzip();
        super::Metrics {
            cpus: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            available_mib: memory.as_deref().and_then(available_mib).unwrap_or(0),
            load,
            busy,
        }
    }
}

#[cfg(target_os = "macos")]
async fn output(path: &str, args: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new(path);
    command.args(args).env("LC_ALL", "C").kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(3), command.output())
        .await
        .ok()?
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

fn cpu_sample(iostat: &str) -> Option<(u8, f64)> {
    // The first row averages CPU usage since boot. The second covers the last second.
    let mut rows = iostat
        .lines()
        .skip_while(|line| {
            !line
                .split_whitespace()
                .eq(["us", "sy", "id", "1m", "5m", "15m"])
        })
        .skip(2);
    let mut row = rows.next()?.split_whitespace();
    let idle: u8 = row.nth(2)?.parse().ok()?;
    let load: f64 = row.next()?.parse().ok()?;
    (load.is_finite() && load >= 0.0).then_some((100_u8.checked_sub(idle)?, load))
}

fn available_mib(vm_stat: &str) -> Option<u64> {
    let mut lines = vm_stat.lines();
    let header = lines.next()?;
    let page_size: u64 = header
        .split_once("page size of ")?
        .1
        .strip_suffix(" bytes)")?
        .parse()
        .ok()?;
    let mut free = None;
    let mut inactive = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let count = match name {
            "Pages free" => &mut free,
            "Pages inactive" => &mut inactive,
            _ => continue,
        };
        *count = value.trim().trim_end_matches('.').parse::<u64>().ok();
    }
    free?
        .checked_add(inactive?)?
        .checked_mul(page_size)
        .map(|b| b / (1024 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_interval_cpu_and_one_minute_load() {
        let first = " cpu    load average\n us sy id   1m   5m   15m\n  8  3 90  1.72 1.57 1.75\n";
        assert_eq!(cpu_sample(first), None);
        assert_eq!(
            cpu_sample(&format!("{first}  2  1 97  0.50 1.57 1.75\n")),
            Some((3, 0.5))
        );
        assert_eq!(cpu_sample(&format!("{first}0 0 101 0 0 0")), None);
        assert_eq!(cpu_sample(&format!("{first}0 0 100 NaN 0 0")), None);
    }

    #[test]
    fn uses_reported_page_size_and_reclaimable_pages() {
        let sample = |size| {
            format!(
                "Mach Virtual Memory Statistics: (page size of {size} bytes)\nPages free: 192.\nPages speculative: 100.\nPages inactive: 64.\n"
            )
        };
        assert_eq!(available_mib(&sample(16384)), Some(4));
        assert_eq!(available_mib(&sample(4096)), Some(1));
        assert_eq!(
            available_mib(&sample(16384).replace("Pages free", "Unrecognized")),
            None
        );
        assert_eq!(available_mib(&sample(4096).replace("64.", "?")), None);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn system_tools_return_a_live_sample() {
        let metrics = Sampler::default().read().await;
        assert!(metrics.cpus > 0 && metrics.available_mib > 0);
        assert!(metrics.load.is_some() && metrics.busy.is_some());
    }
}
