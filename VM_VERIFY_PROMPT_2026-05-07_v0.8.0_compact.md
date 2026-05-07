# VM verify prompt — v0.8.0 compact

Single self-contained prompt for one fresh VM Claude Code session. Paste everything between the markers.

---

````
你是 cli-pulse-desktop v0.8.0 在 Azure VM (clipulse-win-test) 的发布验证员。当前 Latest = v0.8.0。前一轮(v0.7.0)留下诊断盲区:medium-risk hook 服务端 0 行,无法判断 hook fail-fast 还是 RPC reject。v0.8.0 加了 `remote-hook.log` 解决。**核心新功能**:本机 HOST 别的设备 UI 驱动的 managed Claude session(ConPTY)。

## Phase 0 + Phase 1 — sanity + upgrade
1. PowerShell:`Get-StartApps | ? { $_.Name -like "*CLI Pulse*" }` 记录当前版本(应为 v0.7.0)
2. 启动 GUI,header banner "Update available" → 点 → Settings → 等下载完 → relaunch → Settings → About 应显示 `0.8.0`
   - banner 失败 fallback:Mac 端 `gh release download v0.8.0 -p "*x64-setup.exe"` 转传 VM 后 `Start-Process -Wait ...x64-setup.exe -ArgumentList "/S"`

## Block A — 本机 spawn(新功能)
3. Sessions tab 顶部 "+ Start new session" → 输 cwd(如 `C:\Users\clipulse\Documents`)→ Start
4. PASS 标准:row 出现 `pending` → ~1s flip `running`;Task Manager 看到 `claude.exe` 子进程

## Block B — Send / Interrupt(**P0 #2 核心安全验证**)
5. 用上面 session,Send prompt:`count slowly from 1 to 100, one per second`
6. Claude streaming 时点 Interrupt
7. **CRITICAL**:`cli-pulse-desktop.exe` 主进程**必须保持运行**(Task Manager 验证)。主进程消失 → 立即 P0 chat 上报。期望:claude 收到 Ctrl-C 中断,主进程不动

## Block C — Job Object orphan cleanup
8. Task Manager 强制 End task `cli-pulse-desktop.exe`(不要 tray Quit)
9. PASS 标准:~2s 内之前 spawn 的 `claude.exe` 也死(KILL_ON_JOB_CLOSE)。还活着 → P1 上报
10. 重启 GUI(从 Start Menu)

## Block D — 关闭 v0.7.0 诊断盲区
11. spawn 一个新 claude session,跑 medium-risk:`echo hello-from-vm-080`,Mac/iOS 审批
12. 检查 `%LOCALAPPDATA%\dev.clipulse.desktop\logs\remote-hook.log` 应有完整链路:
    `hook fired → config loaded → risk classified → create_request POST → poll → decision`
13. PASS 标准:文件存在,六个步骤都有时间戳行;FAIL = 文件空 / 缺 `create_request POST`

## Block E — 历史回归(每项 1 行 PASS/FAIL,跳着 spot-check 即可)
14. v0.6.0 远程审批 modal 能 view + decide(任一语言)
15. v0.7.0 hook 安装状态在 Settings → Privacy 显示正常
16. Settings → About → "Send test event"(Mac 用 sentry-cli 验证)
17. `cli-pulse.log` 每 120s 有 `background sync ok`

## RDP 焦点限制(don't debug)
VM 经 RDP 控制时 Esc / Enter 未必到 WebView2。Esc 不关 modal 但 Cancel 工作 → INCONCLUSIVE 跳过

## Block F — 关 VM(**等 Jason 确认**)
18. 提交 Phase 0 / Phase 1 / Block A-E 总表(每行 PASS/FAIL/SKIPPED/INCONCLUSIVE)
19. tray → Quit;**不要直接 deallocate** — 等 Jason chat 说"确认关 VM"再跑 `az vm deallocate -g cli-pulse-test-rg -n clipulse-win-test`

## Privacy
device_id / helper_secret / JWT 不要原文贴入回报。device_id 前 4-8 位 hex(如 `7e6e…`)可贴。
````
