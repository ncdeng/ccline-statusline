# ccline-statusline

Claude Code 的状态栏程序，用 Rust 写成，一个可执行文件就能用。基于 [Haleclipse](https://github.com/Haleclipse)（哈雷）的 [CCometixLine](https://github.com/Haleclipse/CCometixLine)（MIT）改写，并加了几项功能。

```
󰓒 Opus 5.5 xhigh | 󰉋 my-project | 󰊢 main ✓ | $12.34 | 󰆼 46.7% · 466.6k/1M | ⚡ 39m59s | 106 tok/s · ttft 3.7s | 5h 27%→3h29m | 7d 51%→1d20h · Wed 10:00 · $651 2.3B tok/~$1.3k 4.51B tok
```

## 显示的内容

| 段 | 含义 |
|---|---|
| `Opus 5.5 xhigh` | 模型和推理强度 |
| `my-project`、`main ✓` | 当前目录、git 分支和状态（`●` 有改动，`⚠` 有冲突，`↑2 ↓1` 领先和落后的提交数） |
| `$12.34` | 这个会话的费用（按 API 价格）。重启 Claude Code、`/resume` 之后接着累计，不会归零 |
| `46.7% · 466.6k/1M` | 上下文用量 |
| `⚡ 39m59s` | 提示词缓存还剩多久过期。过期后下一条消息要重新写缓存，上下文大时很贵。命中率低于 95% 时会显示 `hit xx%`；缓存刚未命中的 10 分钟内会显示原因，比如 `miss: tools +466k` |
| `106 tok/s · ttft 3.7s` | 最近 8 次回复的生成速度，以及最近一次回复的首 token 等待时间 |
| `5h 27%→3h29m` | 5 小时额度用量和重置倒计时 |
| `7d 51%→1d20h · Wed 10:00` | 周额度用量、重置倒计时和重置时间（本地时间） |
| `$651 2.3B tok` | 本周期这台电脑上 Claude Code 的总用量：按 API 价格折算的美元，和总 token 数 |
| `/~$1.3k 4.51B tok` | 按“已用 ÷ 百分比”推算的整个周额度。周额度用量低于 5% 时不显示 |

终端太窄时，放不下的段会整段移到下一行，不会被 Claude Code 截成“…”。

## 和 CCometixLine 的区别

- 加了上表里的会话费用累计、缓存倒计时、速度、额度和周用量。
- 每次刷新只运行 1 个 git 进程（ccline 运行 5 个），而且仓库没变化时直接用缓存。
- 刷新一次约 13 ms（Windows 11 上测得，git 缓存命中时）。
- 模型、目录、git、费用这 4 段的样式和 ccline 1.1.2 逐字节相同，读的也是同一份 `~/.claude/ccline/config.toml`。

## 安装

### 1. 下载程序

在 [Releases](https://github.com/ncdeng/ccline-statusline/releases/latest) 页面下载对应系统的文件：

| 系统 | 文件 |
|---|---|
| Windows x64 | `ccline-statusline-x86_64-pc-windows-msvc.exe` |
| macOS（Apple 芯片） | `ccline-statusline-aarch64-apple-darwin` |
| macOS（Intel） | `ccline-statusline-x86_64-apple-darwin` |
| Linux x64 | `ccline-statusline-x86_64-unknown-linux-musl` |
| Linux ARM64 | `ccline-statusline-aarch64-unknown-linux-musl` |

也可以用命令下载。

Windows（PowerShell）：

```powershell
$dir = "$HOME\.claude\ccline"
New-Item -ItemType Directory -Force $dir | Out-Null
Invoke-WebRequest https://github.com/ncdeng/ccline-statusline/releases/latest/download/ccline-statusline-x86_64-pc-windows-msvc.exe -OutFile "$dir\ccline-statusline.exe"
```

macOS（Apple 芯片；Intel 把文件名里的 `aarch64` 换成 `x86_64`）：

```bash
mkdir -p ~/.claude/ccline
curl -fL -o ~/.claude/ccline/ccline-statusline https://github.com/ncdeng/ccline-statusline/releases/latest/download/ccline-statusline-aarch64-apple-darwin
chmod +x ~/.claude/ccline/ccline-statusline
```

Linux 同理，换成对应的文件名。

装了 Rust 的话，也可以从源码安装：`cargo install --git https://github.com/ncdeng/ccline-statusline`。程序会装到 `~/.cargo/bin/`，下一步的路径要相应改掉。

### 2. 打开 `~/.claude/settings.json`，加上 `statusLine`

如果文件里已经有 `statusLine`，就替换掉它。

Windows（路径要写完整，把 `<你的用户名>` 换成实际的用户名）：

```json
"statusLine": {
  "type": "command",
  "command": "C:/Users/<你的用户名>/.claude/ccline/ccline-statusline.exe",
  "padding": 0,
  "refreshInterval": 10
}
```

macOS / Linux：

```json
"statusLine": {
  "type": "command",
  "command": "~/.claude/ccline/ccline-statusline",
  "padding": 0,
  "refreshInterval": 10
}
```

保存后状态栏会自动重新加载。如果没有变化，就重启 Claude Code。

### 3. 字体和样式（可选）

- 图标要用 [Nerd Font](https://www.nerdfonts.com/) 字体才能显示。没有的话，要改用普通 emoji：把仓库里的 `config.toml` 复制到 `~/.claude/ccline/`，再把里面的 `mode = "nerd_font"` 改成 `mode = "plain"`。
- `~/.claude/ccline/` 里没有 `config.toml` 和 `models.toml` 时，程序用内置的默认配置，就是仓库里的这两个文件。
- 已经在用 CCometixLine 的话，程序直接读你现有的 `config.toml`。如果其中启用了这 4 段以外的段（`context_window`、`usage`、`session`、`output_style`），或者用了背景色、Powerline 分隔符，这部分就交给 `~/.claude/ccline/ccline(.exe)` 本身去画，所以要保留 ccline。

## 需要什么

- **较新的 Claude Code**：在 2.1.296 上测试过。旧版本缺少某些数据时，对应的段不显示，不会报错。
- **Claude Pro / Max 订阅**：5h / 7d 额度只有订阅账号才有。用 API key 时这两段显示 `--`。

## 它会在本机保存的数据

全部在 `~/.claude/ccline/` 里，都是小 JSON 文件：

| 文件 | 内容 |
|---|---|
| `session-cost/` | 每个会话的费用账本，每个约 100 字节 |
| `week-spend.json` | 本周期的用量统计进度，每周重置时重新开始 |
| `rate-limits-cache.json` | 最近一次的 5h / 7d 额度，刚启动还没拿到数据时用它显示 |
| `git-cache/` | git 状态缓存，避免每次刷新都运行 git |

## 注意

- **推算的周额度有两个前提**：你只在这台电脑上用 Claude Code，而且没有在 claude.ai 网页版或手机上用同一个账号。否则别处的用量不在本机统计里，推算的额度会偏低。
- **美元数会比实际少几个百分点**：后台压缩上下文这类请求不写进会话记录，统计不到。
- **价格表写在程序里**：在 `src/quota.rs` 的 `PRICES`。Anthropic 出新模型或改价以后要更新。不认识的模型不计入美元统计。
- **第一次运行会慢一点**：要先补算本周的用量，状态栏会显示几分钟带 `…` 的数，补完就正常了。
- **卸载**：删掉 `settings.json` 里的 `statusLine` 段，再删掉 `~/.claude/ccline/ccline-statusline(.exe)`。

## 从源码构建

```bash
cargo build --release   # 程序在 target/release/
cargo test
```

## 致谢

模型、目录、git、费用这 4 段的图标、配色和配置格式参考了 [Haleclipse](https://github.com/Haleclipse)（哈雷）的 [CCometixLine](https://github.com/Haleclipse/CCometixLine)（MIT），读的也是同一份 `config.toml`。想要 TUI 配置界面和多套主题，可以直接用 CCometixLine。

## License

MIT，见 [LICENSE](LICENSE)。
