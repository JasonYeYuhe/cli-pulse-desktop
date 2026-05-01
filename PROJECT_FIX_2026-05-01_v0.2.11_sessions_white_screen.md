# PROJECT FIX — v0.2.11 — Sessions tab white-screen on Windows

**Severity:** P1
**Discovered:** 2026-05-01 by Jason during the v0.2.10 real-VM smoke test on
Azure Windows Server 2025 (`clipulse-win-test`, Japan East).
**Affects:** all v0.1.0–v0.2.10 (latent; never surfaced before because the
Windows GUI didn't actually launch until v0.2.10's `default-run` packaging
fix).
**Fix shipped in:** v0.2.11
**Yanked:** v0.2.10 stays prerelease (sessions broken). v0.2.9 stays draft.

## Symptom

Steps to reproduce on Windows:

1. Install v0.2.10 NSIS.
2. Launch `cli-pulse-desktop.exe`. GUI opens, Overview / Providers tabs render.
3. Click the **Sessions** tab.
4. Window goes blank-white. Tab bar disappears. Title bar (Tauri chrome)
   remains. Process stays alive and idle.

Jason's screenshot showed every UI element gone — Overview / Providers /
Sessions / Alerts / Settings tab strip, the header (`CLI Pulse · Desktop ·
Windows`, Not paired indicator, Rescan button), all of it — leaving only
the empty white background inside the OS-level window chrome.

## Where the multi-hour forensic went wrong

The first diagnostic prompt to Claude Code on the VM tried to recover
information from a Rust-panic-style abort:

- WER ReportArchive — empty.
- WER ReportQueue — empty.
- `%LOCALAPPDATA%\CrashDumps` — directory absent.
- Application event log — zero `cli-pulse-desktop`-related entries.
- LocalDumps registry key armed with full memory dump capture — never fired
  because the process didn't abort.
- 60-second polling burst on the live PID — process stayed alive, idle, ~37
  MB working set, no thread/handle/CPU anomaly.

Each null result was correctly ruled out:

| Hypothesis | Evidence against |
|------------|------------------|
| Rust panic via `panic = "abort"` | PID alive 5+ minutes, CPU idle, no event log, no LocalDumps |
| WebView2 main process crash | child processes alive, no Crashpad reports |
| WebView2 renderer crash | `dev.clipulse.desktop\EBWebView\Crashpad\reports\` empty |
| OS / Defender intervention | System log clean for the relevant window |

The remaining hypothesis was a **frontend render-time exception** that
unmounted the tree. The window-chrome-intact-but-everything-else-gone
signature is React 18's default behavior when an exception bubbles past the
root with no `ErrorBoundary` wrapping the app.

F12 in the running process did nothing — Tauri 2 release builds have
devtools disabled by default unless the `devtools` Cargo feature is
enabled. We could not inspect the live error.

## Root cause

`src-tauri/src/sessions.rs` had:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSession {
    // … server fields …
    pub cpu_usage: f32,
    pub memory_mb: u64,
    pub pids: Vec<u32>,
    pub command: String,

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
```

(layout simplified for the writeup; all four fields were marked
`skip_serializing`)

The intent was: when the backend POSTs the sessions payload to Supabase via
`helper_sync.p_sessions`, strip the UI-only fields (CPU%, RAM, PIDs, raw
command) so we don't leak local process state to the server.

`#[serde(skip_serializing)]` strips the field from **every** serialization
of the struct. That includes the Tauri IPC return path:

```rust
#[tauri::command]
async fn list_sessions() -> Result<sessions::SessionsSnapshot, String> {
    async_runtime::spawn_blocking(sessions::collect_sessions)
        .await.map_err(|e| format!("sessions join error: {e}"))
}
```

Tauri serializes the return value with `serde_json` to ship it to the
WebView2 frontend, and serde respected the `skip_serializing` attribute on
both paths.

So the IPC response actually looked like:

```json
{
  "sessions": [
    {
      "id": "proc-7600",
      "name": "claude",
      "provider": "claude",
      "project": "(unknown)",
      "status": "Running",
      "total_usage": 540,
      "exact_cost": null,
      "requests": 1,
      "error_count": 0,
      "collection_confidence": "high",
      "started_at": "2026-05-01T14:30:12+00:00",
      "last_active_at": "2026-05-01T14:32:00+00:00"
    }
  ],
  "total_processes_seen": 218,
  "matched_before_dedup": 1,
  "collected_at": "2026-05-01T14:32:00+00:00"
}
```

No `cpu_usage`. No `memory_mb`. No `pids`. No `command`.

The Sessions tab in `src/App.tsx`:

```tsx
{sessions.map((s) => (
  <tr key={s.id} className="border-t border-neutral-800">
    <td>{s.provider}</td>
    <td>{s.project}</td>
    <td>{s.name}</td>
    <td>{s.cpu_usage.toFixed(1)}%</td>   // <-- TypeError here
    <td>{s.memory_mb} MB</td>
    <td><ConfidenceDot c={s.collection_confidence} /></td>
  </tr>
))}
```

`s.cpu_usage` was `undefined`. `undefined.toFixed(1)` throws
`TypeError: Cannot read properties of undefined (reading 'toFixed')`. The
exception happened during render of the `<tr>`, bubbled up through the
`map`, the `<tbody>`, the Sessions component, and the App component. With
no `ErrorBoundary` anywhere in the tree, React 18 unmounted the entire
React root, leaving the empty `<div id="root"></div>` at the top of
`index.html` — exactly the screenshot Jason sent.

### Why nobody saw this for 14 versions

- Mac is a separate Swift app, doesn't share this code.
- Linux had no real users (per memory: "v0.2.9 downloads: 4 (probably
  auto-update + my own tests); 14d page views: 21 (9 unique — basically
  just me)") and no developer manually ran the GUI.
- Windows GUI never started — every release v0.1.0 through v0.2.9
  shipped a `scan_cli` sidecar instead of the GUI binary (separate P0
  fixed in v0.2.10).
- `cargo test` and the Vitest suite test format helpers, scanner
  arithmetic, i18n key coverage — they don't render the React tree.

The bug needed all three to align: working Windows install, a process
matching one of the 27 provider regexes (Claude Code happened to be running
on the test VM), and a click on the Sessions tab. The first two became
possible only with v0.2.10. The click happened the same day.

## Fix

### 1. Split data model: `LiveSession` (full IPC) vs `SyncableSession` (stripped supabase view)

`src-tauri/src/sessions.rs`:

```rust
// LiveSession is now fully serializable; the IPC frontend sees all fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSession {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub project: String,
    pub status: String,
    pub total_usage: i64,
    pub exact_cost: Option<f64>,
    pub requests: i64,
    pub error_count: i64,
    pub collection_confidence: String,
    pub started_at: String,
    pub last_active_at: String,
    pub cpu_usage: f32,    // (was skip_serializing)
    pub memory_mb: u64,    // (was skip_serializing)
    pub pids: Vec<u32>,    // (was skip_serializing)
    pub command: String,   // (was skip_serializing)
}

// Stripped view used only when constructing the helper_sync.p_sessions
// payload. Same final wire shape sent to Supabase as before — just via
// an explicit struct boundary instead of a serde attribute that
// silently affected the IPC path too.
#[derive(Debug, Serialize)]
struct SyncableSession<'a> {
    id: &'a str,
    name: &'a str,
    provider: &'a str,
    project: &'a str,
    status: &'a str,
    total_usage: i64,
    exact_cost: Option<f64>,
    requests: i64,
    error_count: i64,
    collection_confidence: &'a str,
    started_at: &'a str,
    last_active_at: &'a str,
}

impl<'a> From<&'a LiveSession> for SyncableSession<'a> { /* … */ }

pub fn sessions_payload(snapshot: &SessionsSnapshot) -> serde_json::Value {
    let stripped: Vec<SyncableSession<'_>> =
        snapshot.sessions.iter().map(SyncableSession::from).collect();
    serde_json::to_value(&stripped).unwrap_or(serde_json::json!([]))
}
```

The supabase wire format is byte-for-byte identical to v0.2.10 (same field
set, same names, same order). No backend schema migration needed.

### 2. NaN sanitization on `cpu_usage`

`sysinfo` on Windows can return NaN for short-lived or protected processes
where the CPU% delta isn't computable. NaN serializes to the literal `NaN`
in `serde_json`'s output, which is invalid JSON; even when the field is
stripped, downstream arithmetic (`total_usage = elapsed_secs as f64 *
(1.5f64.max(cpu as f64 + 1.0))` — see `sessions.rs`) taints with NaN.

```rust
let cpu = proc.cpu_usage();
let cpu = if cpu.is_finite() { cpu } else { 0.0 };
```

### 3. Frontend defensive read

`src/App.tsx`:

```tsx
<td>{(s.cpu_usage ?? 0).toFixed(1)}%</td>
<td>{s.memory_mb ?? 0} MB</td>
```

A missing field now becomes `0.0%` and `0 MB` instead of a render-time
crash. This pairs with the backend fix; if either layer regresses, the
other still keeps the UI alive.

### 4. ErrorBoundary at the App root

New `src/ErrorBoundary.tsx` (class component because React 19 still has no
hook equivalent for `componentDidCatch` / `getDerivedStateFromError`).
Imported in `src/main.tsx`:

```tsx
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
```

Future render-time exceptions show a dark fallback panel with the error
name, message, JS stack trace, React component stack, and a "Try to
recover" button that resets the boundary state. The user sees a real
error instead of a silent white screen.

### 5. Tauri devtools enabled in release

`src-tauri/Cargo.toml`:

```toml
tauri = { version = "2", features = ["tray-icon", "devtools"] }
```

Ctrl+Shift+I / F12 now opens the WebView2 devtools panel in production
builds. Bundle size cost: negligible. This is what we wanted for the v0.2.10
diagnostic and didn't have.

## Tests

Existing tests still pass:

- 12 Rust integration tests in `scanner_integration.rs` (CRLF, TZ, cache
  idempotency, etc.).
- 25 Vitest unit tests (format helpers, i18n coverage).

No new tests added in this commit. A future addition could be a Vitest
test that mounts `<Sessions sessions={…}/>` with a mock IPC response that
omits `cpu_usage` and asserts the boundary catches; for now the
defense-in-depth in App.tsx covers it inline.

## Lesson

`#[serde(skip_serializing)]` on a struct that crosses two unrelated
serialization boundaries (Tauri IPC + Supabase RPC) is a footgun. Any
attribute that affects the wire format should be applied at the boundary
that owns the wire shape, not on the data type itself. Pre-v0.2.11 we did
the right thing for one boundary (supabase) and accidentally broke the
other (IPC).

Defensive frontend reads with `??` defaults are cheap insurance against
backend-frontend drift. We should adopt this consistently for any
optional-looking field that's actually required by the renderer.

`ErrorBoundary` is a hard requirement for production React apps. The cost
is one class component; the benefit is "we see what went wrong instead of
a blank screen." Should have shipped this from v0.1.0.

The combined v0.2.10 + v0.2.11 incident is the strongest argument yet for
the **mandatory real-VM smoke test before un-drafting** that the release
contract picked up between these two versions. CI never installed and
launched the binary; CI never clicked Sessions; CI never saw white screen.
The first time anyone clicked Sessions on a real Windows machine was the
same day the bug was found and fixed.
