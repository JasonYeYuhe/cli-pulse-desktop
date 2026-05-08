# VM smoke — v0.8.1 (radical revert post-v0.8.0 incident)

Single-block prompt for the VM Claude session. Goal: confirm v0.8.1 launches and stays alive (the v0.8.0 BEX64 crash is gone). No feature verification — just survival.

---

````
你是 cli-pulse-desktop 在 clipulse-win-test 的 v0.8.1 smoke 验证员。**目标只有一个**:确认 v0.8.1 能启动 + 进程能保持 >60s 不死。v0.8.0 在这台 VM 上每次 ~5-12s 后 BEX64 崩溃,v0.8.1 是 radical revert(去掉 ConPTY feature,只留 v0.7.0 baseline + remote-hook.log diagnostic)。

## 前提
- v0.8.0 已 YANK 到 prerelease,Latest 是 v0.7.0
- v0.8.1 一旦 CI green + Mac 端 promote,Latest 自动指向 v0.8.1
- 你这边目前装的可能是 v0.8.0(无法启动)

## 任务
1. 卸载 v0.8.0(若存在):Settings → Apps → 找 CLI Pulse → Uninstall。或 PowerShell:
   `Get-Package | Where-Object { $_.Name -like "*CLI Pulse*" } | Uninstall-Package`
2. 等 Mac 端 chat 通知 "v0.8.1 promoted to Latest",然后:
   - Mac 端 `gh release download v0.8.1 -p "*x64-setup.exe"` → 转传到 VM
   - 或:让 Mac 端把 NSIS URL 给你,你 `curl` 下来
3. `Start-Process -Wait -FilePath "...x64-setup.exe" -ArgumentList "/S"`
4. 启动 GUI:`Start-Process "C:\Program Files\CLI Pulse\cli-pulse-desktop.exe"` (或 Start Menu)
5. **关键观察期 60s**:
   - PASS 标准:60s 后 `Get-Process cli-pulse-desktop` 仍返回进程,GUI 窗口可见,5 tabs 渲染(Overview / Providers / Sessions / Alerts / Settings)
   - FAIL 标准:进程在 60s 内消失;或 WER 重新出现 BEX64 / 0xc0000409 事件;或 GUI 窗口从不出现
6. (可选)Settings → About → 验证显示 `0.8.1 windows`
7. (可选)留 GUI 跑 5 min,确认 Background sync 正常 tick

## 报告
一行 PASS/FAIL + 关键证据(进程 ID / Get-Process 输出 / 必要时 Get-WinEvent -LogName Application -Newest 10)。

如果 PASS → Jason 这边把 Latest 留在 v0.8.1。如果 FAIL → Jason 立即再回退 Latest 到 v0.7.0,v0.8.1 也 YANK,这意味着崩溃源头不在 v0.8.0 的新增 ConPTY 代码,需要更深挖。

Privacy:device_id / helper_secret / JWT 不要原文贴入回报。
````
