# Release Guide

CLI Pulse Desktop publishes to GitHub Releases only — no Microsoft Store,
no Flathub, no AUR (yet). Releases are built by CI on tag push, signed
with a Tauri minisign keypair, and auto-updated in-app via
`tauri-plugin-updater`.

## Artifacts per release

| Platform | File | Purpose |
|---|---|---|
| Windows | `CLI.Pulse_X.Y.Z_x64-setup.exe` | NSIS installer (double-click → install per-user) |
| Linux | `cli-pulse-desktop_X.Y.Z_amd64.deb` | Debian / Ubuntu |
| Linux | `cli-pulse-desktop-X.Y.Z-1.x86_64.rpm` | Fedora / RHEL |
| Linux | `cli-pulse-desktop_X.Y.Z_amd64.AppImage` | Universal Linux |
| Updater | `latest.json` | Updater manifest — pointed at by `tauri.conf.json` |
| Signatures | `*.sig` | One per artifact, signed with the minisign private key |

## Cutting a release

1. **Bump version** in `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`,
   and `package.json`. Use `npm version X.Y.Z --no-git-tag-version` or
   edit manually — keep the three in sync.
2. **Update CHANGELOG.md** with the changes since the last tag.
3. **Commit + push** to `main`.
4. **Tag** the commit:
   ```bash
   git tag -a vX.Y.Z -m "vX.Y.Z"
   git push origin vX.Y.Z
   ```
5. The `Release` workflow triggers automatically. It:
   - Builds Windows (NSIS) + Linux (.deb / .rpm / .AppImage)
   - Signs each artifact with the minisign key from GH Actions secrets
   - Creates a **draft** GitHub Release with all artifacts + `latest.json`
6. Review the draft at
   https://github.com/JasonYeYuhe/cli-pulse-desktop/releases, polish the
   notes, flip from Draft → Published. **Publishing makes auto-update
   live** for all existing installs.

## GitHub Actions secrets (one-time setup)

The release workflow expects these secrets at
`Settings → Secrets and variables → Actions`:

| Secret | Where to find it |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | Full text of `~/.config/tauri/cli-pulse-desktop.key` (Jason's dev machine). `cat` the file and paste. |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Empty (the current key has no password). Omit the secret entirely, OR set to empty string. |

```bash
# Jason's machine — add secret
gh secret set TAURI_SIGNING_PRIVATE_KEY \
  --repo JasonYeYuhe/cli-pulse-desktop \
  < ~/.config/tauri/cli-pulse-desktop.key
```

**Don't rotate the key** unless it leaks. Rotating breaks auto-update
for every existing install — users have to manually download the next
release once, because their old pubkey (baked into `tauri.conf.json`)
can't verify updates signed by the new private key.

## Auto-update flow (what users see)

1. On every app launch + via the Settings → Updates button, the app
   calls `check()` against
   `github.com/.../releases/latest/download/latest.json`
2. If a newer version is advertised, the app downloads the installer
   for the user's platform, verifies the signature against the pubkey
   embedded at build time, installs, and prompts to restart.
3. No prompts before download — user already clicked the button.

## Rolling back a bad release

1. **Unpublish** (flip to Draft) the broken release on GitHub. That
   removes it from the `latest` endpoint; existing installs keep
   running the old version, new users can't get the broken one.
2. **Publish** the previous good release as the new `latest` by
   editing it and toggling "Set as latest".
3. Fix forward in a new `vX.Y.Z+1` tag.

## Dev builds (no tag)

The regular `CI` workflow does `tauri build --debug --no-bundle` on
every push to validate the code compiles. No artifacts are attached,
no signing happens, no Release is created.
