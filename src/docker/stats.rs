//! Live per-container resource usage.
//!
//! Docker's stats endpoint is one long-lived stream per container, so the
//! manager keeps a task per running container and diffs against the latest
//! container list: new containers get a stream, departed ones get aborted.

use std::collections::HashMap;

use bollard::models::ContainerStatsResponse;
use bollard::query_parameters::StatsOptions;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::model::ContainerRow;
use crate::docker::Client;
use crate::event::AppEvent;

/// One reading, already reduced to the numbers the UI shows.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatSample {
    pub cpu_percent: f64,
    pub mem_bytes: u64,
    pub mem_limit: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub block_read: u64,
    pub block_write: u64,
    pub pids: u64,
}

impl StatSample {
    pub fn mem_percent(&self) -> Option<f64> {
        if self.mem_limit == 0 {
            None
        } else {
            Some(self.mem_bytes as f64 / self.mem_limit as f64 * 100.0)
        }
    }
}

/// Owns one streaming task per running container.
#[derive(Default)]
pub struct StatsManager {
    tasks: HashMap<String, JoinHandle<()>>,
}

impl StatsManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconcile the running set of stream tasks with the current container
    /// list. Cheap to call on every refresh.
    pub fn sync(
        &mut self,
        client: &Client,
        containers: &[ContainerRow],
        tx: &mpsc::UnboundedSender<AppEvent>,
    ) {
        let wanted: Vec<&ContainerRow> =
            containers.iter().filter(|c| c.state.is_running()).collect();

        // Drop streams for containers that stopped or disappeared.
        let wanted_ids: std::collections::HashSet<&str> =
            wanted.iter().map(|c| c.id.as_str()).collect();
        self.tasks.retain(|id, handle| {
            if wanted_ids.contains(id.as_str()) && !handle.is_finished() {
                true
            } else {
                handle.abort();
                false
            }
        });

        // Start streams for anything new.
        for c in wanted {
            if self.tasks.contains_key(&c.id) {
                continue;
            }
            let handle = tokio::spawn(stream_one(client.clone(), c.id.clone(), tx.clone()));
            self.tasks.insert(c.id.clone(), handle);
        }
    }

    /// Abort every stream. Used when the daemon connection drops so we don't
    /// keep a pile of dead tasks retrying.
    pub fn clear(&mut self) {
        for (_, handle) in self.tasks.drain() {
            handle.abort();
        }
    }
}

impl Drop for StatsManager {
    fn drop(&mut self) {
        self.clear();
    }
}

async fn stream_one(client: Client, id: String, tx: mpsc::UnboundedSender<AppEvent>) {
    let options = StatsOptions {
        stream: true,
        one_shot: false,
    };
    let mut stream = client.docker.stats(&id, Some(options));

    while let Some(item) = stream.next().await {
        let Ok(raw) = item else { break };
        // The very first frame of a stream has no `precpu` baseline, so any CPU
        // number derived from it is meaningless. `reduce` returns None there.
        let Some(sample) = reduce(&raw) else { continue };
        if tx
            .send(AppEvent::Stat {
                id: id.clone(),
                sample,
            })
            .is_err()
        {
            return;
        }
    }
}

/// Turn a raw stats frame into a [`StatSample`], mirroring how the Docker CLI
/// computes the same numbers.
fn reduce(raw: &ContainerStatsResponse) -> Option<StatSample> {
    let cpu = raw.cpu_stats.as_ref()?;
    let precpu = raw.precpu_stats.as_ref()?;

    let total = cpu
        .cpu_usage
        .as_ref()
        .and_then(|u| u.total_usage)
        .unwrap_or(0);
    let pre_total = precpu
        .cpu_usage
        .as_ref()
        .and_then(|u| u.total_usage)
        .unwrap_or(0);
    let system = cpu.system_cpu_usage.unwrap_or(0);
    let pre_system = precpu.system_cpu_usage.unwrap_or(0);

    // No baseline yet — skip rather than reporting a false spike.
    if pre_system == 0 || system <= pre_system || total < pre_total {
        return None;
    }

    let cpu_delta = (total - pre_total) as f64;
    let system_delta = (system - pre_system) as f64;
    let cores = cpu
        .online_cpus
        .map(|n| n as usize)
        .or_else(|| {
            cpu.cpu_usage
                .as_ref()
                .and_then(|u| u.percpu_usage.as_ref())
                .map(Vec::len)
        })
        .filter(|n| *n > 0)
        .unwrap_or(1);

    let cpu_percent = cpu_delta / system_delta * cores as f64 * 100.0;

    let (mem_bytes, mem_limit) = raw
        .memory_stats
        .as_ref()
        .map(|m| {
            let usage = m.usage.unwrap_or(0);
            // Page cache is charged to the container but isn't really "in use";
            // subtract it the way `docker stats` does. cgroup v2 calls it
            // `inactive_file`, v1 `total_inactive_file`.
            let cache = m
                .stats
                .as_ref()
                .and_then(|s| {
                    s.get("inactive_file")
                        .or_else(|| s.get("total_inactive_file"))
                })
                .copied()
                .unwrap_or(0);
            (usage.saturating_sub(cache), m.limit.unwrap_or(0))
        })
        .unwrap_or((0, 0));

    let (net_rx, net_tx) = raw
        .networks
        .as_ref()
        .map(|nets| {
            nets.values().fold((0u64, 0u64), |(rx, tx), n| {
                (rx + n.rx_bytes.unwrap_or(0), tx + n.tx_bytes.unwrap_or(0))
            })
        })
        .unwrap_or((0, 0));

    let (block_read, block_write) = raw
        .blkio_stats
        .as_ref()
        .and_then(|b| b.io_service_bytes_recursive.as_ref())
        .map(|entries| {
            entries.iter().fold((0u64, 0u64), |(r, w), e| {
                let v = e.value.unwrap_or(0);
                match e.op.as_deref().map(str::to_ascii_lowercase).as_deref() {
                    Some("read") => (r + v, w),
                    Some("write") => (r, w + v),
                    _ => (r, w),
                }
            })
        })
        .unwrap_or((0, 0));

    Some(StatSample {
        cpu_percent,
        mem_bytes,
        mem_limit,
        net_rx,
        net_tx,
        block_read,
        block_write,
        pids: raw.pids_stats.as_ref().and_then(|p| p.current).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerCpuStats, ContainerCpuUsage, ContainerMemoryStats};

    fn cpu(total: u64, system: u64) -> ContainerCpuStats {
        ContainerCpuStats {
            cpu_usage: Some(ContainerCpuUsage {
                total_usage: Some(total),
                percpu_usage: None,
                usage_in_kernelmode: None,
                usage_in_usermode: None,
            }),
            system_cpu_usage: Some(system),
            online_cpus: Some(4),
            throttling_data: None,
        }
    }

    fn frame(cpu_stats: ContainerCpuStats, precpu: ContainerCpuStats) -> ContainerStatsResponse {
        ContainerStatsResponse {
            cpu_stats: Some(cpu_stats),
            precpu_stats: Some(precpu),
            memory_stats: Some(ContainerMemoryStats {
                usage: Some(200),
                stats: Some([("inactive_file".to_string(), 50u64)].into_iter().collect()),
                limit: Some(1000),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn first_frame_is_skipped() {
        // precpu system usage of 0 means there's no baseline.
        let f = frame(cpu(100, 1000), cpu(0, 0));
        assert!(reduce(&f).is_none());
    }

    #[test]
    fn cpu_percent_matches_docker_formula() {
        // 10% of one core across 4 cores => 40%.
        let f = frame(cpu(1_100, 11_000), cpu(1_000, 10_000));
        let s = reduce(&f).unwrap();
        assert!(
            (s.cpu_percent - 40.0).abs() < 0.001,
            "got {}",
            s.cpu_percent
        );
    }

    #[test]
    fn page_cache_is_excluded_from_memory() {
        let f = frame(cpu(1_100, 11_000), cpu(1_000, 10_000));
        let s = reduce(&f).unwrap();
        assert_eq!(s.mem_bytes, 150);
        assert_eq!(s.mem_limit, 1000);
        assert!((s.mem_percent().unwrap() - 15.0).abs() < 0.001);
    }
}
