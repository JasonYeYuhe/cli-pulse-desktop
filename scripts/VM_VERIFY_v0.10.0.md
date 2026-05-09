# VM verify task — cli-pulse-desktop v0.10.0

你是 cli-pulse-desktop 在 Windows VM (clipulse-win-test) 上的发布
验证员。运行环境 Windows Server 2025 / x86_64,你有 PowerShell +
RDP + GitHub raw download 权限。

## 背景

VM 当前可能装着 v0.7.0(Latest)或某个 v0.8.x prerelease。Mac
端两天内连续 ship 了 7 个 prerelease(v0.8.1 → v0.10.0),所有
都在等 VM 验证后再 promote v0.10.0 到 Latest。

v0.8.0 BEX64 crash-on-launch 事故的根因 Sentry 已确认:
`tokio::spawn` 从 Tauri setup 钩子调用,无 tokio runtime context →
panic → `panic = "abort"` → 立即崩溃。v0.9.1 改用
`tauri::async_runtime::spawn` 修复;v0.9.2 把完整 ConPTY 实现接回
来。这一轮 verify **就是 v0.8.0 事故的回归校验**。

## 目标

验证 **v0.10.0 在这台 VM 上可以正常工作** — 包括启动、ConPTY
管理会话(P0 #2 安全验证)、Job Object 孤儿清理、诊断包等等。
全部 PASS → Mac 端把 v0.10.0 promote 为 Latest。任何 P0 FAIL →
立即上报,Mac 端会回退。

## 预备

下载 v0.10.0 安装包:
```
https://github.com/JasonYeYuhe/cli-pulse-desktop/releases/download/v0.10.0/CLI.Pulse_0.10.0_x64-setup.exe
```

卸载 VM 当前装的 CLI Pulse,装 v0.10.0(NSIS 接受 `/S`),启动
GUI(默认装到 `C:\Program Files\CLI Pulse\cli-pulse-desktop.exe`)。
打开后保持应用运行做下面的测试。

## Block A — 启动 + 60s 存活(P0 — v0.8.0 BEX64 回归)

1. 启动 GUI,打开后 等 60s
2. PASS 标准:
   - 进程仍活(`Get-Process cli-pulse-desktop`)
   - Windows Application Event Log **没有**新的
     `cli-pulse-desktop.exe` Application Error 事件(BEX64 / 0xc0000409)
   - GUI 5 个 tab 渲染(Overview / Providers / Sessions / Alerts /
     Settings)
   - Settings → About 显示 `0.10.0 windows`
3. FAIL = 进程消失 / WER 出现 BEX64 / GUI 不出现 → **P0 立即上报**

## Block B — 键盘快捷键(v0.10.0 新功能)

不需要联网,本地就能验证:
1. `Ctrl + R` 触发重新扫描(Overview tab 的 stats 应该刷新或
   showing scanning indicator)
2. `Ctrl + 1` / `Ctrl + 2` / `Ctrl + 3` / `Ctrl + 4` / `Ctrl + 5`
   切换 5 个 tab
3. `Ctrl + ,` 切到 Settings
4. **`Ctrl + Shift + /`** 弹出快捷键帮助 modal,显示快捷键列表
   带平台对应的 modifier(Windows 显示 `Ctrl`)
5. Esc 关 modal
6. PASS = 全部行为符合预期。FAIL = 某个绑定无效或冲突

## Block C — 诊断包按钮(v0.9.3 新功能)

1. Settings → About 找 "Save diagnostic bundle" 按钮(或类似 i18n
   翻译)
2. 点击它
3. PASS:
   - 操作系统 file save dialog 打开
   - 默认文件名 `cli-pulse-diag-<timestamp>.zip`
   - 保存后,zip 内容包含:
     - `cli-pulse.log`(若已 paired)
     - `remote-hook.log`(若 hook 跑过)
     - `diagnostic_snapshot.json`(总有)
     - 可选:`crash-history.jsonl`、WER events
   - 总大小 < 2 MB

## Block D — Settings → About 代理诊断(v0.9.2 恢复)

1. Settings → About 查看 "Agent diagnostic" 区块
2. PASS(已 paired + 非 recovery mode 时):
   - 显示三行:`N managed session(s) running` /
     `N hosted lifetime` / `Last tick X s ago`(应每 5s 更新)
3. PASS(未 paired 或 recovery 模式):
   - 显示 `Agent loop not running (sign in to enable)` 单行

## Block E — ConPTY 管理会话 round-trip(P0 #2 — 核心安全验证)

⚠️ **本 block 是 v0.9.2 的核心 P0 安全验证 — 关键中之关键**

需要 paired 设备 + Remote Control 已开 + ⚠️ Mac 端协助 OR
你自己用 Sessions tab 的 "+ Start new session" CTA。

### E.1 — Spawn

1. Sessions tab 顶部应有 `+ Start new session`(v0.9.2 恢复的)
2. 点击 → modal dialog → 输入工作目录(如 `C:\Users\clipulse\Documents`)
   → Provider = Claude → 点 Start
3. **预期 ~1s 内**:
   - Sessions list 出现新 row,status=`pending`
   - ~1s 后 status flip 到 `running`
   - Task Manager 看到 `claude.exe` 子进程

### E.2 — Send + Interrupt(P0 #2)

4. 用上面 spawn 的 session,点 Send,输入 prompt:
   `count slowly from 1 to 100, one per second`
5. Claude 开始 streaming 时(5-10 之间),点 **Interrupt** 按钮
6. **CRITICAL PASS 标准**:
   - **`cli-pulse-desktop.exe` 主进程必须保持运行**(Task Manager 验证)
   - 主进程消失 → **立即 P0 上报,这是 v0.8.0 P0 #2 没修好的明确信号**
   - claude.exe 收到 Ctrl-C 中断,session 状态保持 running

### E.3 — Stop

7. 点 Stop button
8. claude.exe 应终止,row status flip 到 `stopped`
9. Job Object 关闭,所有子进程清理

## Block F — Job Object 孤儿清理(v0.9.2 P1 验证)

1. spawn 一个新 session(同 Block E.1)
2. **强制杀**主进程:Task Manager → cli-pulse-desktop.exe → End
   task(Force,**不是** tray Quit — 后者是优雅退出)
3. **~2s 内**观察 Task Manager:
   - PASS = 之前 spawn 的 `claude.exe` 也死了(Job Object
     KILL_ON_JOB_CLOSE 触发)
   - FAIL = claude.exe 还活着 → P1 上报(Job Object FFI 没生效)
4. 重启 cli-pulse-desktop GUI

## Block G — 崩溃恢复模式(v0.9.0 新功能)

要触发,你需要快速强制杀进程 3 次:

1. 启动 GUI,30s 内 force-kill(Task Manager 或
   `Stop-Process -Name cli-pulse-desktop -Force`)
2. 立即重启 GUI,30s 内 force-kill 第二次
3. 立即重启 GUI,30s 内 force-kill 第三次
4. **第 4 次启动 GUI**:
   - 应显示 banner:"Detected repeated crashes. Some features
     disabled" 或类似中文/日文
   - 设置中的 agent diagnostic 显示 "not running"
   - tray 图标可能没出现(refresh loop 被禁,tray 本身仍装)
5. 关闭应用让 5 分钟过去后再启动,recovery 应自动消失
6. PASS = banner 出现且 agent 被禁。FAIL = banner 不出现 OR agent
   仍跑

## Block H — Kill-switch env var(v0.9.1 新功能)

1. PowerShell 中:
   ```powershell
   $env:CLI_PULSE_DISABLE_REMOTE_AGENT = "1"
   & "C:\Program Files\CLI Pulse\cli-pulse-desktop.exe"
   ```
2. PASS:
   - GUI 启动正常
   - cli-pulse.log 含 `Remote agent loop NOT spawned —
     CLI_PULSE_DISABLE_REMOTE_AGENT env var set`
   - Settings → About agent diagnostic 显示 "not running"

## Block I — Sentry 端到端

1. Settings → About → "Send test event"
2. 让 Mac 端用 sentry-cli 验证:
   ```
   sentry-cli issues list --org jason-yeyuhe --project desktop \
     --query "release:cli-pulse-desktop@0.10.0 age:-1h"
   ```
3. PASS = 1 条 `diagnostic_test=true` 事件

## Block J — 历史回归 spot-check(v0.7.0 起的核心功能)

1. v0.6.0 远程审批 modal 能 view + decide(任一语言)
2. v0.7.0 hook 安装状态在 Settings → Privacy 显示正常
3. background sync 每 120s 跑一次(`cli-pulse.log` 看
   `background sync ok`)
4. tray 菜单显示 Month/Forecast/Synced ago(已 paired)

## 已知 RDP 焦点限制

VM 经 RDP 控制时,某些键盘事件(Esc / Enter / 修饰键组合)未必到
WebView2。如果 Esc 不关 modal 但 Cancel 按钮工作 → INCONCLUSIVE
跳过,本地 Win 机器上不是 bug。

## 报告

回复一段总报告,**结尾必须有**两行:

```
P0 status: PASS / FAIL / DEGRADED  (Block A + E.2)
Overall: PASS / PARTIAL / FAIL
```

每个 block 一行 PASS / FAIL / SKIPPED / INCONCLUSIVE,关键证据
(进程 PID / WER 事件 ID / banner 截图 URL 等)。

## Block F 关 VM(等 Jason 确认)

完成所有 block 后**不要直接 deallocate**。等 Jason chat 说"确认
关 VM"再跑:
```
az vm deallocate --resource-group cli-pulse-test-rg --name clipulse-win-test
```

## Privacy

device_id / helper_secret / refresh_token / JWT 不要原文贴回报。
device_id 前 4-8 位 hex(如 `7e6e…`)可贴。Sentry event id 是
非凭据 ID,可贴。版本号、PID、exception code、faulting module 名
字都安全。
