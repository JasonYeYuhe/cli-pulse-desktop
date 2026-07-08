//! WSL (Windows Subsystem for Linux) usage discovery.
//!
//! Windows developers frequently run Claude Code / Codex **inside a WSL distro**,
//! where the logs live under the Linux home (`/home/<user>/.claude`, etc.) — a
//! filesystem the native Windows scanner never sees. WSL2 exposes each distro at
//! the `\\wsl.localhost\<distro>\` UNC share, so we enumerate the home dirs of
//! **running** distros and hand them to `paths::*` to scan alongside the native
//! Windows roots. Their usage then merges into the same per-provider totals.
//!
//! Design choices (mirroring `javis603/token-monitor`'s behaviour):
//! - **Running distros only** (`wsl.exe -l --running -q`) — avoids waking a
//!   stopped distro's Plan9 file server on every scan; a distro the user is
//!   actually working in is running.
//! - **Best-effort + fail-safe** — any error (no `wsl.exe`, nothing running,
//!   unreadable share) yields an empty list, so a machine without WSL (including
//!   every Linux/macOS build) is completely unaffected.
//! - We use only the `\\wsl.localhost\` prefix (not the legacy `\\wsl$\`, which
//!   aliases the same files) so a distro is never double-counted.

use std::path::PathBuf;

/// Where a scanned log file physically lives. Derived purely from the file's
/// path (WSL files sit under the `\\wsl.localhost\<distro>\` UNC share we add in
/// `paths.rs`), so it survives the incremental cache — the cache is keyed by
/// absolute path, and origin is re-derivable from that key at emit time with no
/// cache-schema change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A native Windows / macOS / Linux home directory.
    Native,
    /// Inside a running WSL distro (the captured name, e.g. "Ubuntu").
    Wsl(String),
}

/// Classify a scanned file path as native vs. a WSL distro. Pure + case- and
/// slash-tolerant. Recognizes the `\\wsl.localhost\<distro>\` share we scan (and
/// the legacy `\\wsl$\<distro>\` alias, for robustness); the first path segment
/// after the prefix is the distro name. Everything else is `Native`.
pub fn classify_origin(path: &str) -> Origin {
    // Normalize forward slashes so a UNC that came back with `/` still matches;
    // ASCII-lowercase for the case-insensitive prefix test (length-preserving,
    // so byte indices stay valid in the original-case `norm`).
    let norm = path.replace('/', "\\");
    let lower = norm.to_ascii_lowercase();
    for prefix in [r"\\wsl.localhost\", r"\\wsl$\"] {
        if let Some(idx) = lower.find(prefix) {
            let after = &norm[idx + prefix.len()..];
            let distro = after.split('\\').next().unwrap_or("").trim();
            if !distro.is_empty() {
                return Origin::Wsl(distro.to_string());
            }
        }
    }
    Origin::Native
}

/// Home directories inside running WSL distros to also scan for Claude/Codex
/// logs. Windows-only; empty everywhere else.
#[cfg(windows)]
pub fn wsl_home_roots() -> Vec<PathBuf> {
    let distros = running_distros();
    let mut homes = Vec::new();
    for distro in distros {
        let base = PathBuf::from(format!(r"\\wsl.localhost\{distro}"));
        // Regular users live under /home/<user>.
        if let Ok(entries) = std::fs::read_dir(base.join("home")) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    homes.push(p);
                }
            }
        }
        // The root account's home is /root, not /home/root.
        let root_home = base.join("root");
        if root_home.is_dir() {
            homes.push(root_home);
        }
    }
    homes
}

#[cfg(not(windows))]
pub fn wsl_home_roots() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(windows)]
fn running_distros() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW — `wsl.exe` is a console app; without this it flashes a
    // console window on every scan in our GUI process.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match std::process::Command::new("wsl.exe")
        .args(["-l", "--running", "-q"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(o) if o.status.success() => parse_distro_list(&o.stdout),
        _ => Vec::new(),
    }
}

/// Parse `wsl.exe -l --running -q` stdout — which WSL emits as **UTF-16LE** — into
/// distro names, dropping the BOM, blank lines, and Docker/Rancher internal
/// distros. Pure and unit-tested; the process spawn around it is Windows-only.
pub fn parse_distro_list(stdout: &[u8]) -> Vec<String> {
    let utf16: Vec<u16> = stdout
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&utf16)
        .lines()
        .map(|l| l.trim_start_matches('\u{feff}').trim())
        .filter(|l| !l.is_empty())
        .filter(|l| !is_internal_distro(l))
        .map(String::from)
        .collect()
}

/// Docker Desktop / Rancher Desktop register hidden WSL distros that never hold
/// user AI-tool logs — skip them so we don't wake their file servers.
fn is_internal_distro(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "docker-desktop" | "docker-desktop-data" | "rancher-desktop" | "rancher-desktop-data"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `&str` as UTF-16LE bytes (what `wsl.exe` writes to stdout).
    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn parses_utf16le_names_with_bom_and_crlf() {
        let bytes = utf16le("\u{feff}Ubuntu\r\nDebian-12\r\n");
        assert_eq!(parse_distro_list(&bytes), vec!["Ubuntu", "Debian-12"]);
    }

    #[test]
    fn drops_blank_lines_and_internal_distros() {
        let bytes = utf16le("Ubuntu\ndocker-desktop\n\nDOCKER-DESKTOP-DATA\nrancher-desktop\n");
        assert_eq!(parse_distro_list(&bytes), vec!["Ubuntu"]);
    }

    #[test]
    fn empty_output_is_empty_list() {
        assert!(parse_distro_list(&[]).is_empty());
        assert!(parse_distro_list(&utf16le("\u{feff}")).is_empty());
    }

    #[test]
    fn names_with_spaces_preserved() {
        let bytes = utf16le("My Distro\nUbuntu-24.04\n");
        assert_eq!(parse_distro_list(&bytes), vec!["My Distro", "Ubuntu-24.04"]);
    }

    #[cfg(not(windows))]
    #[test]
    fn home_roots_empty_off_windows() {
        assert!(wsl_home_roots().is_empty());
    }

    #[test]
    fn classify_origin_native_paths() {
        assert_eq!(
            classify_origin(r"C:\Users\jason\.claude\projects\p\f.jsonl"),
            Origin::Native
        );
        assert_eq!(
            classify_origin("/home/jason/.codex/sessions/f.jsonl"),
            Origin::Native
        );
        assert_eq!(classify_origin(""), Origin::Native);
    }

    #[test]
    fn classify_origin_wsl_extracts_distro() {
        assert_eq!(
            classify_origin(r"\\wsl.localhost\Ubuntu\home\jason\.claude\projects\p\f.jsonl"),
            Origin::Wsl("Ubuntu".to_string())
        );
        assert_eq!(
            classify_origin(r"\\wsl.localhost\Debian-12\root\.codex\sessions\f.jsonl"),
            Origin::Wsl("Debian-12".to_string())
        );
    }

    #[test]
    fn classify_origin_is_case_and_slash_tolerant() {
        // Some APIs return the share prefix lowercased or with forward slashes.
        assert_eq!(
            classify_origin(r"\\WSL.LOCALHOST\Ubuntu\home\u\f"),
            Origin::Wsl("Ubuntu".to_string())
        );
        assert_eq!(
            classify_origin("//wsl.localhost/Ubuntu/home/u/f"),
            Origin::Wsl("Ubuntu".to_string())
        );
    }

    #[test]
    fn classify_origin_handles_legacy_wsl_dollar_and_missing_distro() {
        assert_eq!(
            classify_origin(r"\\wsl$\Alpine\home\u\f"),
            Origin::Wsl("Alpine".to_string())
        );
        // Prefix present but no distro segment → not a real WSL path → Native.
        assert_eq!(classify_origin(r"\\wsl.localhost\"), Origin::Native);
    }
}
