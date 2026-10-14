//! Status labels and values, shared by the popover and native regressions.
use super::format::format_size;
use revaro_core::system::SystemStatusSummary;

pub struct SystemMetric {
    pub label: &'static str,
    pub value: Option<String>,
    pub title: String,
}

fn optional_size(bytes: Option<u64>) -> String {
    bytes.map(format_size).unwrap_or_else(|| "—".to_owned())
}

fn pair(used: Option<u64>, total: Option<u64>) -> String {
    if used.is_none() && total.is_none() {
        "—".to_owned()
    } else {
        format!("{} / {}", optional_size(used), optional_size(total))
    }
}

pub fn storage_usage(status: Option<&SystemStatusSummary>) -> Option<f64> {
    status
        .filter(|s| s.disk_total_bytes > 0)
        .map(|s| (s.disk_used_bytes as f64 / s.disk_total_bytes as f64 * 100.0).clamp(0.0, 100.0))
}

pub fn server_metrics(status: Option<&SystemStatusSummary>) -> [SystemMetric; 5] {
    let values = status.map(|s| {
        [
            pair(s.memory.host_used_bytes, s.memory.host_total_bytes),
            optional_size(s.memory.available_bytes),
            optional_size(s.memory.process_rss_bytes),
            pair(Some(s.cache.memory_bytes), s.cache.memory_limit),
            format_size(s.cache.disk_bytes),
        ]
    });
    let available_title = status.map_or_else(String::new, |s| {
        let host = format!(
            "宿主机可用：{}",
            optional_size(s.memory.host_available_bytes)
        );
        if let Some(limit) = s.memory.cgroup_limit_bytes {
            format!(
                "{host}；容器上限：{}；容器余量：{}",
                format_size(limit),
                optional_size(s.memory.cgroup_available_bytes)
            )
        } else {
            host
        }
    });
    let rows = [
        (
            "宿主机内存",
            "已用 / 总量；已用 = 总量 − 可用内存".to_owned(),
        ),
        ("实际可用内存", available_title),
        ("Revaro RSS", "Revaro 进程实际驻留内存".to_owned()),
        (
            "应用缓存",
            "已用 / 动态上限；上限每 5 秒按内存压力调整".to_owned(),
        ),
        ("磁盘缓存", "应用磁盘缓存已用空间".to_owned()),
    ];
    std::array::from_fn(|index| SystemMetric {
        label: rows[index].0,
        value: values.as_ref().map(|values| values[index].clone()),
        title: rows[index].1.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use revaro_core::system::{CacheSummary, MemorySummary};

    fn status() -> SystemStatusSummary {
        SystemStatusSummary {
            disk_total_bytes: 100 << 30,
            disk_used_bytes: 42 << 30,
            disk_available_bytes: 58 << 30,
            cache: CacheSummary {
                memory_bytes: 128 << 20,
                memory_limit: Some(640 << 20),
                disk_bytes: 3 << 20,
            },
            memory: MemorySummary {
                host_total_bytes: Some(16 << 30),
                host_used_bytes: Some(4 << 30),
                host_available_bytes: Some(12 << 30),
                available_bytes: Some(768 << 20),
                process_rss_bytes: Some(256 << 20),
                cgroup_limit_bytes: Some(1 << 30),
                cgroup_available_bytes: Some(768 << 20),
            },
        }
    }

    #[test]
    fn host_rss_and_cache_are_separate_and_use_actual_values() {
        let status = status();
        let rows = server_metrics(Some(&status));
        assert_eq!(rows[0].value.as_deref(), Some("4.0 GB / 16.0 GB"));
        assert_eq!(rows[1].value.as_deref(), Some("768.0 MB"));
        assert_eq!(rows[2].value.as_deref(), Some("256.0 MB"));
        assert_eq!(rows[3].value.as_deref(), Some("128.0 MB / 640.0 MB"));
        assert_eq!(rows[4].value.as_deref(), Some("3.0 MB"));
        assert!(rows[1].title.contains("容器上限：1.0 GB"));
    }

    #[test]
    fn missing_metrics_show_dashes_and_zero_limit_is_visible() {
        let mut status = status();
        status.memory = MemorySummary::default();
        status.cache.memory_bytes = 0;
        status.cache.memory_limit = Some(0);
        let rows = server_metrics(Some(&status));
        for row in &rows[..3] {
            assert_eq!(row.value.as_deref(), Some("—"));
        }
        assert_eq!(rows[3].value.as_deref(), Some("0 B / 0 B"));
        status.cache.memory_limit = None;
        assert_eq!(
            server_metrics(Some(&status))[3].value.as_deref(),
            Some("0 B / —")
        );
        assert!(server_metrics(None).iter().all(|row| row.value.is_none()));
    }

    #[test]
    fn storage_percentage_uses_disk_usage_and_handles_missing_or_inconsistent_totals() {
        let mut status = status();
        assert_eq!(storage_usage(Some(&status)), Some(42.0));
        assert_eq!(storage_usage(None), None);
        status.disk_total_bytes = 0;
        assert_eq!(storage_usage(Some(&status)), None);
        status.disk_total_bytes = 100;
        status.disk_used_bytes = 0;
        assert_eq!(storage_usage(Some(&status)), Some(0.0));
        status.disk_used_bytes = 100;
        assert_eq!(storage_usage(Some(&status)), Some(100.0));
        status.disk_used_bytes = u64::MAX;
        assert_eq!(storage_usage(Some(&status)), Some(100.0));
    }
}
