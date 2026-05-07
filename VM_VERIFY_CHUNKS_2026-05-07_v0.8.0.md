# VM verify chunks — v0.8.0 (paste-one-at-a-time)

Each chunk below is a complete, self-contained prompt for a **fresh** Claude Code session on `clipulse-win-test`. Run them in order. After each chunk, close the session before pasting the next one.

---

## ▼ Chunk 1: Sanity + upgrade to v0.8.0 (~5 min)

````
你是 cli-pulse-desktop 在 Azure Windows VM (clipulse-win-test) 的发布验证员。本次任务:把 VM 从 v0.7.0 升级到 v0.8.0(已 promote 为 Latest)并做 sanity 检查。这是 v0.8.0 verify 的第 1/5 chunk。

## 任务

1. PowerShell 跑 `Get-StartApps | Where-Object { $_.Name -like "*CLI Pulse*" }` 记录当前版本(应为 v0.7.0)
2. 启动 v0.7.0 GUI,header 应在 ~30s 内出现 "Update available" banner(latest.json 已指向 v0.8.0)
3. 点 banner → Settings → 等下载完 → 接受 relaunch
4. v0.8.0 起来后,Settings → About 应显示 `0.8.0 windows`
5. 如果 banner-click 失败,fallback:Mac 端用 `gh release download v0.8.0 --pattern "*x64-setup.exe" --dir ...` 取 NSIS,转传 VM 后 `Start-Process -Wait -FilePath "...x64-setup.exe" -ArgumentList "/S"`,再启动验证

## 报告

PASS / FAIL / INCONCLUSIVE 一行 + 关键截图。Privacy: device_id / helper_secret / JWT 不要原文贴入回报。
````

---

## ▼ Chunk 2: Block A spawn + Block B Send/Interrupt (P0 #2 — 核心安全验证) (~10 min)

````
你是 cli-pulse-desktop v0.8.0 在 Azure Windows VM 的验证员,本次跑 Block A(spawn 管理会话)+ Block B(Send / Interrupt round-trip)。Chunk 2/5。**Block B.2 是 v0.8.0 的核心 P0 安全验证 — 关键中之关键。**

前提:VM 上 cli-pulse-desktop 已是 v0.8.0(Chunk 1 完成)。

## Block A — 本机 spawn 管理会话

1. Sessions tab 顶部有新 CTA "+ Start new session",点开
2. 输入 cwd(如 `C:\Users\clipulse\Documents`),provider 选 Claude,提交
3. **预期 ~1s 内**:
   - A.1 PASS → Sessions tab 出现新 row,status=`pending`,然后 ~1s flip 到 `running`
   - A.2 PASS → Task Manager 看到 `claude.exe` 子进程
   - A.3 PASS → `%LOCALAPPDATA%\dev.clipulse.desktop\logs\cli-pulse.log` 有 `spawned session <8-char> provider=claude`

## Block B — Send / Interrupt(P0 #2)

4. 用上面 spawn 出来的 session,点 Send,输入 prompt:`count slowly from 1 to 100, one per second`,提交
5. Claude 开始 streaming 时(最好在 5-10 之间),点 Interrupt 按钮
6. **CRITICAL PASS 标准**:
   - B.1 → claude.exe 收到 Ctrl-C,中断当前操作,session 状态保持 running
   - **B.2 → `cli-pulse-desktop.exe` 主进程必须保持运行**(Task Manager 验证)。如果主进程消失 → 立即 P0 上报,这是 v0.8.0 没修好 P0 #2 的明确信号
7. **暂时不要点 Stop**,session 留着给 Chunk 3 用

## 报告

A.1 / A.2 / A.3 / B.1 / B.2 各一行 PASS/FAIL,B.2 是 P0 立即 chat 上报。Privacy 同 Chunk 1。
````

---

## ▼ Chunk 3: Block C — 孤儿子进程清理(Job Object 验证)(~3 min)

````
你是 cli-pulse-desktop v0.8.0 在 Azure Windows VM 的验证员,本次跑 Block C(Job Object KILL_ON_JOB_CLOSE)。Chunk 3/5。

前提:Chunk 2 留下了一个 running 的 session(claude.exe 是 cli-pulse-desktop.exe 的子进程,经 Job Object 注册)。

## 任务

1. Task Manager 找到 `cli-pulse-desktop.exe` 主进程
2. **强制杀**(不要 tray Quit,那是优雅退出):右键 → End task(Force)
3. **~2s 内**观察 Task Manager:
   - C.1 PASS → 之前 spawn 的 `claude.exe` 也死了(Job Object KILL_ON_JOB_CLOSE 触发,内核级 cleanup)
   - C.1 FAIL → claude.exe 还活着 → 立即 P1 上报(Job Object FFI 没生效)
4. 重启 cli-pulse-desktop GUI(从 Start Menu)
5. ~30s 内观察 Sessions tab:之前 status=running 的 row 可能仍显示 running — 这不是 FAIL,v0.8.0 主动 defer 了 boot-time orphan reconciliation(见 CHANGELOG "Out of scope")。但 C.1 主进程死那一刻 child 立即死,是关键

## 报告

C.1 PASS / FAIL 一行 + Task Manager 截图(可选)。Privacy 同前。
````

---

## ▼ Chunk 4: Block D 日志文件 + Block E 历史回归(~10 min)

````
你是 cli-pulse-desktop v0.8.0 在 Azure Windows VM 的验证员,本次跑 Block D(remote-hook.log,关闭 v0.7.0 诊断盲区)+ Block E(历史回归 spot-check)。Chunk 4/5。

前提:cli-pulse-desktop v0.8.0 已重启(Chunk 3 重启后)。

## Block D — `remote-hook.log` 文件验证

1. 让 Mac / iOS 端发起 / 或 VM 自己用 Sessions tab spawn 一个 claude session,给它发 medium-risk Bash:`echo hello-from-vm-080`
2. Mac / iOS 端审批
3. 检查 `%LOCALAPPDATA%\dev.clipulse.desktop\logs\remote-hook.log`:
   - D.1 PASS → 包含完整链路(每行带 RFC3339 时间戳):
     ```
     ... hook INFO hook fired: provider=claude
     ... hook INFO config loaded: device_id=<8-chars>
     ... hook INFO risk classified: medium
     ... hook INFO create_request POST → ok request_id=<8-chars>
     ... hook INFO poll: status=pending — continuing
     ... hook INFO decision: allow
     ```
   - D.1 FAIL → 文件不存在 / 空 / 缺关键步骤(尤其 `create_request POST`)
4. (可选)D.2:Mac 用 Supabase MCP 查 `select created_at, summary, status from remote_permission_requests where device_id = '<vm>' order by created_at desc limit 5;` 应有对应行

## Block E — 历史回归 spot-check

5. E.1 远程审批 modal 还能 view + decide(en / zh-CN / ja 至少一种语言)— 1 行 PASS / FAIL
6. E.2 Settings → Privacy → "Claude permission hook" 状态正常,`~/.claude/settings.json` 仍含 `--remote-approval-hook --provider claude`
7. E.3 Sentry test event:Settings → About → "Send test event",让 Mac 端 sentry-cli 验证(注意 issue-vs-event filter)
8. E.4 background sync:`cli-pulse.log` 应每 120s 有 `background sync ok`

## 已知 RDP 焦点限制(don't waste time)

VM 通过 RDP 控制时 Esc / Enter 等键盘事件未必到 WebView2。如果 Esc 不关 modal 但 Cancel 按钮工作 → INCONCLUSIVE,跳下一项。

## 报告

D.1 / D.2 / E.1 / E.2 / E.3 / E.4 各一行 PASS/FAIL/INCONCLUSIVE。Privacy 同前。
````

---

## ▼ Chunk 5: Block F — 关闭 VM(等 Jason 确认后再跑)

````
你是 cli-pulse-desktop v0.8.0 在 Azure Windows VM 的验证员,本次任务:整理总报告 + 关闭 VM。Chunk 5/5。

前提:Chunks 1-4 已跑完,各 block 结果已发给 Jason。

## 任务

1. 整理 PASS/FAIL/SKIPPED/INCONCLUSIVE 总表(Phase 0 / Phase 1 / Block A / B.1 / B.2 / C / D.1 / D.2 / E.1 / E.2 / E.3 / E.4)
2. tray → Quit cli-pulse-desktop(force-kill 后备:`Stop-Process -Name "cli-pulse-desktop" -Force`)
3. **不要直接 deallocate** — 在 chat 提交总报告后,等 Jason 说 "确认关 VM" / "go ahead" 再跑下面命令
4. 收到确认后:`az vm deallocate --resource-group cli-pulse-test-rg --name clipulse-win-test`

## Privacy 签名

token / device_id / user_id / refresh_token / helper_secret / 真实 JWT 不要原文贴入回报。device_id 可前 4-8 位 hex(如 `7e6e…`)。Sentry event id 是非凭据 ID,可贴。
````

---

## 使用建议

- **必跑**:Chunks 1, 2, 5。Chunk 2 的 B.2 是 v0.8.0 的 P0 验证。
- **强烈推荐**:Chunk 3(Job Object 验证)+ Chunk 4 的 D.1(关闭 v0.7.0 诊断盲区)。
- **可选**:Chunk 4 的 Block E。如果 Chunk 1-4 都 PASS,E 是低风险的回归 spot-check。
- 每 chunk 跑完报告再开新 session,context 不会累计。
