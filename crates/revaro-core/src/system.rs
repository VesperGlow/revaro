//! Operational metrics. Missing kernel information stays unknown, never zero.

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MemorySummary {
    pub host_total_bytes: Option<u64>,
    /// MemTotal - MemAvailable, including memory that cannot readily be reclaimed.
    pub host_used_bytes: Option<u64>,
    pub host_available_bytes: Option<u64>,
    /// The smaller of host MemAvailable and every finite cgroup's headroom.
    pub available_bytes: Option<u64>,
    pub process_rss_bytes: Option<u64>,
    pub cgroup_limit_bytes: Option<u64>,
    pub cgroup_available_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CacheSummary {
    pub memory_bytes: u64,
    #[serde(default)]
    pub memory_limit: Option<u64>,
    pub disk_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SystemStatusSummary {
    pub disk_total_bytes: u64,
    pub disk_used_bytes: u64,
    pub disk_available_bytes: u64,
    pub cache: CacheSummary,
    #[serde(default)]
    pub memory: MemorySummary,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_kernel_metrics_and_cache_limit_remain_unknown() {
        let status: SystemStatusSummary = serde_json::from_str(
            r#"{"disk_total_bytes":100,"disk_used_bytes":40,"disk_available_bytes":60,"cache":{"memory_bytes":2,"disk_bytes":3}}"#,
        )
        .unwrap();
        assert_eq!(status.memory, MemorySummary::default());
        assert_eq!(status.cache.memory_limit, None);
    }

    #[test]
    fn zero_limit_and_unavailable_metrics_survive_the_wire_format() {
        let status: SystemStatusSummary = serde_json::from_str(
            r#"{"disk_total_bytes":100,"disk_used_bytes":40,"disk_available_bytes":60,"cache":{"memory_bytes":0,"memory_limit":0,"disk_bytes":3},"memory":{"host_total_bytes":8192,"host_used_bytes":6144,"available_bytes":0,"process_rss_bytes":null,"cgroup_limit_bytes":1024}}"#,
        )
        .unwrap();
        assert_eq!(status.cache.memory_limit, Some(0));
        assert_eq!(status.memory.available_bytes, Some(0));
        assert_eq!(status.memory.process_rss_bytes, None);
        assert_eq!(status.memory.cgroup_limit_bytes, Some(1024));
        let encoded = serde_json::to_string(&status).unwrap();
        assert_eq!(
            serde_json::from_str::<SystemStatusSummary>(&encoded).unwrap(),
            status
        );
    }
}
