//! Live sessions collector — enumerate OS processes for the 26 supported
//! AI CLI tools and report them with CPU / memory / project context.
//!
//! Ported from Python `helper/system_collector.py`. Provider regex patterns
//! match exactly — keep them in sync when new providers land.
//!
//! Output shape matches `helper_sync`'s `p_sessions` array:
//!
//! ```json
//! {
//!   "id": "proc-12345",
//!   "name": "claude --project foo/",
//!   "provider": "Claude",
//!   "project": "foo",
//!   "project_hash": null,
//!   "status": "Running",
//!   "total_usage": 1500,
//!   "exact_cost": null,
//!   "requests": 3,
//!   "error_count": 0,
//!   "collection_confidence": "high",
//!   "started_at": "2026-04-24T12:34:56Z",
//!   "last_active_at": "2026-04-24T13:02:10Z"
//! }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

/// (provider_name, regex, confidence) — lower-cased command line is matched.
type ProviderPattern = (&'static str, &'static str, Confidence);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    fn as_str(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

static PROVIDER_PATTERNS: &[ProviderPattern] = &[
    ("Codex", r"\bcodex\b", Confidence::High),
    ("Codex", r"\bopenai\b", Confidence::Medium),
    ("Gemini", r"\bgemini\b", Confidence::High),
    ("Gemini", r"\bgoogle-generativeai\b", Confidence::Medium),
    ("Claude", r"\bclaude\b", Confidence::High),
    ("Cursor", r"\bcursor\b", Confidence::High),
    ("OpenCode", r"\bopencode\b", Confidence::High),
    ("Droid", r"\bdroid\b", Confidence::Low),
    ("Antigravity", r"\bantigravity\b", Confidence::High),
    (
        "Copilot",
        r"\bcopilot\b|\bgithub.copilot\b",
        Confidence::High,
    ),
    ("z.ai", r"\bz\.ai\b|\bzai\b", Confidence::High),
    ("MiniMax", r"\bminimax\b", Confidence::High),
    ("Augment", r"\baugment\b", Confidence::Medium),
    (
        "JetBrains AI",
        r"\bjetbrains[\s-]?ai\b|\bjbai\b",
        Confidence::High,
    ),
    ("Kimi K2", r"\bkimi[\s_-]*k2\b", Confidence::High),
    ("Kimi", r"\bkimi\b", Confidence::Medium),
    ("Amp", r"\bamp\b", Confidence::Low),
    ("Synthetic", r"\bsynthetic\b", Confidence::Medium),
    ("Warp", r"\bwarp\b", Confidence::Medium),
    ("Kilo", r"\bkilo\b|\bkilo[_-]?code\b", Confidence::High),
    ("Ollama", r"\bollama\b", Confidence::High),
    ("OpenRouter", r"\bopenrouter\b", Confidence::High),
    (
        "Alibaba",
        r"\balibaba\b|\bqwen\b|\btongyi\b",
        Confidence::High,
    ),
    ("Kiro", r"\bkiro\b", Confidence::High),
    ("Vertex AI", r"\bvertex[\s_-]?ai\b", Confidence::High),
    ("Perplexity", r"\bperplexity\b", Confidence::High),
    (
        "Volcano Engine",
        r"\bvolcano[\s_-]?engine\b|\bvolcengine\b",
        Confidence::High,
    ),
];

static IGNORED_PATTERNS: &[&str] = &[
    r"crashpad",
    r"--type=renderer",
    r"--type=gpu-process",
    r"--utility-sub-type",
    r"codex helper",
    r"electron framework",
    r"\.vscode-server",
    r"--ms-enable-electron",
    r"node_modules/\.bin",
];

static COMPILED_PROVIDERS: Lazy<Vec<(&'static str, Regex, Confidence)>> = Lazy::new(|| {
    PROVIDER_PATTERNS
        .iter()
        .map(|(p, pat, c)| (*p, Regex::new(pat).expect("valid provider regex"), *c))
        .collect()
});

static COMPILED_IGNORED: Lazy<Vec<Regex>> = Lazy::new(|| {
    IGNORED_PATTERNS
        .iter()
        .map(|p| Regex::new(p).expect("valid ignore regex"))
        .collect()
});

static PATH_EXTRACT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?:[A-Z]:\\|/(?:Users|home|opt|var|tmp|srv))[^\s"']+"#).unwrap());

/// Payload shape sent to `helper_sync.p_sessions`. Fields mirror the
/// Python collector output exactly (see `CollectedSession`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSession {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub project: String,
    pub status: String, // "Running"
    pub total_usage: i64,
    pub exact_cost: Option<f64>,
    pub requests: i64,
    pub error_count: i64,
    pub collection_confidence: String,
    pub started_at: String,
    pub last_active_at: String,

    // CPU kept for the local Sessions tab UI; stripped before helper_sync
    #[serde(skip_serializing)]
    pub cpu_usage: f32,
    #[serde(skip_serializing)]
    pub memory_mb: u64,
    #[serde(skip_serializing)]
    pub pids: Vec<u32>,
    #[serde(skip_serializing)]
    pub command: String,
}

/// One snapshot of the running AI CLI processes on this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsSnapshot {
    pub sessions: Vec<LiveSession>,
    pub total_processes_seen: usize,
    pub matched_before_dedup: usize,
    pub collected_at: String,
}

pub fn collect_sessions() -> SessionsSnapshot {
    let mut sys = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::everything()),
    );
    // sysinfo requires two refreshes to compute a usable CPU% delta.
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );
    std::thread::sleep(std::time::Duration::from_millis(250));
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );

    let now = Utc::now();
    let mut raw: Vec<LiveSession> = Vec::new();
    let total_processes_seen = sys.processes().len();

    for (pid, proc) in sys.processes() {
        let cmdline = proc
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let command = if cmdline.is_empty() {
            proc.name().to_string_lossy().to_string()
        } else {
            cmdline
        };
        if command.is_empty() {
            continue;
        }
        let lowered = command.to_lowercase();
        if COMPILED_IGNORED.iter().any(|r| r.is_match(&lowered)) {
            continue;
        }
        let (provider, confidence) = match detect_provider(&lowered) {
            Some(m) => m,
            None => continue,
        };

        let cwd = proc.cwd().map(PathBuf::from);
        let project = guess_project(&command, cwd.as_deref());

        let started_at = DateTime::<Utc>::from_timestamp(proc.start_time() as i64, 0)
            .unwrap_or(now)
            .to_rfc3339();
        let cpu = proc.cpu_usage();
        let elapsed_secs = now
            .signed_duration_since(
                DateTime::<Utc>::from_timestamp(proc.start_time() as i64, 0).unwrap_or(now),
            )
            .num_seconds()
            .max(1);

        // Heuristics ported verbatim from Python helper
        let total_usage =
            (500i64).max((elapsed_secs as f64 * (1.5f64.max(cpu as f64 + 1.0))) as i64);
        let requests = (1i64).max(elapsed_secs / 45);

        raw.push(LiveSession {
            id: format!("proc-{}", pid.as_u32()),
            name: pretty_name(&command),
            provider: provider.to_string(),
            project,
            status: "Running".to_string(),
            total_usage,
            exact_cost: None,
            requests,
            error_count: 0,
            collection_confidence: confidence.as_str().to_string(),
            started_at,
            last_active_at: now.to_rfc3339(),
            cpu_usage: cpu,
            memory_mb: proc.memory() / 1024 / 1024,
            pids: vec![pid.as_u32()],
            command,
        });
    }
    let matched_before_dedup = raw.len();

    let mut merged = deduplicate(raw);
    merged.sort_by(|a, b| {
        b.cpu_usage
            .partial_cmp(&a.cpu_usage)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.last_active_at.cmp(&a.last_active_at))
    });
    merged.truncate(12);

    SessionsSnapshot {
        sessions: merged,
        total_processes_seen,
        matched_before_dedup,
        collected_at: now.to_rfc3339(),
    }
}

fn detect_provider(lowered: &str) -> Option<(&'static str, Confidence)> {
    for (name, re, conf) in COMPILED_PROVIDERS.iter() {
        if re.is_match(lowered) {
            return Some((*name, *conf));
        }
    }
    None
}

fn pretty_name(command: &str) -> String {
    let compact = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 48 {
        compact
    } else {
        let truncated: String = compact.chars().take(45).collect();
        format!("{truncated}...")
    }
}

/// Path components that are NEVER a project name — they're OS / toolchain
/// locations that happen to appear in process cmdlines because that's
/// where binaries live. Without this filter, Homebrew nodes and Claude
/// Desktop helpers show up as projects named "Cellar" / "Library".
///
/// Keep this list conservative: some of these ARE legitimate directory
/// names users pick (e.g. "node_modules") but they're never a *project*
/// root, so filtering them out of the project-name guess is correct.
const NON_PROJECT_COMPONENTS: &[&str] = &[
    "Library",      // ~/Library/*, /Library/*
    "Application",  // truncated "Application Support" (path regex stops at space)
    "Applications", // /Applications/*.app
    "Cellar",       // /opt/homebrew/Cellar/*
    "homebrew",     // /opt/homebrew/*
    "node_modules", // ...
    ".nvm",
    ".cargo",
    ".rustup",
    ".npm",
    ".pnpm-store",
    ".yarn",
    "Program Files", // C:\Program Files\*
    "Program Files (x86)",
    "AppData", // Windows user appdata
    "ProgramData",
    "System32",
    "WindowsApps",
    "snap", // /snap/*
    "flatpak",
    "local", // catches /usr/local/*; we want the next segment
    "bin",
    "sbin",
    "lib",
    "lib64",
    "share",
    "etc",
];

fn is_non_project_component(part: &str) -> bool {
    NON_PROJECT_COMPONENTS
        .iter()
        .any(|&n| n.eq_ignore_ascii_case(part))
}

/// Find a project name from (in order):
/// 1. Process CWD (walk up looking for a marker file like `.git` /
///    `Cargo.toml` / `package.json` etc.).
/// 2. Path components extracted from the cmdline, after filtering out
///    filesystem / toolchain locations that are never project roots
///    (Library, Cellar, node_modules, Program Files, ...). This prevents
///    `/opt/homebrew/Cellar/node/25.9.0/bin/node` → `"Cellar"` etc.
/// 3. "local-workspace" as a final fallback.
fn guess_project(command: &str, cwd: Option<&Path>) -> String {
    if let Some(dir) = cwd {
        if let Some(root) = find_project_root(dir) {
            if let Some(name) = root.file_name() {
                let s = name.to_string_lossy().into_owned();
                if !is_non_project_component(&s) {
                    return s;
                }
            }
        }
    }
    for m in PATH_EXTRACT.find_iter(command) {
        let p = Path::new(m.as_str());
        // Filter out root-level dirs (Users/home/opt/...) AND known
        // non-project components (Library/Cellar/...). Keep the rest
        // in original order, mirror Python's `parts[1]` choice — on
        // macOS/Linux this means "take the component right after the
        // username", which is almost always the project's parent
        // directory ("code", "Documents", "Projects", etc.) or the
        // project itself for flat layouts.
        let parts: Vec<String> = p
            .iter()
            .filter_map(|s| {
                let s = s.to_string_lossy();
                let rooty = matches!(
                    s.as_ref(),
                    "/" | "Users" | "home" | "opt" | "var" | "tmp" | "srv"
                );
                if rooty || is_non_project_component(&s) {
                    None
                } else {
                    Some(s.into_owned())
                }
            })
            .collect();
        if parts.len() >= 2 {
            return parts[1].clone();
        }
        if let Some(only) = parts.first() {
            return only.clone();
        }
    }
    "local-workspace".to_string()
}

const PROJECT_MARKERS: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "Makefile",
    "CMakeLists.txt",
    ".git",
];

fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start.to_path_buf());
    while let Some(dir) = current {
        // Stop at system roots
        let s = dir.to_string_lossy();
        if matches!(
            s.as_ref(),
            "/" | "/Users" | "/home" | "/opt" | "/var" | "/tmp" | "/srv"
        ) {
            return None;
        }
        for marker in PROJECT_MARKERS {
            if dir.join(marker).exists() {
                return Some(dir);
            }
        }
        current = dir.parent().map(|p| p.to_path_buf());
    }
    None
}

/// Merge sessions with the same (provider, project) — parent + children
/// of the same CLI tool should appear as one row.
fn deduplicate(sessions: Vec<LiveSession>) -> Vec<LiveSession> {
    let mut groups: HashMap<(String, String), Vec<LiveSession>> = HashMap::new();
    for s in sessions {
        groups
            .entry((s.provider.clone(), s.project.clone()))
            .or_default()
            .push(s);
    }

    let mut merged = Vec::with_capacity(groups.len());
    for ((_provider, _project), mut group) in groups {
        if group.len() == 1 {
            merged.push(group.pop().unwrap());
            continue;
        }
        group.sort_by_key(|s| {
            std::cmp::Reverse(match s.collection_confidence.as_str() {
                "high" => 3u8,
                "medium" => 2u8,
                _ => 1u8,
            })
        });
        let mut primary = group.remove(0);
        let worker_count = group.len();
        let mut total_usage = primary.total_usage;
        let mut total_requests = primary.requests;
        let mut total_errors = primary.error_count;
        let mut total_cpu = primary.cpu_usage;
        let mut total_mem = primary.memory_mb;
        for child in &group {
            total_usage += child.total_usage;
            total_requests += child.requests;
            total_errors += child.error_count;
            total_cpu += child.cpu_usage;
            total_mem += child.memory_mb;
            primary.pids.extend_from_slice(&child.pids);
            if child.started_at < primary.started_at {
                primary.started_at = child.started_at.clone();
            }
            if child.last_active_at > primary.last_active_at {
                primary.last_active_at = child.last_active_at.clone();
            }
        }
        primary.total_usage = total_usage;
        primary.requests = total_requests;
        primary.error_count = total_errors;
        primary.cpu_usage = total_cpu;
        primary.memory_mb = total_mem;
        if worker_count > 0 {
            primary.name = format!("{} (+{} workers)", primary.name, worker_count);
        }
        merged.push(primary);
    }
    merged
}

// Used by helper_sync payload — a stripped-down JSON with only server
// fields (omits CPU / memory / pids which are UI-only).
pub fn sessions_payload(snapshot: &SessionsSnapshot) -> serde_json::Value {
    serde_json::to_value(&snapshot.sessions).unwrap_or(serde_json::json!([]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_claude_high_confidence() {
        let (p, c) = detect_provider("/usr/local/bin/claude --project foo").unwrap();
        assert_eq!(p, "Claude");
        assert_eq!(c, Confidence::High);
    }

    #[test]
    fn detect_codex_high_confidence() {
        let (p, c) = detect_provider("node /opt/codex/bin/codex exec -").unwrap();
        assert_eq!(p, "Codex");
        assert_eq!(c, Confidence::High);
    }

    #[test]
    fn ignored_electron_helper_skipped_by_caller() {
        let lowered = "electron framework helper --type=renderer";
        let ignore_hit = COMPILED_IGNORED.iter().any(|r| r.is_match(lowered));
        assert!(ignore_hit);
    }

    #[test]
    fn pretty_name_truncates_long_commands() {
        let long = "a".repeat(100);
        let p = pretty_name(&long);
        assert!(p.ends_with("..."));
        assert_eq!(p.chars().count(), 48);
    }

    #[test]
    fn non_project_components_are_case_insensitive() {
        assert!(is_non_project_component("Library"));
        assert!(is_non_project_component("library"));
        assert!(is_non_project_component("CELLAR"));
        assert!(is_non_project_component("node_modules"));
        assert!(is_non_project_component("Program Files"));
        assert!(!is_non_project_component("my-project"));
        assert!(!is_non_project_component("cli-pulse"));
    }

    #[test]
    fn guess_project_skips_homebrew_cellar() {
        // Previously returned "Cellar" — a Homebrew internal dir, never a project.
        let cmd = "/opt/homebrew/Cellar/node/25.9.0_2/bin/node /some/script.js";
        let got = guess_project(cmd, None);
        // After filtering "Cellar" and the root, parts = ["node", "25.9.0_2", "script.js"]
        // parts[1] = "25.9.0_2" — a version string, still noise but at least
        // not the toolchain-owner name. Future work: also filter version-looking
        // components. For Sprint 5 we accept this as a strict improvement.
        assert_ne!(got, "Cellar");
    }

    #[test]
    fn guess_project_skips_claude_helpers_library() {
        // The old logic returned "Library" because `/Users/jason/Library/...`
        // has "Library" at parts[1].
        let cmd = "/Users/jason/Library/Application Support/Claude/helper";
        let got = guess_project(cmd, None);
        assert_ne!(got, "Library");
    }

    #[test]
    fn guess_project_takes_user_folder_structure() {
        // Common layout: /Users/<name>/code/<project>/...
        let cmd = "claude /Users/jason/code/my-cool-app/src/main.ts";
        let got = guess_project(cmd, None);
        // After filter: ["jason", "code", "my-cool-app", "src", "main.ts"]
        // parts[1] = "code" — matches Python helper's behavior
        assert_eq!(got, "code");
    }

    #[test]
    fn guess_project_windows_program_files_filtered() {
        let cmd = "C:\\Program Files\\MyApp\\bin\\tool.exe --run";
        let got = guess_project(cmd, None);
        // "Program Files" and "bin" both filtered
        assert_ne!(got, "Program Files");
        assert_ne!(got, "bin");
    }

    #[test]
    fn collect_sessions_runs_and_returns_snapshot() {
        // Smoke — real machine may or may not have CLI tools running, so
        // just assert the shape. The `scan_cli` binary and CI exercise real
        // data paths.
        let snap = collect_sessions();
        assert!(snap.total_processes_seen > 0);
        // sessions list may be empty on a CI runner with no AI CLI running.
        let _ = snap.collected_at;
    }
}
