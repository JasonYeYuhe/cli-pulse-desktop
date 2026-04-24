# CLI Pulse — Desktop (Windows + Linux)

Cross-platform companion to the [CLI Pulse](https://github.com/JasonYeYuhe/cli-pulse)
macOS / iOS / Android apps. Tracks usage, tokens, and cost for AI command-line
tools (Claude Code, Codex, Gemini CLI, Cursor, Kimi, GLM, and ~20 more) from a
single native desktop app.

**Status:** Sprint 0 — local scan + UI only, no network yet.

## Stack

- **Backend:** Rust 1.85+, Tauri 2
- **Frontend:** React 19 + TypeScript + Tailwind CSS v4
- **Targets:** Windows 11 (MSIX + portable exe), Linux (.deb / .rpm / .AppImage / Flathub)

## Dev

```bash
npm install
npm run tauri dev       # dev with hot reload
npm run tauri build     # production bundle
(cd src-tauri && cargo test)
```

### Linux build deps (Ubuntu/Debian)

```bash
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev libssl-dev patchelf
```

## Layout

```
src/                      React + TS frontend
src-tauri/
  src/
    lib.rs                Tauri entry + invoke handlers
    paths.rs              ~/.claude / ~/.codex resolution (Win + Unix)
    pricing.rs            Per-token cost tables (Codex + Claude)
    scanner.rs            JSONL scanner + per-day/model/provider aggregation
  tauri.conf.json
  Cargo.toml
.github/workflows/ci.yml  rustfmt + clippy + tests + tauri build on Win + Linux
```

## Roadmap

- **Sprint 0** — local scan + UI (this commit)
- **Sprint 1** — Supabase pairing (6-digit code from iPhone), helper_sync + upsert_daily_usage
- **Sprint 2** — system tray, Windows/Linux notifications, auto-update
- **Sprint 3** — Microsoft Store (MSIX) + Linux packaging (AppImage / .deb / .rpm / Flathub)
