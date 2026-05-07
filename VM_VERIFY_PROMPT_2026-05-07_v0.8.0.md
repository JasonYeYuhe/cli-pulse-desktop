# VM verify prompt — v0.8.0 ConPTY managed-session local host

Copy the section between the markers below into a fresh Claude Code session on `clipulse-win-test`. Block F has the standard wait-for-user-confirmation pattern before deallocating the VM.

---

## ▼ START COPY-PASTE BELOW THIS LINE ▼

````
你是 cli-pulse-desktop 项目在 Windows VM (clipulse-win-test) 上的发布验证员。本次验证 **v0.8.0 — ConPTY managed-session local host**(Slice 4 of the Remote Sessions track,Windows desktop 现在能 HOST 别的设备 UI 驱动的 managed Claude session)。

## 上下文
- 当前 Latest = v0.8.0(本轮 ship 后会 promote)。前几轮 ship:
  - v0.7.0 (2026-05-07):Windows-side hook emission(claude PermissionRequest → 远程审批)
  - v0.6.2 (2026-05-07):Send / Stop / Interrupt managed sessions(app-side)
  - v0.6.1 (2026-05-07):Esc-key 关 modal 修复
  - v0.6.0 (2026-05-06):Remote Approvals(app-side view + decide)
- 上次 VM 状态:v0.7.0 verify 大部分 PASS,**关键留下的诊断盲区**:medium-risk D.1 在 claude 端正确 round-trip(13.35s 后 echo 执行),但服务端 `remote_permission_requests` 表 0 行 — 无法判断 hook 子进程是 fail-fast 还是服务端 gate 拒绝。本轮 v0.8.0 折进了 `remote-hook.log` file logging 解决这个盲区。
- VM 凭据已就绪:`gh`(read-only spot-checks,push 在 Mac 端做)、`sentry-cli`、`az`(可以 self-deallocate)
- LocalDumps 已预设到 `%USERPROFILE%\v0.8.0-smoke\diag\dumps`
- VM admin user: `clipulse`(密码不要让我或 Claude 输入,你手动)

## Phase 0 — 启动 + sanity check
1. Mac 端起 VM:`az vm start --resource-group cli-pulse-test-rg --name clipulse-win-test`(等 ~30s)
2. Portal 看新 public IP,RDP 进入
3. PowerShell sanity:
   - `Get-StartApps | Where-Object { $_.Name -like "*CLI Pulse*" }` → 当前应为 v0.7.0
   - `gh --version` / `sentry-cli --version` / `az --version` 应该都能跑(无需登录,凭据已 cached)
   - 确认 `%LOCALAPPDATA%\dev.clipulse.desktop\logs\` 存在(v0.4.x 起就有)

## Phase 1 — 升级到 v0.8.0
**首选:banner-click 自动更新路径**
- 启动 v0.7.0 GUI,header 应在 ~30s 内出现 "Update available" banner(latest.json 已被 promote 到 v0.8.0)
- 点 banner → Settings → 应自动开始下载 → 下载完后弹 relaunch 提示 → 接受
- v0.8.0 GUI 起来后,Settings → About 应显示 `0.8.0 windows`

**Fallback:fresh-install /S 路径**(banner-click 失败时)
- 在 Mac 端用 gh 下载 NSIS:`gh release download v0.8.0 --pattern "*x64-setup.exe" --dir /tmp/v080`
- 计算 sha256 比对(选项)
- 复制 / 转传到 VM,然后 `Start-Process -Wait -FilePath "...x64-setup.exe" -ArgumentList "/S"`
- 启动 GUI 验证版本号

## Block A — 管理会话 spawn(关键新功能)

需要 Mac 协助触发 — 让 Jason 在他的 Mac 上的 CLI Pulse Bar / iOS app 中:
1. 切到 Sessions 视图
2. 点 "Start new session on <Windows-VM 设备名>"(或对应入口),输入一个工作目录(如 `C:\Users\clipulse\Documents`),provider 选 Claude
3. 提交

VM 端预期(在 ~1s 内):
- A.1 PASS → Sessions tab 出现新行,status=`pending`,然后 ~1s 后 flip 到 `running`
- A.2 PASS → Task Manager 应该看到 `claude.exe` 子进程,父进程为 `cli-pulse-desktop.exe`(或 detached 但仍在 Job Object 中,任一)
- A.3 PASS → `%LOCALAPPDATA%\dev.clipulse.desktop\logs\cli-pulse.log` 应该有一行 `Remote agent loop started`(出现在启动时,不是这次 spawn 时)+ `spawned session <8-char-prefix> provider=claude`

**也测 Windows 端 spawn 自身**:
- VM 端在 Sessions tab 顶上找 "+ Start new session" CTA(v0.8.0 新加的)
- 点击 → 弹 dialog → 输入工作目录 → 点 Start
- 同样验证 row 出现 + claude.exe 起来

## Block B — Send / Stop / Interrupt round-trip(P0 #2 验证)

⚠️ **本 block 是 v0.8.0 最关键的安全验证**

让 Mac 协助(或在 VM 自己用刚 spawn 的 session):

- B.1 Send → 输入一段简单 prompt(如 "list files in current directory"),Submit
  - VM 端 Claude 应在 ~1-2s 内开始响应(stdout 走 PTY 但本版不 upload,所以服务端不会看到内容,但 child 进程应正常工作)
  - PASS 标准:`remote-hook.log`(下面 Block D)看到一连串 hook 触发 + 决策
  
- **B.2 Interrupt → 这是 P0 #2 的核心验证**
  - 让 Claude 跑一个长任务(如 "count slowly from 1 to 100, one per second"),它开始 streaming 时点 Interrupt(或 Mac 端发 interrupt)
  - **CRITICAL PASS 标准**:`cli-pulse-desktop.exe` 进程**必须保持运行**。Task Manager 应该看到主进程仍在,只是子 claude.exe 收到中断
  - **CRITICAL FAIL 标准**:如果点 Interrupt 后 cli-pulse-desktop 整个挂掉 → 立即上报 P0,这是 v0.8.0 没修好 P0 #2 的明确信号
  - 期望行为:claude.exe 收到 Ctrl-C(0x03 字节走 PTY stdin → ConPTY 转 CTRL_C_EVENT 给伪控制台内进程组),Interrupt 中断当前操作,session 继续运行
  
- B.3 Stop → 点 Stop button
  - claude.exe 应该终止
  - status 应该 flip 到 `stopped`
  - Job Object 应该 close,清掉所有子进程

## Block C — 孤儿子进程清理(P1 验证)

- C.1 Spawn 一个新 session(同 Block A)
- C.2 强制杀 cli-pulse-desktop 主进程:
  - Task Manager → 找 cli-pulse-desktop.exe → End task(force)
  - **不要** tray Quit(那是优雅退出,会触发 graceful shutdown)
- C.3 在 ~2s 内观察 Task Manager:
  - PASS 标准:之前 spawn 的 `claude.exe` 应该也死了(Job Object KILL_ON_JOB_CLOSE 触发)
  - FAIL 标准:claude.exe 还活着 → Job Object FFI 没生效,P1 fix 失败
- C.4 重启 cli-pulse-desktop GUI
- C.5 在 ~30s 内观察 Sessions tab + 服务端 `remote_sessions`:
  - 之前 status=running 的 row 可能仍显示 running(v0.8.0 主动 defer 了 boot-time orphan reconciliation,见 CHANGELOG "Out of scope")
  - 这不是 FAIL,是 documented limitation。**但** 主进程被杀那一刻 child 应该立即死掉(C.3 验证)

## Block D — `remote-hook.log` 文件(关闭 v0.7.0 诊断盲区)

- D.1 触发 medium-risk Bash command:让 Mac 端 / 或 VM 自己的 claude session 跑 `echo hello-from-vm-080`(中等风险,会触发 hook 远程审批)
- D.2 用户在 Mac / iOS 端审批
- D.3 检查 `%LOCALAPPDATA%\dev.clipulse.desktop\logs\remote-hook.log`:
  - 应包含一连串带时间戳的行,例如:
    ```
    2026-05-07T... hook INFO hook fired: provider=claude
    2026-05-07T... hook INFO config loaded: device_id=<8-chars>
    2026-05-07T... hook INFO risk classified: medium
    2026-05-07T... hook INFO create_request POST → ok request_id=<8-chars>
    2026-05-07T... hook INFO poll: status=pending — continuing
    2026-05-07T... hook INFO decision: allow
    ```
  - PASS 标准:**有完整链路**(hook fired → config loaded → risk → create_request POST status → poll → decision)
  - FAIL 标准:文件不存在 / 是空 / 缺关键步骤(尤其缺 `create_request POST`)
- D.4 服务端 `remote_permission_requests` 表应有对应行(可让 Mac 用 Supabase MCP 查):`select created_at, summary, status from remote_permission_requests where device_id = '<vm-device-id>' order by created_at desc limit 5;`
  - 现在如果这一查 0 行,我们能从日志判断是 hook fail 还是 RPC reject — 不再是诊断盲区
- D.5 验证 file rotation(可选):用 PowerShell 给文件 append `Add-Content -Value (1..1500000 -join "x")`,再触发一次 hook,观察文件是否被截断回 ~1 行(注意 P1 #2 fix 后 set_len(0) 在 Win 上才能生效)

## Block E — 历史回归 spot-check

- E.1 v0.6.0 / v0.6.1 / v0.6.2 基础(每个版本一行验证):
  - 远程审批 modal 还能 view + decide(en / zh-CN / ja)
  - Esc-key 关 modal 在直接本地 Win 操作下应能关(VM RDP 焦点限制下若 INCONCLUSIVE 不算 FAIL)
  - Send / Stop / Interrupt 按钮在 read-only sessions list 上还能用
- E.2 v0.7.0 hook installer:
  - Settings → Privacy → "Claude permission hook" sub-section 状态正常
  - `~/.claude/settings.json` 应仍含 `--remote-approval-hook --provider claude` 钩子
- E.3 Sentry 测试事件:
  - Settings → About → "Send test event"
  - 让 Mac 用 `sentry-cli issues list --org jason-yeyuhe --project desktop --query "release:cli-pulse-desktop@0.8.0 age:-1h"` 验证(注意 issue-vs-event filter,看 reference_sentry.md 的 caveat)
- E.4 background sync 没回归:
  - 看 cli-pulse.log 应每 120s 有一行 `background sync ok`

## 已知 RDP 焦点限制(每次 verify 都要重申)

VM 通过 RDP session 控制时,某些键盘事件(尤其 Esc / Enter / 修饰键组合)未必到达 WebView2 内容区,即使 click + foreground 操作正常。如果 Esc 不关 modal 但 Cancel 按钮工作 → 标 INCONCLUSIVE 直接跳下一项,不要花时间 debug。本地 Win 机器上不是 bug。

## Block F — 关闭 VM(等 Jason 确认后再跑)

- 完成 Block A-E 后,生成 PASS/FAIL/SKIPPED/INCONCLUSIVE 的总表 + 关键截图(Block A 行出现、B.2 cli-pulse 没死、C.3 claude 死、D.3 日志内容)
- tray → Quit cli-pulse-desktop(force-kill 后备:`Stop-Process -Name "cli-pulse-desktop" -Force`)
- **不要直接 deallocate**:在 report 提交后,等 Jason 在 chat 里说 "确认 / 关 VM / go ahead"
- 收到确认后:`az vm deallocate --resource-group cli-pulse-test-rg --name clipulse-win-test`

## 报告格式

每个 block 一行 PASS/FAIL/SKIPPED/INCONCLUSIVE + 关键观察。P0/P1 立即 chat 上报,不要等 Block F。
**privacy 签名**:token / device_id / user_id / refresh_token / helper_secret / 真实 JWT 都不要原文贴入回报。device_id 可前 4-8 位 hex(如 `7e6e…`)。Sentry event id 是 Sentry 自己生成的非凭据 ID,可贴。
````

## ▲ END COPY-PASTE ABOVE THIS LINE ▲
