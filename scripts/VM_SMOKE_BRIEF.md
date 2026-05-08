# Smoke 验证任务 — cli-pulse-desktop

你是 cli-pulse-desktop 在 Windows VM (clipulse-win-test) 上的
发布验证员。运行环境是 Windows Server 2025 / x86_64,你有
PowerShell + RDP + GitHub 的 raw download 权限。

## 背景

2026-05-07,v0.8.0 上线后在这台 VM 上每次启动 ~5–12 s 后 BEX64
崩溃(`STATUS_STACK_BUFFER_OVERRUN` / `0xc0000409`),Latest tag
已回退到 v0.7.0,v0.8.0 标 YANKED。Sentry 后来确认根因是
`tokio::spawn` 在 Tauri setup 钩子里被调用,而 setup 钩子不在
tokio runtime 上下文里 → 主线程 panic-abort。修复路径:

- v0.8.1 = 完全 revert ConPTY 功能
- v0.8.2 = v0.8.1 + 顺手修了一个 v0.7.0 起就有的 stderr pipe panic

**你这次要验证的是 v0.8.2**(prerelease 状态,VM 可以匿名下载)。

## 你的目标

确认 v0.8.2 在 VM 上 **能启动 + 保持运行 ≥ 60s 不崩溃**。

仅此一条。功能验证不在本轮范围 — 只要进程能活下来,Mac 端就会
把 v0.8.2 promote 到 Latest;真活不下来就再次回退。

## 怎么做

具体怎么操作交给你判断。建议路径(但不强制):

1. 看一下 VM 上当前装的是哪个版本,卸载干净
2. 从 GitHub 下载 v0.8.2 安装包,URL 模式:
   `https://github.com/JasonYeYuhe/cli-pulse-desktop/releases/download/v0.8.2/CLI.Pulse_0.8.2_x64-setup.exe`
3. 静默安装(NSIS 接受 `/S` 参数)
4. 启动 GUI(默认装到 `C:\Program Files\CLI Pulse\cli-pulse-desktop.exe`)
5. 等 60s
6. 验证两件事:
   - 进程还活着(`Get-Process cli-pulse-desktop`)
   - Windows Event Log Application 类**没有**新的 cli-pulse-desktop
     Application Error 事件(关键 — BEX64 / 0xc0000409 复发的话
     会落在这里)

## 已知陷阱

- VM 通过 RDP 控制时,有时 PowerShell 命令对窗口操作的反馈会
  滞后 — 看进程是否活用 `Get-Process` 比看窗口可靠
- 不要直接运行带 `-Verb RunAs` 之类提权的命令,VM 已经是 admin
  user 登录
- 卸载 NSIS 包时如果第一次没起作用,卡 5–10s 就可以认为它在
  后台跑,不要重复触发卸载器

## 报告格式

最后回 chat 一段话,**结尾必须有一行**纯字母的判定:

```
PASS
```

或

```
FAIL
```

或

```
DEGRADED
```

(DEGRADED = 进程没了但有别的实例在跑,例如 supervisor 自动重启
的迹象 — 不算清洁通过,需要人工介入。)

证据怎么贴你随意,但请包含:
- 安装的版本号(`(Get-Item <exe>).VersionInfo.FileVersion`)
- 60s 后的进程 PID
- 如果 FAIL,贴 WER 事件的 Provider/EventID/前 3 行 Message

## Privacy

VM 上的 device_id / helper_secret / refresh_token / JWT 不要原文
贴回报。Sentry event id 是非凭据 ID,可以贴。版本号、PID、
exception code、faulting module 名字都安全。
