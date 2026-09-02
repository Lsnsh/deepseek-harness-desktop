# 更新日志（Changelog）

本文件记录 **DeepSeek Harness Developer Preview** 的重要变更。格式遵循
[Keep a Changelog](https://keepachangelog.com/zh-CN/)，版本管理遵循
[语义化版本规范](https://semver.org/lang/zh-CN/)——开发阶段所有版本均带
`-beta` 预发布后缀。

> ⏰ 本文档中的日期与时间均为 **UTC+8（北京时间）**。

## [0.1.0-beta.11] — 2026-09-03

### 变更

- **内置 dsh 由 0.1.0-rc.6 升级到 0.1.1-rc.2**（上游共 4 个版本），要点：
  - 多模态：DeepSeek 适配器支持原生图片请求，`/goal`、`/plan` 支持图文输入，
    `@` 菜单可引用文件与会话，图片经 Files API 上传；新增
    `DeepSeek-V4-Flash-Vision-Exp` 视觉模型。
  - 子代理：Claude Code / Codex 子代理可作为 Profile Bundle 按需安装，并接入
    任务面板（Job Panel）。
  - DeepSeek 支持可选 `low` 推理强度（默认仍为 `high`）；`web_search` 支持
    并发查询。
  - 上游修复：大历史分页栈溢出、max-tokens 截断后会话不可用、超大/累积图片
    载荷请求失败、Bubblewrap 沙箱逃逸。
- 工程：定时上游检查现在能稳定自动开出依赖升级 PR（同步 lockfile、改用
  REST API 创建）——本次 dsh 升级即由它端到端产出。

## [0.1.0-beta.10] — 2026-08-16

### 修复

- **插件安装/卸载此前静默无效**：macOS WKWebView 没有 JS 对话框代理，
  `window.confirm()` 会立即返回 false 且不弹窗，导致所有安装/卸载被拦下。
  插件页改用两步武装按钮（首次点击武装并显示警告文案，再次点击确认），安装
  真正执行并流式输出；spawn 前也确保 profile 目录存在。

### 新增

- Plugin Manager 已安装列表显示 GitHub 来源插件的**仓库描述与作者头像**
  （带缓存、限流友好）；搜索卡片同样显示头像。

## [0.1.0-beta.9] — 2026-08-16

### 新增

- **VSCode 式更新流程**：后台下载更新并显示进度；「Check for Updates…」菜单在
  下载中变为「Downloading…」（禁用）、就绪后变为「Restart to Update (1)」；
  点击询问是否立即重启；取消（或未选择直接退出）则在退出时安装，下次启动即为
  新版本。
- **dsh 进程冲突防护**：启动时引导页检测到用户自启的 `dsh web`（共享 ~/.dsh
  会导致运行中会话损坏）时，可选择**接管**（杀掉该 dsh 并启动内置的）或
  **Attach**（作为浏览器连接用户的 dsh 实例）。
- Release notes 改用精简统一模板（版本 + beta 提示 + 链接化的 Full Changelog
  对比）；macOS 仅发布 DMG（自动更新归档仍生成）。

## [0.1.0-beta.8] — 2026-08-16

### 变更

- **运行时二阶段瘦身**：内置运行时归档降至约 66 MiB（原 67.3 MiB）——移除
  OTel `build/esm`+`build/esnext` 双构建、shiki `onig.wasm`（实际使用 JS
  正则引擎）、npm 的 `.package-lock.json`，gzip 提到 level 9。smoke 通过；
  启动模块追踪不变（1117 文件）。
- `verify-update` 新增**签名 key-id 一致性校验**（manifest 签名内嵌 key id
  必须匹配配置的 updater 公钥；minisign 无内置 verify 子命令，完整内容校验
  仍需手动）。

### 新增

- `docs/platform-preflight.md`——Windows/Linux 支持预研：逐文件平台盘点、
  5 个阻塞项（pnpm.cmd shim、USERPROFILE 回退、Linux 托盘、仅 AppImage
  可自更新、通知点击激活）、CI 矩阵方案（darwin/windows/linux）与建议顺序。
- 下一跳体积优化记录：运行时归档改用 `xz -9e` 可再省约 38%（41 MiB），
  但需要壳层解压命令支持 xz（`tar -xJf`；macOS/Linux/Windows bsdtar 均支持）
  ——留待下一轮决策。
- **Developer ID 代码签名 + 公证**：应用以 `Developer ID Application:
  Apple Developer ID` 签名并经 App Store Connect 公证（CI 用 GitHub
  secrets 的 API key；本地构建用钥匙串 profile——全程无明文）。不上架
  App Store。Bundle identifier 改为 `site.lsnsh.deepseek-harness-desktop`。
- **按版本 Release notes**：`docs/release-notes/v<ver>-{zh,en}.md` 由
  CHANGELOG 生成；GitHub release 描述默认用中文版（cc-switch 风格，英文版
  同步归档）。
- **机密扫描**：CI 每次推送/PR 跑 gitleaks，另有每周全量历史扫描并在发现时
  开安全 issue；本地全量扫描干净。

## [0.1.0-beta.7] — 2026-08-16

### 新增

- **Plugin Manager 二阶段**：
  - 安装/卸载**流式输出**：`dsh plugin` 的 stdout/stderr 经 Tauri 事件逐行
    推送并在插件页实时滚动显示，不再干等。
  - 已安装插件**版本检测**：从 profile 读取真实版本展示；GitHub 来源插件检测到
    更新（release/tag）时显示「有更新」徽章（10 分钟缓存以规避 GitHub 限流）。
  - 卸载失败给出明确**恢复建议**；失败也写入审计日志。
- **跨工作区会话跳转**：点击完成通知时会先探测目标会话所属工作区（走回环
  `workspace.list` API）并记录，跨工作区也照常跳转（当前前端聚合所有工作区，
  跳转可用）；探测失败则仅聚焦窗口。
- 文档：`docs/platform-preflight.md`——Windows/Linux 支持预研（逐文件盘点、
  5 个阻塞项、CI 矩阵方案）。

## [0.1.0-beta.6] — 2026-08-16

### 变更

- **发布构建改用裁剪后的运行时**（release 工作流 `DSH_DESKTOP_PRUNE=1`）：
  内置运行时归档从约 97 MiB 降至约 63 MiB（−35%）——node 二进制 strip（并
  重新签名）、node-pty 跨平台载荷、source map、`.d.ts`/`@types`、测试/文档、
  otel `sdk-trace` 均被移除；smoke 仍通过。
- CI 增加 Rust 构建缓存（`Swatinem/rust-cache`），重复 `cargo check --release`
  不再全量重编译依赖树。
- README：体积数字更新为裁剪后运行时；补充 `verify:update` 说明。

### 新增

- `scripts/verify-update.mjs`（`pnpm run verify:update`）：拉取 updater-latest
  清单，断言版本（可用 `--expect-version <v>`）与 darwin-aarch64 归档可达性。
  已接入 release 工作流作为发布后验证步骤——每次发布都会端到端校验自动更新链路。

## [0.1.0-beta.5] — 2026-08-16

### 新增

- **Plugin Manager 加固**：
  - 安装前 manifest 校验：GitHub 仓库来源的插件在安装前会拉取原始
    `package.json` 校验 `dsh.bundle` / `dsh.client` 契约——裸仓库直接拒绝并
    返回明确错误。
  - Plugin Manager 安装确认弹窗（仓库名、星数、描述、第三方代码执行警告）。
  - 安装/卸载审计日志：`$DSH_HOME/desktop-audit.log`（UTC+8、动作、插件、结果）。
- README：补充 Plugin Manager 说明；路线图中该项标记为已实现（MVP）。

## [0.1.0-beta.4] — 2026-08-16

### 新增

- **Plugin Manager**（菜单「Plugin Manager…」）：管理 dsh 插件的原生窗口——
  列出 web profile 已安装插件（含卸载按钮）、搜索 GitHub 上带 `dsh-plugin`
  标签的仓库（带缓存）、通过 `dsh plugin --profile web add <spec>` 安装。
  安装/卸载后提示「重启 Web 服务生效」。
- 开启 `withGlobalTauri`，让本地插件管理页可调用命令；远端 harness GUI 仍
  无 IPC（capabilities 仍仅限 main/plugins 窗口）。

## [0.1.0-beta.3] — 2026-08-16

### 新增

- **通知点击跳转到完成的会话**（macOS）：点击会话完成通知（或存在未查看完成
  通知时点击 Dock 图标）会通过 `?jump=<sessionId>` 与前端自身的当前会话持久化
  键，把 GUI 导航到该会话。
- **Restart Web Server** 菜单/托盘动作——重启内置 dsh web 服务，供插件
  安装/卸载（bundle 层在启动时组合）生效。
- **内置 pnpm**（运行时，`pnpm/bin/pnpm.cjs`，跑在内置 Node 上）——`dsh plugin`
  支持的基础；运行时归档约增 4.5 MiB。

## [0.1.0-beta.2] — 2026-08-16

### 变更

- Release 资产改用简短 **DSH-DP** 命名：
  `DSH-DP_{版本}_{系统}_{架构}.{扩展名}`（例如 `DSH-DP_v0.1.0-beta.2_macOS_aarch64.dmg`），
  发布列表中文件扩展名清晰可见；Release 描述末尾附上分系统/架构的下载快捷链接。
- 升级 GitHub Actions（checkout@v7、setup-node@v7、upload-artifact@v7、
  download-artifact@v8、pnpm/action-setup@v6），消除 Node.js 20 弃用警告。
- CI 触发策略：Linux 运行时产物不被下游复用，main 分支仅在代码/运行时路径变更时
  触发完整 CI；PR 与手动触发仍总是执行——内部频繁提交阶段节约 runner 资源。
- 文档时间统一标注 **UTC+8（北京时间）**。

### 修复

- 应用图标：β 开发者预览角标在其角标背景内居中显示（此前偏左）。
- 托盘图标替换为 POC 的手调参考版本（不再被裁切）；gen-icons.sh 检测到已有
  托盘图标时保留之。

## [Unreleased]

## [0.1.0-beta.1] — 2026-08-16

### 修复

- 更新清单 URL：发布工作流改用连字符资产名上传更新包。GitHub 会把上传
  资产名中的空格改写为点号，导致 `latest.json` 中 `%20` 编码的 URL
  返回 404，静默破坏自动更新。

### 变更

- 发布工作流仅构建 Apple Silicon（`macos-latest`），加快 CI/CD 循环；
  更新清单仅含 `darwin-aarch64`。


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

[unreleased]: https://github.com/Lsnsh/deepseek-harness-desktop/compare/v0.1.0-beta.11...HEAD
[0.1.0-beta.11]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.11
[0.1.0-beta.10]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.10
[0.1.0-beta.9]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.9
[0.1.0-beta.8]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.8
[0.1.0-beta.7]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.7
[0.1.0-beta.6]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.6
[0.1.0-beta.5]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.5
[0.1.0-beta.4]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.4
[0.1.0-beta.3]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.3
[0.1.0-beta.2]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.2
[0.1.0-beta.1]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.1
[0.1.0-beta.0]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.0
