# 更新日志（Changelog）

本文件记录 **DeepSeek Harness Developer Preview** 的重要变更。格式遵循
[Keep a Changelog](https://keepachangelog.com/zh-CN/)，版本管理遵循
[语义化版本规范](https://semver.org/lang/zh-CN/)——开发阶段所有版本均带
`-beta` 预发布后缀。

## [Unreleased]

### 新增

- `check-upstream` 工作流：每天定时检查 npm 上 `@deepseek-ai/dsh` 是否有新版本；
  有更新时自动创建/更新「依赖升级 PR」。
- CI 工作流：每次推送/PR 执行前端构建、运行时装配、运行时冒烟测试与
  `cargo check`。
- 发布工作流：在 macOS（Apple Silicon）构建并签名应用，发布带
  `vX.Y.Z-beta.N` 的 pre-release，并附上签名后的 `latest.json` 更新清单。

## [0.1.0-beta.0] — 2026-08-16

### 新增

- 面向 DeepSeek Harness 的原生 macOS 桌面外壳（Tauri 2）：
  - All-in-one 架构：自带 Node.js LTS 运行时（v24.19.0）与 `@deepseek-ai/dsh`
    生产安装（`dsh web`），打包为单个压缩归档（约 97 MiB），首次启动解压到
    应用缓存目录。
  - 内置服务生命周期：用内置 Node 启动 `dsh --profile web --port 0`，等待就绪
    地址行、轮询服务健康后导航主窗口；服务输出镜像到系统日志目录；失败时展示
    专用错误页。
  - 导航围栏：窗口只访问本地服务 origin；外部链接交给系统浏览器。
  - 系统托盘：关闭窗口隐藏到托盘、服务继续运行；托盘菜单可恢复窗口或退出；
    单击图标显示窗口（macOS Dock 点击同样生效）。
  - 自动更新：启动 3 秒后静默检查 + 菜单「Check for Updates…」手动检查；
    从 GitHub Releases 读取签名清单（`latest.json`）；`DSH_DESKTOP_AUTO_UPDATE=0`
    可关闭启动检查。
  - 会话完成通知：后台轮询 `~/.dsh/sessions/**/session.jsonl.zstd`，检测到新的
    `turn/end`（reason 为 completed 或 error）时发送系统通知；点击通知显示并
    聚焦窗口。可调项：`DSH_DESKTOP_NOTIFY=0`、`DSH_DESKTOP_NOTIFY_INTERVAL_MS`。
  - 单实例保护；原生应用菜单（关于对话框含构建溯源、编辑角色支持复制粘贴、
    窗口角色）；自定义应用图标与托盘图标。
- 运行时装配工具：
  - `scripts/download-node.mjs` — 固定的 Node.js LTS 二进制（v24.19.0，支持
    `DSH_DESKTOP_NODE_VERSION` 覆盖），幂等。
  - `scripts/assemble-runtime.mjs` — 用 npm 干净地生产安装 `@deepseek-ai/dsh`
    （真实目录布局，天然自包含），生成带结构哈希的清单与单个
    `runtime.tar.gz`。
  - `scripts/smoke.mjs` — 以与桌面端完全相同的方式启动装配好的运行时并验证
    服务根路径响应。
  - `scripts/check-upstream.mjs` — 供定时上游检查使用的 npm 版本对比。
  - `scripts/gen-icons.sh`、`scripts/gen-signer-key.sh` — 图标与更新签名密钥工具。
- 文档：双语 README（`README.md` / `README-zh.md`）与本更新日志
  （`CHANGELOG.md` / `CHANGELOG-zh.md`），MIT 许可。

[unreleased]: https://github.com/Lsnsh/deepseek-harness-desktop/compare/v0.1.0-beta.0...HEAD
[0.1.0-beta.0]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.0
