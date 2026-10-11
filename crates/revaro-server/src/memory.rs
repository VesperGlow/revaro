use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct MemoryBudget {
    pub total_bytes: i64,
    pub cache_bytes: i64,
    pub database_bytes: i64,
    pub mmap_bytes: i64,
}

impl MemoryBudget {
    pub fn detect(requested: i64) -> Self {
        let physical = meminfo("MemTotal:").unwrap_or(requested);
        let container = cgroup_directories()
            .iter()
            .filter_map(|directory| read_number(&directory.join("memory.max")))
            .filter(|limit| *limit > 0)
            .min()
            .unwrap_or(requested);
        Self::constrained(requested, physical, container)
    }

    fn constrained(requested: i64, physical: i64, container: i64) -> Self {
        Self::new(requested.min(physical).min(container))
    }

    pub fn new(total_bytes: i64) -> Self {
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
        let available = cgroup_directories()
            .iter()
            .filter_map(|directory| {
                let current = read_number(&directory.join("memory.current"))?;
                let limit = read_number(&directory.join("memory.max"))?;
                if limit <= 0 {
                    return None;
                }
                let inactive =
                    read_field(&directory.join("memory.stat"), "inactive_file").unwrap_or(0);
                Some(limit.saturating_sub(current.saturating_sub(inactive)))
            })
            .chain(meminfo("MemAvailable:"))
            .min();
        let usage = read_field(Path::new("/proc/self/status"), "VmRSS:")
            .map(|value| value.saturating_mul(1024))
            .unwrap_or(0);
        self.cache_limit(cached, usage, available)
    }
}

fn meminfo(field: &str) -> Option<i64> {
    read_field(Path::new("/proc/meminfo"), field).map(|value| value.saturating_mul(1024))
}

fn read_field(path: &Path, field: &str) -> Option<i64> {
    let contents = std::fs::read_to_string(path).ok()?;
    contents.lines().find_map(|line| {
        let mut values = line.split_whitespace();
        (values.next()? == field)
            .then(|| values.next()?.parse().ok())
            .flatten()
    })
}

fn read_number(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn cgroup_directories() -> Vec<PathBuf> {
    let root = Path::new("/sys/fs/cgroup");
    let mut directories = vec![root.to_path_buf()];
    if let Ok(contents) = std::fs::read_to_string("/proc/self/cgroup")
        && let Some(path) = contents.lines().find_map(|line| line.strip_prefix("0::"))
    {
        let mut directory = root.join(path.trim_start_matches('/'));
        while directory.starts_with(root) && directory != root {
            directories.push(directory.clone());
            if !directory.pop() {
                break;
            }
        }
    }
    directories
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
