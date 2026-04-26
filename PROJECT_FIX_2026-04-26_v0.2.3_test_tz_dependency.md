# PROJECT_FIX 2026-04-26 — v0.2.3 integration test depended on host TZ

## Symptom

The first push of v0.2.3 (`670afff`) failed CI on **all four** matrix
platforms (Windows x64, Windows ARM64, Linux x64, Linux ARM64) with
the same error:

```
test timezone_anchor_uses_today_override_consistently ... FAILED
thread 'timezone_anchor_uses_today_override_consistently' panicked at
tests/scanner_integration.rs:106:13:
no entry for (2026-04-25, Claude, claude-haiku-4-5).
```

Locally on my dev Mac (JST = UTC+9) the test passed. The same code,
same fixture, same assertion — but four CI machines saw a different
result than my one machine.

## Root cause

The fixture used `2026-04-25T05:00:00+09:00` as the Claude event
timestamp:

```rust
const CLAUDE_LATE_NIGHT: &str = r#"...
{"type":"assistant","timestamp":"2026-04-25T05:00:01+09:00",...}
"#;
```

The scanner's `parse_day_key_local` converts every timestamp to the
host's local timezone before extracting the day-key. So on my machine:

- JST (UTC+9): `2026-04-25T05:00 +09:00` → 05:00 JST = local
  `2026-04-25` ✓ matches the asserted "2026-04-25" entry

On a UTC CI runner:

- UTC (offset 0): `2026-04-25T05:00 +09:00` → `2026-04-24T20:00 UTC`
  = local `2026-04-24` ✗ no entry for "2026-04-25"

The test was correctly verifying that today_override pinned the
range, but it accidentally also tested that the host's local TZ
matched JST — which only my dev machine does.

## Irony

The bug is the **exact same flavor** as the v0.2.2 production bug
that Codex flagged a day earlier — host-TZ vs assumed-TZ mismatch.
Sprint 9 was supposed to add tests that protect us from that whole
class of bug, and the very first such test re-introduced the same
mistake at the test layer.

## Fix

[`tests/scanner_integration.rs:CLAUDE_TZ_STABLE`](src-tauri/tests/scanner_integration.rs)
— switched the fixture timestamp to mid-day UTC:

```rust
const CLAUDE_TZ_STABLE: &str = r#"...
{"type":"assistant","timestamp":"2026-04-25T12:00:01Z",...}
"#;
```

`12:00:00Z` lands on `2026-04-25` for any host TZ in the range
`UTC-12` to `UTC+14`, which covers every real-world timezone except
two uninhabited Pacific islands.

The other 9 integration-test fixtures were already using mid-day UTC
timestamps, so this was a one-line fix in one fixture. Caught and
shipped within ~5 minutes of the CI failure notification.

## Lesson

**Tests must never depend on the host's local timezone.** Specifically:

- Don't use offset suffixes (`+09:00`, `-08:00`, `+05:30`) in test
  fixtures unless the test is *deliberately* about offset handling
  AND it pins the host TZ via `TZ=UTC` env var.
- Mid-day UTC (`THH:MM:SSZ` with HH between roughly 04 and 20) is
  TZ-stable for any reasonable host.
- For tests that assert local-frame behavior, set `today_override`
  AND ensure the fixture timestamps are TZ-stable, OR pin `TZ` for
  that test specifically.

Adding this as a checklist item to mental review whenever I write
fixtures that include timestamps. Considered adding a clippy lint or
a custom test helper that rejects non-Z timestamps in fixture
strings, but the false-positive rate on any sufficiently general
matcher would outweigh the value.

## Discovered by

GitHub Actions matrix CI (4 platforms, all UTC). The matrix
specifically caught this because each runner has the same TZ but
differs from my dev machine. A single-platform CI would have
silently passed if it ran on a JST machine.

## Severity

Low — purely a test-harness bug, no production code path was
affected. The production fix from v0.2.2 (anchor today/since/until/
today_key on `Local::now()`) was and remains correct.

## Shipped as

`5881807` (rolled into the v0.2.3 tag). v0.2.3 ships normally with
the corrected test.
