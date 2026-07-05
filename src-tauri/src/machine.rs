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
    Components, CpuRefreshKind, MemoryRefreshKind, ProcessRefreshKind, ProcessesToUpdate,
    RefreshKind, System,
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

/// A readable temperature sensor. Only sensors that report a finite,
/// physically-plausible value are surfaced (capability honesty).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineTemp {
    pub label: String,
    pub celsius: f32,
}

/// Battery health, when a battery is present + readable. `None` on desktops
/// / VMs (no battery) — never fabricated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineBattery {
    /// Charge, 0..=100.
    pub percent: f32,
    /// One of: charging | discharging | full | empty | unknown.
    pub state: String,
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
    /// Readable temperature sensors — empty when the platform exposes none
    /// (common on Windows consumer HW / VMs / containers). The UI shows an
    /// "unavailable" state rather than a fake reading.
    pub temperatures: Vec<MachineTemp>,
    /// Battery, or `None` when there's no battery.
    pub battery: Option<MachineBattery>,
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
        temperatures: collect_temperatures(),
        battery: collect_battery(),
        collected_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// Read temperature sensors via `sysinfo` Components (Linux hwmon / Windows
/// WMI / macOS SMC). Returns only sensors reporting a finite, plausible
/// value — an empty vec means "the platform exposes no readable temps here"
/// (frequent on Windows consumer HW / VMs), which the UI shows honestly
/// rather than inventing a number.
fn collect_temperatures() -> Vec<MachineTemp> {
    let comps = Components::new_with_refreshed_list();
    let mut out: Vec<MachineTemp> = comps
        .iter()
        .filter_map(|c| {
            let t = c.temperature();
            if t.is_finite() && (-40.0..=150.0).contains(&t) {
                Some(MachineTemp {
                    label: c.label().to_string(),
                    celsius: t,
                })
            } else {
                None
            }
        })
        .collect();
    // Hottest first; stable label tiebreak so the list doesn't jitter.
    out.sort_by(|a, b| {
        b.celsius
            .partial_cmp(&a.celsius)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.label.cmp(&b.label))
    });
    out
}

/// Read the primary battery via `starship-battery`. `None` when there's no
/// battery or the platform can't read one (desktops, VMs, CI runners) —
/// never fabricated.
fn collect_battery() -> Option<MachineBattery> {
    let manager = starship_battery::Manager::new().ok()?;
    let bat = manager.batteries().ok()?.next()?.ok()?;
    let percent = clamp_pct(bat.state_of_charge().value * 100.0);
    let state = match bat.state() {
        starship_battery::State::Charging => "charging",
        starship_battery::State::Discharging => "discharging",
        starship_battery::State::Full => "full",
        starship_battery::State::Empty => "empty",
        _ => "unknown", // Unknown + any future #[non_exhaustive] variant
    }
    .to_string();
    Some(MachineBattery { percent, state })
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
        // Sensors are capability-gated — may be empty on CI/VMs. When
        // present, temps are finite + plausible and battery % is in range.
        for tp in &s.temperatures {
            assert!(
                tp.celsius.is_finite() && (-40.0..=150.0).contains(&tp.celsius),
                "temp {} out of range",
                tp.celsius
            );
        }
        if let Some(b) = &s.battery {
            assert!((0.0..=100.0).contains(&b.percent), "battery {}", b.percent);
            assert!(!b.state.is_empty());
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
