# macOS/iOS Parity Roadmap — 2026-06-20

Driven by a multi-agent gap audit of the desktop port (v0.10.0) against
the macOS/iOS app (`cli-pulse-private`, v1.28). 109 gaps across 8 areas
(overview, providers, sessions, alerts, settings, swarm, onboarding,
tray/menubar).

## Executive summary

The desktop port has the structural skeleton (Overview/Providers/
Sessions/Alerts/Settings tabs, local-scan pipeline, OTP auth, tray,
native notifications) but trails Mac/iOS on visual identity and depth.
The biggest theme is missing **frontend-only presentation** (provider
accent colors/icons, cost summary strips, severity dots, metric-tile
theming) — much shippable against data the desktop already fetches.
Three whole surfaces are absent: the **Swarm tab**, the **onboarding/
first-run** experience, and the **alert lifecycle** (resolve/ack/
snooze) — the latter two need backend work.

## Quick wins — frontend-only, high/medium impact

| # | Gap | Area | Impact | Effort | Status |
|---|-----|------|--------|--------|--------|
| 1 | Per-provider accent color + avatar on cards | providers | High | M | ✅ done (`89094c9`) |
| 2 | Provider usage breakdown bars on Overview (`get_provider_summary` exists) | overview | High | M | todo |
| 3 | Cost summary strip (Today / 30-day est) atop Providers | providers | High | S | todo (verify row fields) |
| 4 | Per-provider 30-day usage mini bar-chart | providers | High | M | todo |
| 5 | Inline approve/reject on session rows (`decide_remote_approval` exists) | sessions | High | M | todo |
| 7 | Cost Forecast card: actual-to-date + day-progress bar | overview | Med | S | todo |
| 8 | Metric tiles: icons, accent colors, cost_status sub-badge | overview | Med | S | todo |
| 11 | Severity dots + summary badges + entity chips on alerts | alerts | Med | S | todo |
| 12 | Per-provider color on session rows (reuses `providerTheme.ts`) | sessions | Med | S | todo — natural next |
| 10 | Tier bars: "Resets X" + LOW/OK quota badge | providers | Med | S | todo |

Also landed this sprint: **per-provider visibility filter** (`e841c46`).

## Bigger rocks — need Tauri/Rust or Supabase work

- **Swarm tab** — new `remote_list_swarms` Tauri command wrapping the
  existing `remote_app_list_swarms` RPC + 2 serde structs; pairs with a
  frontend tab (attention-sort, "N swarms · N agents · N blocked").
- **Alert lifecycle** — Rust commands for PATCH RPCs (resolve / ack /
  snooze / resolve-all) + re-point the tab from `preview_alerts` to
  persisted `get_server_alerts`. Unblocks the whole Alerts area.
- **Live session output / transcript tail** — get-session-events / tail
  command + ANSI-stripped, secrets-redacted, privacy-gated output panel.
- **Onboarding wizard + local-mode** — Welcome/Features/Privacy/SignIn/
  AllSet shell + persisted "onboarding completed" flag. Largest
  greenfield UX area.
- **Live tray status item** — drive `set_icon`/`set_title` from live
  metrics + alert badge in `TrayMetrics`.
- **Tray actions** — wire "Refresh now" + "Remote Approvals" menu items
  to existing `sync_now` / approval commands.
- **Cost summary / subscriptions / per-model breakdown (Overview)** —
  new backend surfaces for subscription price, utilization, per-model
  aggregation.
- Lower priority: Yield Score card, export menu (CSV/PDF), provider
  enable/disable + reorder, launch-at-login, service-status badges.

## Next batch (recommended order)

Reuse the `providerTheme.ts` palette while it's fresh: **#12** (session
row colors) then **#11** (alert severity dots) — both frontend-only,
S effort. Then **#2/#3** on Overview/Providers (verify the desktop
`ProviderSummaryRow` / `ScanResult` already carry the needed fields
before building). Bigger rocks (Swarm, alert lifecycle) when ready to
touch Rust.
