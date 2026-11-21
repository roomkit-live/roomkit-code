//! Resource samples used for executor placement.
#[cfg(not(target_os = "macos"))]
mod linux;
#[cfg(any(test, target_os = "macos"))]
mod macos;

#[cfg(not(target_os = "macos"))]
pub(super) use linux::Sampler;
#[cfg(target_os = "macos")]
pub(super) use macos::Sampler;

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
