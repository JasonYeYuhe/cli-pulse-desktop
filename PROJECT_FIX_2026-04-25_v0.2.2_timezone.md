# PROJECT_FIX 2026-04-25 — v0.2.2 timezone scan-range bug

## Symptom

Non-UTC users (especially JST and other UTC+ timezones) saw "today's"
usage stuck at 0 between local 00:00 and ~09:00, even when actively
using Claude Code or Codex. The Overview "Today — Cost" card stayed
empty until enough hours passed for UTC to catch up to local date.

## Reproduction

JST machine (UTC+9), at 06:00 local on 2026-04-25:
- `Utc::now()` = 2026-04-24 21:00 UTC → date_naive = `2026-04-24`
- `Local::now()` = 2026-04-25 06:00 JST → date_naive = `2026-04-25`
- A Claude session at 05:00 local emits a JSONL line with timestamp
  `2026-04-25T05:00:00+09:00`
- `parse_day_key_local(timestamp)` correctly returns `"2026-04-25"`
  (it converts to local TZ before extracting the date)
- `range.until_key` was `"2026-04-24"` (UTC date)
- `in_range("2026-04-25", "...", "2026-04-24")` → **false**
- Today's events filtered out across the entire day-by-day pipeline:
  scanner buckets, cache emit, ScanResult.entries

## Root cause

In `src-tauri/src/scanner.rs::scan_with_options` the date range was
anchored on `Utc::now()` while every other place that classifies an
event by day uses local time:

```rust
// before (buggy)
let today = Utc::now().date_naive();              // ← UTC
let since = today.checked_sub_signed(...);
let range = DateRange { since_key, until_key };   // ← UTC
let today_key = fmt_date(chrono::Local::now()...); // ← LOCAL (mismatched)
```

Mismatched anchors mean `parse_day_key_local`'s output is in a
different reference frame than the filter range it gets compared to.

## Fix

[`scanner.rs:87`](src-tauri/src/scanner.rs#L87) — anchor `today`,
`since`, `until_key`, AND `today_key` all on `chrono::Local::now()`:

```rust
let today = chrono::Local::now().date_naive();
let since = today.checked_sub_signed(...);
let range = DateRange {
    since_key: fmt_date(since),
    until_key: fmt_date(today),
};
let today_key = fmt_date(today); // same anchor — guaranteed to match until_key
```

Removed unused `Utc` import.

## Regression tests

[`scanner.rs::tests`](src-tauri/src/scanner.rs#L820) — 4 new tests:

- `today_key_matches_range_until_key` — runs an actual `scan_with_options`
  with a temp cache dir, asserts `result.today_key ==
  fmt_date(Local::now().date_naive())`. Property holds in any timezone.
- `parse_day_key_local_handles_rfc3339` — RFC3339 timestamps parse to
  10-char `YYYY-MM-DD`.
- `parse_day_key_local_falls_back_to_prefix` — date-only strings
  (`2026-04-25`) parse correctly via the prefix-extract branch.
- `in_range_inclusive` — boundary check for since/until inclusiveness.

42/42 Rust tests pass (was 38).

## Discovered by

Codex independent review on 2026-04-25 evening, post-v0.2.1 ship.
Codex output was just one line — `scanner.rs:87 local-vs-UTC day
filtering. I would not ship that to non-UTC users.` — but the call
was correct. Verified by tracing the data flow.

## Severity

Medium-high. Affects all non-UTC users for the portion of the day
where local date != UTC date. For JST users that's 9 hours (00:00–
09:00 local). For PST it's 16:00–24:00 local. UTC users never see it.

## User-visible impact pre-fix

- Overview "Today — Cost" / "Today — Tokens" / "Today — Messages"
  reported 0 / 0 / 0 during the affected window.
- 7-day cost trend chart showed no bar for "Today" column.
- Daily budget alerts couldn't trigger before today's data was
  visible — silent failure mode.
- helper_sync upload skipped today's metric rows entirely (the
  `__claude_msg__`-bucket-filtered iteration in `from_entry` saw no
  in-range entries for today).

## Shipped as

v0.2.2 (2026-04-25). Auto-update from v0.2.x picks it up.
