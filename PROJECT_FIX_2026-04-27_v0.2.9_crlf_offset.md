# PROJECT_FIX 2026-04-27 — v0.2.9 CRLF byte-offset drift

## Symptom

On Windows, JSONL files written with CRLF line endings would silently
lose the first event of every appended tail across incremental scans.
The cumulative-delta math for Codex `total_token_usage` would also
drift if the offset landed mid-line on resumption.

Likely user-visible effect on Windows: today's most recent Codex
events not showing up in the Overview between consecutive 2-minute
sync ticks, sometimes off by ~1 message per file.

The bug had been latent since v0.1.3 (incremental cache, 2026-04-25).
No user reports — the cohort of paired Windows users at this stage is
small and the off-by-one is invisible without comparing to ground
truth.

## Reproduction

```rust
// scanner.rs:373 (codex) and :661 (claude), pre-fix:
for line in reader.lines() {
    let line = match line { Ok(l) => l, Err(_) => continue };
    bytes_seen += line.len() as i64 + 1;  // ← +1 for the newline consumed
    // ...
}
```

`BufRead::lines()` strips both `\r\n` and `\n` from each yielded
`String`, but `line.len()` is the content length only. We were always
adding +1 to account for `\n`, so for CRLF-terminated lines we were
under-counting the actual file bytes consumed by exactly 1.

After parsing N CRLF lines, `parsed_bytes = (true file bytes) − N`.
Persisted to disk in the cache. On the next scan, `decide_action`
returns `Incremental { start_offset: parsed_bytes }`, and the file
seeks N bytes too early — into the middle of line N+1. The first
serde_json::from_str on the partial-line fails, the line is silently
dropped, and parsing continues normally from line N+2.

LF-only files (Linux, macOS, Codex/Claude CLIs writing on Windows
through Node.js / Rust which default to LF) were unaffected.

## Discovered by

Codex deep review of v0.2.8 — terse one-line output again:

> Next sprint recommendation: fix the incremental scanner offset
> bookkeeping first; it's the highest-value correctness risk in the
> shipped product.

This is the third Codex review in three sprints to nail a real bug
in a single line. The other two:

- v0.2.2: `scanner.rs:87` UTC-vs-Local timezone mismatch
- v0.2.7: `i18n.ts:52` unawaited `changeLanguage()` Promise

## Fix

[`scanner.rs::parse_codex_file`](src-tauri/src/scanner.rs) and
[`parse_claude_file`](src-tauri/src/scanner.rs) — replace
`for line in reader.lines()` with explicit `read_until(b'\n', &mut buf)`:

```rust
let mut buf: Vec<u8> = Vec::with_capacity(4096);
loop {
    buf.clear();
    let n = match reader.read_until(b'\n', &mut buf) {
        Ok(0) => break,
        Ok(n) => n,
        Err(_) => continue,
    };
    bytes_seen += n as i64;  // EXACT byte count including terminator
    while matches!(buf.last(), Some(&b'\n') | Some(&b'\r')) {
        buf.pop();
    }
    if buf.is_empty() { continue; }
    let line: &str = match std::str::from_utf8(&buf) {
        Ok(s) => s,
        Err(_) => continue,
    };
    // ... existing line processing ...
}
```

`read_until` returns the actual number of bytes consumed from the
underlying reader, terminator included. We strip CR / LF in-place
from the buffer ourselves, then process the line as before. No
information about file byte counts is ever inferred from line content
length.

## Regression tests

[`tests/scanner_integration.rs`](src-tauri/tests/scanner_integration.rs) —
two new tests, both fixture-based:

- `crlf_codex_jsonl_parses_identically_to_lf` — writes the same
  Codex content twice, once with `\r\n`, once with `\n`. Asserts
  resulting `DailyEntry` totals are byte-equal.
- `crlf_incremental_resume_does_not_drop_lines` — writes a CRLF
  fixture, scans (warm), appends a second event, scans again. Asserts
  the appended event's tokens land in the result. Pre-fix this test
  fails with `2500 → 1000` (only the original line counted, the
  appended event silently dropped).

Total tests: 53 Rust + 25 frontend + 12 integration = **90** (was
78 in v0.2.8).

## Severity

**Fix-soon** rather than block, because:

- Affects only Windows users (LF-only files unaffected)
- Loss is limited to the FIRST event after each cache resumption,
  not all subsequent events — the JSONL line-by-line resync recovers
  on the next valid `\n`
- The cumulative-delta math for Codex tokens means a dropped
  `token_count` event is "absorbed" by the next event's higher
  cumulative total — so the daily total is preserved, only the
  per-message granularity is lost

Real impact: small drift in per-message `cost_nanos` slot for Claude
(per-message cost is the bit-exact-Swift-parity invariant). Worst
case: a busy Windows user could see today's Claude cost off by
1–2% if many cache resumptions happen mid-day. UTC and macOS users
unaffected.

## Shipped as

v0.2.9 (2026-04-27). Auto-update from any 0.1.x or 0.2.x picks it up.
This release also activates Sentry crash reporting for the first
time — see CHANGELOG.
