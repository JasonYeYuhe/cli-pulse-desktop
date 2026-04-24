//! Diagnostic binary — enumerates AI CLI processes on this machine and
//! prints what the live sessions collector would send in `helper_sync`.
//!
//! Usage:
//!   cargo run --bin sessions_smoke

fn main() {
    let snap = cli_pulse_desktop_lib::sessions::collect_sessions();
    println!("Processes scanned:       {}", snap.total_processes_seen);
    println!("Matched (before dedup):  {}", snap.matched_before_dedup);
    println!("After dedup / truncate:  {}", snap.sessions.len());
    println!("Collected at:            {}", snap.collected_at);
    println!();

    for s in &snap.sessions {
        let short: String = s.name.chars().take(60).collect();
        println!(
            "  [{:>5.1}% cpu, {:>5} MB]  {:<14} {:<20} {}",
            s.cpu_usage, s.memory_mb, s.provider, s.project, short
        );
    }
}
