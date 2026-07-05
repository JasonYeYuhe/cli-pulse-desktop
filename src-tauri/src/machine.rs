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
//! Per-process rows NEVER leave the device (privacy + volume). The aggregate
//! machine-health metrics (whole-device CPU/mem via `collect_load`, and the
//! temps/battery `p_metrics` blob via `collect_sensor_metrics`) DO sync to the
//! `devices` row through the heartbeat — but only the coarse, capability-gated
//! values, never the process table.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
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

/// Build the heartbeat `p_metrics` blob (temps + battery, mapped to the v0.63
/// `device_sensors` keys) from a fresh sensor read, or `None` when the
/// platform exposes nothing syncable — in which case the heartbeat OMITS
/// `p_metrics` so the server's coalesce preserves last-known rather than
/// clobbering (the Mac's `machine_collector` discipline). Blob stays far
/// under the 8192-byte guard.
pub fn collect_sensor_metrics() -> Option<Value> {
    let temps = collect_temperatures();
    let battery = collect_battery();
    build_metrics_json(&temps, battery.as_ref())
}

/// Pure mapping (no I/O) so the label heuristics + state whitelist are
/// unit-tested without hardware. `None` when there's nothing to send.
fn build_metrics_json(temps: &[MachineTemp], battery: Option<&MachineBattery>) -> Option<Value> {
    let mut m = Map::new();
    let mut cap = Map::new();

    // Temps: heuristic label → schema key (only cpu/gpu/battery temps exist
    // in the schema; hwmon/WMI labels vary wildly). Hottest match wins.
    let cpu_t = pick_temp(
        temps,
        &[
            "cpu", "core", "package", "tctl", "tdie", "coretemp", "k10temp",
        ],
    );
    let gpu_t = pick_temp(
        temps,
        &["gpu", "edge", "junction", "amdgpu", "nouveau", "radeon"],
    );
    let bat_t = pick_temp(temps, &["batt"]);
    if let Some(t) = cpu_t {
        m.insert("cpu_temp_c".into(), json!(t));
    }
    if let Some(t) = gpu_t {
        m.insert("gpu_temp_c".into(), json!(t));
    }
    if let Some(t) = bat_t {
        m.insert("battery_temp_c".into(), json!(t));
    }
    cap.insert("cpu_temp".into(), json!(cpu_t.is_some()));
    cap.insert("gpu_temp".into(), json!(gpu_t.is_some()));

    // Battery: percent + wire-whitelisted state.
    if let Some(b) = battery {
        m.insert(
            "battery_charge_pct".into(),
            json!(b.percent.round().clamp(0.0, 100.0) as i64),
        );
        if let Some(ws) = battery_state_wire(&b.state) {
            m.insert("battery_state".into(), json!(ws));
        }
        cap.insert("battery".into(), json!(true));
    } else {
        cap.insert("battery".into(), json!(false));
    }

    // Nothing readable → omit p_metrics entirely (preserve last-known).
    if m.is_empty() {
        return None;
    }
    m.insert("capability".into(), Value::Object(cap));
    Some(Value::Object(m))
}

/// The RPC whitelists `battery_state ∈ {charging,discharging,charged,none,
/// unknown}`. Map our local vocab (`full`/`empty` from the battery crate)
/// onto it; anything unmappable → `None` (dropped, coalesce preserves).
fn battery_state_wire(state: &str) -> Option<&'static str> {
    match state {
        "charging" => Some("charging"),
        "discharging" => Some("discharging"),
        "full" => Some("charged"),
        "empty" => Some("unknown"), // rare; not a wire value, don't clobber
        "unknown" => Some("unknown"),
        _ => None,
    }
}

/// Hottest temperature whose label matches any of `patterns` (case-insensitive
/// substring), or `None`.
fn pick_temp(temps: &[MachineTemp], patterns: &[&str]) -> Option<f32> {
    temps
        .iter()
        .filter(|t| {
            let l = t.label.to_lowercase();
            patterns.iter().any(|p| l.contains(p))
        })
        .map(|t| t.celsius)
        .fold(None, |acc, c| match acc {
            Some(a) if a >= c => Some(a),
            _ => Some(c),
        })
}

/// Lightweight whole-device load for the cross-device heartbeat: global
/// CPU% + memory% as `i32` 0..=100. Cheaper than `collect_machine_snapshot`
/// (no process enumeration, no Components, no battery), but still needs the
/// two-sample CPU delay. Used by the 120s sync tick to populate
/// `devices.cpu_usage` / `memory_usage` so the user's OTHER devices can show
/// this machine's health.
pub fn collect_load() -> (i32, i32) {
    let mut sys = System::new_with_specifics(
        RefreshKind::new()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything()),
    );
    sys.refresh_cpu_specifics(CpuRefreshKind::everything());
    std::thread::sleep(std::time::Duration::from_millis(250));
    sys.refresh_cpu_specifics(CpuRefreshKind::everything());
    sys.refresh_memory();

    let cpu = clamp_pct(sys.global_cpu_usage()).round() as i32;
    let mem_total = sys.total_memory();
    let mem = if mem_total > 0 {
        clamp_pct((sys.used_memory() as f64 / mem_total as f64 * 100.0) as f32).round() as i32
    } else {
        0
    };
    (cpu.clamp(0, 100), mem.clamp(0, 100))
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

    #[test]
    fn collect_load_is_in_range() {
        let (cpu, mem) = collect_load();
        assert!((0..=100).contains(&cpu), "cpu {cpu}");
        assert!((0..=100).contains(&mem), "mem {mem}");
    }

    // ---- p_metrics sensor mapping (pure; no hardware) ----

    #[test]
    fn battery_state_maps_to_wire_whitelist() {
        assert_eq!(battery_state_wire("full"), Some("charged"));
        assert_eq!(battery_state_wire("empty"), Some("unknown"));
        assert_eq!(battery_state_wire("charging"), Some("charging"));
        assert_eq!(battery_state_wire("discharging"), Some("discharging"));
        assert_eq!(battery_state_wire("unknown"), Some("unknown"));
        assert_eq!(battery_state_wire("nonsense"), None);
    }

    #[test]
    fn pick_temp_returns_hottest_match() {
        let temps = vec![
            MachineTemp {
                label: "Core 0".into(),
                celsius: 50.0,
            },
            MachineTemp {
                label: "Package id 0".into(),
                celsius: 70.0,
            },
            MachineTemp {
                label: "acpitz".into(),
                celsius: 40.0,
            },
        ];
        assert_eq!(pick_temp(&temps, &["core", "package"]), Some(70.0));
        assert_eq!(pick_temp(&temps, &["gpu"]), None);
    }

    #[test]
    fn build_metrics_maps_temps_and_battery() {
        let temps = vec![MachineTemp {
            label: "CPU".into(),
            celsius: 55.0,
        }];
        let bat = MachineBattery {
            percent: 83.4,
            state: "full".into(),
        };
        let v = build_metrics_json(&temps, Some(&bat)).unwrap();
        assert_eq!(v["cpu_temp_c"], 55.0);
        assert_eq!(v["battery_charge_pct"], 83); // rounded to int
        assert_eq!(v["battery_state"], "charged"); // full → charged (whitelist)
        assert_eq!(v["capability"]["cpu_temp"], true);
        assert_eq!(v["capability"]["battery"], true);
        // Under the server's 8192-byte guard by a wide margin.
        assert!(serde_json::to_string(&v).unwrap().len() < 8192);
    }

    #[test]
    fn build_metrics_none_when_nothing_readable() {
        // No battery, and the only temp is an unmappable ambient sensor → None
        // (heartbeat omits p_metrics → server preserves last-known).
        let temps = vec![MachineTemp {
            label: "acpitz".into(),
            celsius: 40.0,
        }];
        assert!(build_metrics_json(&temps, None).is_none());
        assert!(build_metrics_json(&[], None).is_none());
    }
}
