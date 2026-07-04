//! System Monitor — "Machine" tab data (LOCAL only; nothing is synced).
//!
//! Cross-platform machine-health snapshot for the local "Machine" tab:
//! whole-machine CPU% + memory + a ranked top-N process table, all via
//! `sysinfo` (already a dep; no privileges, no new crates).
//!
//! First-principles port of the Mac v1.38 System Monitor Phase 1 UI. The
//! Mac reads Apple-only SMC / IOReport / HID for temps / fans / power; the
//! portable subset that Windows AND Linux can *always* read without
//! privileges is CPU / memory / per-process. That lands here. Temperatures
//! and battery (sysinfo `Components` / a battery crate) are a
//! capability-gated follow-up — and this module deliberately reports only
//! what the platform can truthfully read (no fabricated sensor values).
//!
//! Per-process rows NEVER leave the device (privacy + volume) — there is
//! no Supabase write on this path, unlike `sessions` / heartbeat.

use serde::{Deserialize, Serialize};
use sysinfo::{
    CpuRefreshKind, MemoryRefreshKind, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System,
};

/// Default number of top processes surfaced to the UI.
pub const DEFAULT_TOP_N: usize = 12;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineProcess {
    pub pid: u32,
    pub name: String,
    /// Share of TOTAL machine CPU, 0..=100 (sysinfo's per-core % divided by
    /// core count) — same scale as `cpu_percent` so the gauge and the table
    /// agree, matching the Mac plan's "total ≤ 100%" convention.
    pub cpu_percent: f32,
    pub mem_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineSnapshot {
    /// Whole-machine CPU utilisation, 0..=100.
    pub cpu_percent: f32,
    pub cpu_core_count: usize,
    pub mem_total_bytes: u64,
    pub mem_used_bytes: u64,
    /// used / total × 100, 0..=100 (0 when total is 0).
    pub mem_percent: f32,
    pub process_count: usize,
    pub top_processes: Vec<MachineProcess>,
    pub collected_at: String,
}

pub fn collect_machine_snapshot() -> MachineSnapshot {
    collect_machine_snapshot_top_n(DEFAULT_TOP_N)
}

pub fn collect_machine_snapshot_top_n(top_n: usize) -> MachineSnapshot {
    let mut sys = System::new_with_specifics(
        RefreshKind::new()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything())
            .with_processes(ProcessRefreshKind::everything()),
    );
    // sysinfo needs two samples separated by >= MINIMUM_CPU_UPDATE_INTERVAL
    // for a usable CPU% delta (both global and per-process).
    sys.refresh_cpu_specifics(CpuRefreshKind::everything());
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );
    std::thread::sleep(std::time::Duration::from_millis(250));
    sys.refresh_cpu_specifics(CpuRefreshKind::everything());
    sys.refresh_memory();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );

    let core_count = sys.cpus().len().max(1);
    let cpu_percent = clamp_pct(sys.global_cpu_usage());
    let mem_total = sys.total_memory();
    let mem_used = sys.used_memory();
    let mem_percent = if mem_total > 0 {
        clamp_pct((mem_used as f64 / mem_total as f64 * 100.0) as f32)
    } else {
        0.0
    };

    let process_count = sys.processes().len();
    let mut procs: Vec<MachineProcess> = sys
        .processes()
        .iter()
        .map(|(pid, p)| MachineProcess {
            pid: pid.as_u32(),
            name: p.name().to_string_lossy().to_string(),
            cpu_percent: clamp_pct(p.cpu_usage() / core_count as f32),
            mem_bytes: p.memory(),
        })
        .collect();
    // Rank by CPU% desc, then memory desc so ties are deterministic.
    procs.sort_by(|a, b| {
        b.cpu_percent
            .partial_cmp(&a.cpu_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.mem_bytes.cmp(&a.mem_bytes))
    });
    procs.truncate(top_n);

    MachineSnapshot {
        cpu_percent,
        cpu_core_count: core_count,
        mem_total_bytes: mem_total,
        mem_used_bytes: mem_used,
        mem_percent,
        process_count,
        top_processes: procs,
        collected_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// Clamp a percentage into 0..=100 and scrub NaN / Inf → 0. Defensive: a
/// bad sysinfo sample serialized as NaN would break Tauri IPC (serde_json
/// can't encode NaN) or render `undefined` in the UI — the v0.2.11
/// white-screen lesson. Every percentage the UI sees goes through here.
fn clamp_pct(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_pct_scrubs_nan_inf_and_clamps() {
        assert_eq!(clamp_pct(f32::NAN), 0.0);
        assert_eq!(clamp_pct(f32::INFINITY), 0.0);
        assert_eq!(clamp_pct(f32::NEG_INFINITY), 0.0);
        assert_eq!(clamp_pct(-5.0), 0.0);
        assert_eq!(clamp_pct(150.0), 100.0);
        assert_eq!(clamp_pct(42.5), 42.5);
    }

    #[test]
    fn snapshot_has_sane_shape() {
        let s = collect_machine_snapshot();
        assert!(
            (0.0..=100.0).contains(&s.cpu_percent),
            "cpu {}",
            s.cpu_percent
        );
        assert!(
            (0.0..=100.0).contains(&s.mem_percent),
            "mem {}",
            s.mem_percent
        );
        assert!(s.cpu_core_count >= 1);
        // The test process itself always exists.
        assert!(s.process_count >= 1);
        assert!(s.top_processes.len() <= DEFAULT_TOP_N);
        // Every surfaced percentage is finite + in range (no NaN leaks).
        for p in &s.top_processes {
            assert!((0.0..=100.0).contains(&p.cpu_percent));
        }
        assert!(!s.collected_at.is_empty());
    }

    #[test]
    fn top_processes_ranked_by_cpu_desc() {
        let s = collect_machine_snapshot();
        for w in s.top_processes.windows(2) {
            assert!(
                w[0].cpu_percent >= w[1].cpu_percent,
                "not sorted: {} < {}",
                w[0].cpu_percent,
                w[1].cpu_percent
            );
        }
    }

    #[test]
    fn top_n_is_respected() {
        let s = collect_machine_snapshot_top_n(3);
        assert!(s.top_processes.len() <= 3);
    }
}
