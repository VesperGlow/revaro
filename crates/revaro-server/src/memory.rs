use revaro_core::system::MemorySummary;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct MemoryBudget {
    pub total_bytes: i64,
    pub cache_bytes: i64,
    pub database_bytes: i64,
    pub mmap_bytes: i64,
}

impl MemoryBudget {
    pub fn detect(requested: i64) -> Self {
        Self::from_summary(requested, &MemoryProbe::detect().summary)
    }

    fn from_summary(requested: i64, memory: &MemorySummary) -> Self {
        Self::constrained(
            requested,
            memory
                .host_total_bytes
                .map(budget_bytes)
                .unwrap_or(requested),
            memory
                .cgroup_limit_bytes
                .map(budget_bytes)
                .unwrap_or(requested),
        )
    }

    fn constrained(requested: i64, physical: i64, container: i64) -> Self {
        Self::new(requested.min(physical).min(container))
    }

    pub fn new(total_bytes: i64) -> Self {
        let total_bytes = total_bytes.max(0);
        Self {
            total_bytes,
            cache_bytes: total_bytes / 8 * 5,
            database_bytes: total_bytes / 32,
            mmap_bytes: total_bytes / 16,
        }
    }

    pub fn cache_limit(&self, cached: i64, usage: i64, available: Option<i64>) -> i64 {
        let reserve = self.total_bytes / 8;
        let process_limit = cached.saturating_add(
            self.total_bytes
                .saturating_sub(reserve)
                .saturating_sub(usage),
        );
        let available_limit = available
            .map(|available| cached.saturating_add(available.saturating_sub(reserve)))
            .unwrap_or(self.cache_bytes);
        self.cache_bytes
            .min(process_limit)
            .min(available_limit)
            .max(0)
    }

    pub fn current_cache_limit(&self, cached: i64) -> i64 {
        self.cache_limit_for_probe(cached, &MemoryProbe::detect())
    }

    fn cache_limit_for_probe(&self, cached: i64, probe: &MemoryProbe) -> i64 {
        let memory = &probe.summary;
        // Never interpret a failed probe as an idle process or an empty cgroup.
        // Disabling L1 remains recoverable on the next five-second adjustment.
        if probe.cgroup_unreadable
            || memory.available_bytes == Some(0)
            || (memory.available_bytes.is_none() && memory.process_rss_bytes.is_none())
        {
            return 0;
        }
        // Limits may be lowered while the server is running. Keep the startup
        // ceiling but immediately honour any newly observed, smaller limit.
        let budget = Self::from_summary(self.total_bytes, memory);
        let cached = cached.max(0);
        budget.cache_limit(
            cached,
            memory.process_rss_bytes.map(budget_bytes).unwrap_or(cached),
            memory.available_bytes.map(budget_bytes),
        )
    }
}

/// A fresh kernel snapshot; probing failures do not fail the status endpoint.
pub fn read_memory() -> MemorySummary {
    MemoryProbe::detect().summary
}

#[derive(Debug)]
struct MemoryProbe {
    summary: MemorySummary,
    cgroup_unreadable: bool,
}

impl MemoryProbe {
    fn detect() -> Self {
        Self::read(Path::new("/proc"), Path::new("/sys/fs/cgroup"))
    }

    fn read(proc_root: &Path, cgroup_root: &Path) -> Self {
        let meminfo = std::fs::read_to_string(proc_root.join("meminfo")).unwrap_or_default();
        let total = kib_field(&meminfo, "MemTotal:").filter(|total| *total > 0);
        let available = kib_field(&meminfo, "MemAvailable:")
            .map(|available| total.map_or(available, |total| available.min(total)));
        let mut summary = MemorySummary {
            host_total_bytes: total,
            host_used_bytes: total
                .zip(available)
                .map(|(total, available)| total - available),
            host_available_bytes: available,
            process_rss_bytes: std::fs::read_to_string(proc_root.join("self/status"))
                .ok()
                .and_then(|status| kib_field(&status, "VmRSS:")),
            ..Default::default()
        };
        let mut cgroup_unreadable = false;
        for (directory, mount_root) in cgroup_directories(proc_root, cgroup_root) {
            let contents = match std::fs::read_to_string(directory.join("memory.max")) {
                Ok(contents) => contents,
                // The global cgroup v2 root has no memory.max interface.
                Err(error) if mount_root && error.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                Err(_) => {
                    cgroup_unreadable = true;
                    continue;
                }
            };
            if contents.trim() == "max" {
                continue;
            }
            let Some(limit) = parse_bytes(&contents) else {
                cgroup_unreadable = true;
                continue;
            };
            summary.cgroup_limit_bytes = min_known(summary.cgroup_limit_bytes, Some(limit));
            let Some(current) = std::fs::read_to_string(directory.join("memory.current"))
                .ok()
                .and_then(|contents| parse_bytes(&contents))
            else {
                cgroup_unreadable = true;
                continue;
            };
            // Use hard-limit headroom. inactive_file is not guaranteed to be
            // reclaimable and must not allow cache growth past memory.max.
            summary.cgroup_available_bytes = min_known(
                summary.cgroup_available_bytes,
                Some(limit.saturating_sub(current)),
            );
        }
        if cgroup_unreadable {
            summary.cgroup_available_bytes = None;
        } else {
            summary.available_bytes = min_known(available, summary.cgroup_available_bytes);
        }
        Self {
            summary,
            cgroup_unreadable,
        }
    }
}

fn budget_bytes(bytes: u64) -> i64 {
    bytes.min(i64::MAX as u64) as i64
}

fn min_known(first: Option<u64>, second: Option<u64>) -> Option<u64> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (first, second) => first.or(second),
    }
}

fn parse_bytes(contents: &str) -> Option<u64> {
    contents.trim().parse().ok()
}

fn kib_field(contents: &str, field: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let mut values = line.split_whitespace();
        if values.next()? != field {
            return None;
        }
        let value = values.next()?.parse::<u64>().ok()?;
        if values.next()? != "kB" {
            return None;
        }
        value.checked_mul(1024)
    })
}

/// Resolve the visible cgroup v2 mount and walk only this process's ancestors.
/// mountinfo handles nonstandard mounts and container-mounted subtrees.
fn cgroup_directories(proc_root: &Path, fallback_root: &Path) -> Vec<(PathBuf, bool)> {
    let group = std::fs::read_to_string(proc_root.join("self/cgroup"))
        .ok()
        .and_then(|contents| {
            contents
                .lines()
                .find_map(|line| line.strip_prefix("0::").and_then(absolute_path))
        });
    let mountinfo = std::fs::read_to_string(proc_root.join("self/mountinfo")).unwrap_or_default();
    let mut mounts = mountinfo
        .lines()
        .filter_map(|line| {
            let (fields, filesystem) = line.split_once(" - ")?;
            if filesystem.split_whitespace().next()? != "cgroup2" {
                return None;
            }
            let fields: Vec<_> = fields.split_whitespace().collect();
            Some((
                absolute_path(&unescape_mount(fields.get(3)?)?)?,
                absolute_path(&unescape_mount(fields.get(4)?)?)?,
            ))
        })
        .collect::<Vec<_>>();
    if mounts.is_empty() {
        mounts.push((PathBuf::from("/"), fallback_root.to_owned()));
    }
    let mut directories = Vec::new();
    for (mount_tree, mount) in mounts {
        let mut directory = group.as_ref().map_or_else(
            || mount.clone(),
            |group| {
                // /proc/self/cgroup may be relative to a cgroup namespace, while an
                // inherited mount still reports the original hierarchy's root.
                let relative = group
                    .strip_prefix(&mount_tree)
                    .or_else(|_| group.strip_prefix("/"))
                    .expect("cgroup paths are absolute");
                mount.join(relative)
            },
        );
        if !directory.is_dir() {
            directory = mount.clone();
        }
        loop {
            if !directories.iter().any(|(path, _)| *path == directory) {
                directories.push((directory.clone(), directory == mount));
            }
            if directory == mount || !directory.pop() {
                break;
            }
        }
    }
    directories
}

fn absolute_path(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    (path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_))))
    .then(|| path.to_owned())
}

fn unescape_mount(value: &str) -> Option<String> {
    let mut bytes = value.bytes();
    let mut output = Vec::with_capacity(value.len());
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            output.push(byte);
            continue;
        }
        let first = bytes.next()?.checked_sub(b'0')?;
        let second = bytes.next()?.checked_sub(b'0')?;
        let third = bytes.next()?.checked_sub(b'0')?;
        if first > 3 || second > 7 || third > 7 {
            return None;
        }
        output.push(first * 64 + second * 8 + third);
    }
    String::from_utf8(output).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "revaro-memory-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let fixture = Self { root };
            fixture.write(
                "proc/meminfo",
                "MemTotal: 16777216 kB\nMemAvailable: 12582912 kB\nMemFree: 1048576 kB\n",
            );
            fixture.write("proc/self/status", "VmRSS: 262144 kB\nVmSize: 8388608 kB\n");
            fixture.write("proc/self/cgroup", "0::/app/worker\n");
            fixture.write("cgroup/app/worker/memory.max", "max\n");
            fixture.write("cgroup/app/memory.max", "max\n");
            fixture
        }

        fn write(&self, path: &str, contents: &str) {
            let path = self.root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        fn probe(&self) -> MemoryProbe {
            MemoryProbe::read(&self.root.join("proc"), &self.root.join("cgroup"))
        }

        fn limit(&self, group: &str, limit: u64, current: u64) {
            self.write(&format!("cgroup/{group}/memory.max"), &limit.to_string());
            self.write(
                &format!("cgroup/{group}/memory.current"),
                &current.to_string(),
            );
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn eight_gib_budget_reserves_database_and_runtime_headroom() {
        let budget = MemoryBudget::new(8 << 30);
        assert_eq!(budget.cache_bytes, 5 << 30);
        assert_eq!(budget.database_bytes, 256 << 20);
        assert_eq!(budget.mmap_bytes, 512 << 20);
    }

    #[test]
    fn physical_and_container_limits_clamp_the_requested_budget() {
        assert_eq!(
            MemoryBudget::constrained(8 << 30, 16 << 30, 2 << 30).total_bytes,
            2 << 30
        );
        assert_eq!(
            MemoryBudget::constrained(8 << 30, 1 << 30, 8 << 30).total_bytes,
            1 << 30
        );
        assert_eq!(
            MemoryBudget::constrained(8 << 30, 16 << 30, 16 << 30).total_bytes,
            8 << 30
        );
    }

    #[test]
    fn pressure_reclaims_cache_and_recovers_without_exceeding_capacity() {
        let budget = MemoryBudget::new(8 << 30);
        assert_eq!(budget.cache_limit(5 << 30, 6 << 30, None), 5 << 30);
        assert_eq!(budget.cache_limit(5 << 30, 8 << 30, None), 4 << 30);
        assert_eq!(
            budget.cache_limit(5 << 30, 6 << 30, Some(512 << 20)),
            4608 << 20
        );
        assert_eq!(budget.cache_limit(0, 8 << 30, None), 0);
        assert_eq!(budget.cache_limit(1 << 30, 2 << 30, None), 5 << 30);
    }

    #[test]
    fn host_usage_uses_available_memory_and_rss_excludes_virtual_size() {
        let fixture = Fixture::new();
        let probe = fixture.probe();
        assert_eq!(probe.summary.host_total_bytes, Some(16 << 30));
        assert_eq!(probe.summary.host_used_bytes, Some(4 << 30));
        assert_eq!(probe.summary.host_available_bytes, Some(12 << 30));
        assert_eq!(probe.summary.available_bytes, Some(12 << 30));
        assert_eq!(probe.summary.process_rss_bytes, Some(256 << 20));
        assert_eq!(probe.summary.cgroup_limit_bytes, None);
        assert!(!probe.cgroup_unreadable);
    }

    #[test]
    fn ancestor_limits_and_shared_usage_take_priority_over_host_memory() {
        let fixture = Fixture::new();
        fixture.limit("app/worker", 2 << 30, 256 << 20);
        fixture.limit("app", 4 << 30, 3840 << 20);
        fixture.write("cgroup/app/memory.stat", "inactive_file 2147483648\n");
        let probe = fixture.probe();
        assert_eq!(probe.summary.host_total_bytes, Some(16 << 30));
        assert_eq!(probe.summary.cgroup_limit_bytes, Some(2 << 30));
        assert_eq!(probe.summary.cgroup_available_bytes, Some(256 << 20));
        assert_eq!(probe.summary.available_bytes, Some(256 << 20));
        let budget = MemoryBudget::from_summary(8 << 30, &probe.summary);
        assert_eq!(budget.total_bytes, 2 << 30);
        assert_eq!(budget.cache_limit_for_probe(128 << 20, &probe), 128 << 20);
    }

    #[test]
    fn host_pressure_also_constrains_a_large_container() {
        let fixture = Fixture::new();
        fixture.limit("app", 8 << 30, 1 << 30);
        fixture.write(
            "proc/meminfo",
            "MemTotal: 16777216 kB\nMemAvailable: 524288 kB\n",
        );
        let probe = fixture.probe();
        assert_eq!(probe.summary.available_bytes, Some(512 << 20));
        assert_eq!(
            MemoryBudget::new(8 << 30).cache_limit_for_probe(1 << 30, &probe),
            512 << 20
        );
    }

    #[test]
    fn lowering_a_live_limit_shrinks_capacity_and_pressure_recovery_is_bounded() {
        let fixture = Fixture::new();
        let budget = MemoryBudget::new(8 << 30);
        fixture.limit("app", 1 << 30, 960 << 20);
        let probe = fixture.probe();
        assert_eq!(budget.cache_limit_for_probe(256 << 20, &probe), 192 << 20);
        fixture.limit("app", 1 << 30, 256 << 20);
        assert_eq!(
            budget.cache_limit_for_probe(192 << 20, &fixture.probe()),
            640 << 20
        );
        fixture.limit("app", 32 << 30, 256 << 20);
        assert_eq!(
            budget.cache_limit_for_probe(192 << 20, &fixture.probe()),
            5 << 30
        );
    }

    #[test]
    fn zero_or_exceeded_container_limit_disables_cache() {
        let fixture = Fixture::new();
        for (limit, current) in [(0, 0), (1 << 30, 1 << 30), (1 << 30, 2 << 30)] {
            fixture.limit("app", limit, current);
            let probe = fixture.probe();
            assert_eq!(probe.summary.available_bytes, Some(0));
            assert_eq!(
                MemoryBudget::new(8 << 30).cache_limit_for_probe(256 << 20, &probe),
                0
            );
        }
    }

    #[test]
    fn unavailable_proc_information_stays_unknown_and_cgroups_still_work() {
        let fixture = Fixture::new();
        std::fs::remove_file(fixture.root.join("proc/meminfo")).unwrap();
        std::fs::remove_file(fixture.root.join("proc/self/status")).unwrap();
        let probe = fixture.probe();
        assert_eq!(probe.summary, MemorySummary::default());
        let budget = MemoryBudget::new(8 << 30);
        assert_eq!(budget.cache_limit_for_probe(128 << 20, &probe), 0);
        fixture.limit("app", 1 << 30, 256 << 20);
        let probe = fixture.probe();
        assert_eq!(probe.summary.host_total_bytes, None);
        assert_eq!(probe.summary.process_rss_bytes, None);
        assert_eq!(probe.summary.available_bytes, Some(768 << 20));
        assert_eq!(budget.cache_limit_for_probe(128 << 20, &probe), 640 << 20);
    }

    #[test]
    fn failed_container_usage_never_falls_back_to_host_headroom() {
        let fixture = Fixture::new();
        fixture.write("cgroup/app/memory.max", "1073741824\n");
        let budget = MemoryBudget::new(8 << 30);
        for current in [None, Some("-1"), Some("invalid")] {
            if let Some(current) = current {
                fixture.write("cgroup/app/memory.current", current);
            }
            let probe = fixture.probe();
            assert_eq!(probe.summary.host_available_bytes, Some(12 << 30));
            assert_eq!(probe.summary.cgroup_limit_bytes, Some(1 << 30));
            assert_eq!(probe.summary.available_bytes, None);
            assert_eq!(budget.cache_limit_for_probe(128 << 20, &probe), 0);
        }
        fixture.write("cgroup/app/memory.max", "invalid");
        assert_eq!(budget.cache_limit_for_probe(128 << 20, &fixture.probe()), 0);
        fixture.limit("app", 1 << 30, 256 << 20);
        assert_eq!(
            budget.cache_limit_for_probe(128 << 20, &fixture.probe()),
            640 << 20
        );
    }

    #[test]
    fn mounted_subtrees_and_namespace_relative_paths_resolve_the_same_limits() {
        let fixture = Fixture::new();
        fixture.limit("app", 1 << 30, 256 << 20);
        let mount = fixture.root.join("cgroup/app");
        fixture.write(
            "proc/self/mountinfo",
            &format!(
                "1 0 0:1 /tenant/app {} ro - cgroup2 cgroup rw\n",
                mount.display()
            ),
        );
        for group in ["/tenant/app/worker", "/worker", "/"] {
            fixture.write("proc/self/cgroup", &format!("0::{group}\n"));
            let probe = fixture.probe();
            assert_eq!(probe.summary.cgroup_limit_bytes, Some(1 << 30));
            assert_eq!(probe.summary.available_bytes, Some(768 << 20));
            assert!(!probe.cgroup_unreadable);
        }
    }

    #[test]
    fn mount_paths_are_unescaped_and_never_walk_outside_the_mount() {
        let fixture = Fixture::new();
        fixture.write("mount with space/memory.max", "1073741824");
        fixture.write("mount with space/memory.current", "268435456");
        let mount = fixture
            .root
            .join("mount with space")
            .display()
            .to_string()
            .replace(' ', "\\040");
        fixture.write(
            "proc/self/mountinfo",
            &format!("1 0 0:1 / {} ro - cgroup2 cgroup rw\n", mount),
        );
        fixture.write("proc/self/cgroup", "0::/../../outside\n");
        assert_eq!(fixture.probe().summary.cgroup_limit_bytes, Some(1 << 30));
        assert_eq!(unescape_mount("/a\\134b"), Some("/a\\b".to_owned()));
        assert_eq!(absolute_path("/../outside"), None);
    }

    #[test]
    fn kernel_units_invalid_values_and_overflow_are_checked() {
        assert_eq!(
            kib_field("MemAvailable: 2 kB\n", "MemAvailable:"),
            Some(2048)
        );
        for text in [
            "MemAvailable: -1 kB",
            "MemAvailable: 2 MB",
            "MemAvailable: 18446744073709551615 kB",
            "MemAvailable: invalid kB",
        ] {
            assert_eq!(kib_field(text, "MemAvailable:"), None);
        }
        assert_eq!(parse_bytes("max"), None);
        assert_eq!(parse_bytes("-1"), None);
        assert_eq!(parse_bytes("0\n"), Some(0));
    }
}
