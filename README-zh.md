<div align="center">

# 🐳 DeepSeek Harness Developer Preview

**All-in-one 原生桌面客户端 · 内置 Node.js 运行时与 deepseek-harness**

[![release](https://img.shields.io/github/v/release/Lsnsh/deepseek-harness-desktop?include_prereleases&label=version&style=flat-square)](https://github.com/Lsnsh/deepseek-harness-desktop/releases)
[![ci](https://img.shields.io/github/actions/workflow/status/Lsnsh/deepseek-harness-desktop/ci.yml?branch=main&label=CI&style=flat-square)](https://github.com/Lsnsh/deepseek-harness-desktop/actions/workflows/ci.yml)
[![check-upstream](https://img.shields.io/github/actions/workflow/status/Lsnsh/deepseek-harness-desktop/check-upstream.yml?branch=main&label=upstream%20check&style=flat-square)](https://github.com/Lsnsh/deepseek-harness-desktop/actions/workflows/check-upstream.yml)
[![license](https://img.shields.io/github/license/Lsnsh/deepseek-harness-desktop?style=flat-square)](LICENSE)

</div>

## 这是什么

**DeepSeek Harness Developer Preview** 是 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 的**原生桌面客户端**（All-in-one 架构）：

- 📦 **无需安装 Node.js** —— 自带 Node.js LTS 运行时（v24.19.0）
- 🚀 **开箱即用** —— 内置最新版 `@deepseek-ai/dsh`，启动即跑 `dsh web`
- 🖥️ **原生体验** —— Tauri 2 原生窗口、系统托盘、原生菜单
- 🔔 **会话完成通知** —— 会话回合完成后发送系统通知，点击回到窗口
- 🔄 **自动更新** —— 通过 GitHub Releases 分发，启动时静默检查，菜单可手动检查

> ⚠️ 这是**社区维护的第三方桌面客户端**，与 DeepSeek 官方无关，仅供开发与研究使用。
> 当前处于 **beta 开发预览阶段**，仅支持 **macOS（Apple Silicon）**。

## 特性

| 能力 | 说明 |
| --- | --- |
| 内置运行时 | 打包 Node.js LTS v24.19.0 + 完整 `@deepseek-ai/dsh` 生产依赖（约 97 MiB 压缩归档） |
| 本地优先 | 会话与设置保存在本地（默认 `~/.dsh`），服务绑定 `127.0.0.1` |
| 单实例 | 重复启动会聚焦已有窗口，不重复拉起服务 |
| 托盘常驻 | 关闭窗口最小化到托盘，后台服务继续运行 |
| 自动更新 | 启动 3 秒后静默检查更新；菜单「Check for Updates…」手动检查；更新清单经签名校验 |
| 会话通知 | 轮询会话存储，回合完成（`turn/end`，reason 为 completed/error）时发系统通知 |
| 插件管理 | 管理 GitHub `dsh-plugin` topic 插件：列表/搜索/安装/卸载；安装前校验 manifest（`dsh.bundle`/`dsh.client` 契约）、安装确认、审计日志（`~/.dsh/desktop-audit.log`） |
| 外链跳转 | GUI 内的外部链接交给系统浏览器，窗口只访问本地服务 |
| 错误兜底 | 内置服务启动失败时展示错误页，日志写入系统日志目录 |

## 下载与安装

前往 [Releases](https://github.com/Lsnsh/deepseek-harness-desktop/releases) 下载 DMG 安装包。

> macOS 未签名/未公证，首次打开需在「系统设置 → 隐私与安全性」中允许，或右键 → 打开。

## 开发

```bash
# 安装依赖
pnpm install

# 装配内置运行时（下载 Node v24.19.0 + 生产安装 @deepseek-ai/dsh）
pnpm run runtime

# 冒烟测试：像桌面端一样启动服务并请求根路径
pnpm run smoke

# 运行开发模式
pnpm run tauri:dev

# 构建发布包（.app + .dmg）
pnpm run tauri:build

# 检查上游 @deepseek-ai/dsh 是否有新版本
pnpm run check:upstream
```

### 目录结构

```
├── scripts/                  # 运行时装配与工具脚本
│   ├── download-node.mjs     # 下载并校验内置 Node.js 二进制
│   ├── assemble-runtime.mjs  # 生产安装 @deepseek-ai/dsh → resources/runtime
│   ├── smoke.mjs             # 以桌面端相同方式启动并验证服务
│   ├── check-upstream.mjs    # 检查 npm 上游版本（供定时工作流使用）
│   ├── gen-icons.sh          # 生成应用图标 / 托盘图标
│   └── gen-signer-key.sh     # 生成更新签名密钥对
├── resources/                # 构建产物（git 忽略）：runtime/ + runtime.tar.gz
├── src-tauri/                # Tauri 2 外壳（Rust）
│   └── src/
│       ├── lib.rs            # 入口、菜单、窗口生命周期
│       ├── server.rs         # 启动/监督 dsh web、等待就绪、导航窗口
│       ├── tray.rs           # 系统托盘
│       ├── updater.rs        # 自动更新（启动静默 + 菜单手动）
│       └── notify.rs         # 会话完成通知（轮询会话存储）
├── public/error.html         # 内置服务启动失败的兜底页
├── index.html                # 启动页（splash）
└── .github/workflows/        # CI / 上游检查 / 发布
```

### 架构：如何工作

1. `scripts/assemble-runtime.mjs` 用 npm 在临时目录**生产安装** `@deepseek-ai/dsh`（真实目录，无符号链接），连同下载的 Node.js 二进制一起打包为 `resources/runtime.tar.gz`（约 97 MiB）。
2. 发布构建将归档作为 Tauri 资源打进应用。
3. 应用首次启动时把归档解压到缓存目录（按内容哈希命名，升级自动失效）。
4. Rust 端用内置 Node 启动 `dsh --profile web --port 0`（系统随机端口），监听标准输出中的就绪地址行 `dsh web: http://127.0.0.1:<port>`，轮询确认服务健康后导航主窗口。
5. 窗口只允许访问本地服务 origin；外部链接交给系统浏览器。
6. 后台线程每 2 秒扫描 `~/.dsh/sessions/**/session.jsonl.zstd`，检测到新的 `turn/end`（completed/error）时发系统通知。

### 更新签名

自动更新需要签名密钥对（minisign）：

```bash
pnpm run signer-key
```

- 私钥保存在 `~/.tauri/dsh.key`，**切勿提交**；
- 公钥粘贴进 `src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey`；
- CI 构建时通过 Secrets `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 签名更新包。

### 发布流程

1. 更新 `package.json` 与 `src-tauri/Cargo.toml` 的版本号（必须带 `-beta` 后缀），更新 `CHANGELOG.md` / `CHANGELOG-zh.md`，提交推送。
2. 在 GitHub Actions 手动触发 `release` 工作流（或填写版本号覆盖）。
3. 工作流在 macOS（Apple Silicon，`macos-latest`）上构建、签名、发布 GitHub Release（`vX.Y.Z-beta.N`，pre-release），并附加 `latest.json` 更新清单。
4. 客户端启动时与菜单「Check for Updates…」读取该清单完成自动更新。

### 上游同步

`.github/workflows/check-upstream.yml` 每天定时检查 npm 上 `@deepseek-ai/dsh` 的最新版本：

- 有新版本 → 自动创建/更新「依赖升级 PR」，CI 验证通过后合并；
- 也可手动触发：`gh workflow run check-upstream.yml`。

## 路线图

- [x] macOS 端：开发 / CI / 部署 / 自动更新全流程
- [ ] 会话完成通知点击后**跳转到对应会话**
- [ ] Windows / Linux 支持
- [x] dsh plugin 管理（MVP：从 GitHub `dsh-plugin` topic 列表/搜索/安装/卸载，含安装前 manifest 校验与审计日志）
- [ ] 更小的安装包体积（按需裁剪运行时）

## 致谢

- [deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) —— 引擎与本客户端的服务端
- [Tauri](https://tauri.app) —— 原生外壳框架

## License

[MIT](LICENSE)
