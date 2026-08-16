# Windows / Linux 支持预研(platform-preflight)

> 主题:beta.8「Windows/Linux 支持预研」——只读盘点 + 实施建议。
> 作者:X4(组 2 实施)。状态:研究完成,待 X0/X3 决策后进入实施轮次。
> 本文件只做分析与方案,**不修改任何业务代码**;实施轮次另开分支/PR。

---

## 0. 摘要

当前桌面客户端是 **macOS(Apple Silicon)only**:CI 只在 `macos-latest` 构建,updater
manifest 只含 `darwin-aarch64`,托盘/通知/重启均按 macOS 打磨。向 Windows / Linux
扩展,**绝大多数逻辑已经跨平台**(Tauri 2 抽象 + `cfg!(windows)`/`cfg(target_os)` 分支
已就位),真正的阻塞项集中在 6 处:

1. **pnpm shim 是 `#!/bin/sh` 脚本**(plugins.rs)——Windows 不可执行,必须补 `.cmd` shim;
2. **`tar` / `curl` / `strip` 系统工具依赖**(server.rs 解压、download-node/download-pnpm、
   assemble-runtime 打包、plugins.rs http_get)——Windows 依赖系统自带 curl.exe/bsdtar,
   行为与 Unix 有差异,建议逐步替换为 Node 原生方案;
3. **Linux AppImage 自动更新受限**(updater.rs 逻辑本身跨平台,但 deb 安装无法自更新,
   AppImage 替换有依赖约束);
4. **notify.rs 的 `dsh_home()` 不回退 `USERPROFILE`**——Windows 上 `$HOME` 通常未设置,
   会话目录会解析到相对 cwd,通知轮询失效;
5. **通知点击跳转会话依赖 macOS `RunEvent::Reopen`**——Windows/Linux 无此事件,需新的
   激活入口(见 1.4/1.5);
6. **Linux 托盘图标依赖 libappindicator/StatusNotifier**——纯 GNOME(无扩展)可能不显示
   托盘,且 Tauri tray 图标在 Linux 不能用 template 图标语义。

CI 侧:release.yml 的 build job 是单平台硬编码,需改为矩阵 + latest.json 多平台键
(`windows-x86_64` / `linux-x86_64`),详见 §3。

---

## 1. 平台差异盘点(按文件)

### 1.1 `src-tauri/src/server.rs` — 服务器子进程管理

| 逻辑 | 现状 | 平台适配点 | 级别 |
|---|---|---|---|
| `runtime_dir()`(release) | `Command::new("tar") -xzf` 解压 `runtime.tar.gz` 到 app cache | Windows 10 1803+ 自带 bsdtar,`-xzf` 语法兼容;但依赖 PATH 上的 tar。**建议改为 Rust crate(`flate2`+`tar` 或 `tar`+`libz`)解压**,消除外部工具依赖;`tar -xOf`(archive_manifest_key)同理 | 中 |
| `home_dir()` | `#[cfg(windows)] USERPROFILE`,`#[cfg(not(windows))] HOME` | **已适配** ✓ | — |
| `spawn_server()` | `Command::new(node_path)` + `--profile web --host 127.0.0.1 --port 0` | Windows spawn `node.exe` OK;`--host 127.0.0.1` 已固定,无平台差异 | — |
| `parse_port()` / `http_get_ok()` | 原生 `TcpStream` + 文本解析 | 跨平台一致 | — |
| stdio 管道 | `Stdio::piped()` | 跨平台一致 | — |

### 1.2 `src-tauri/src/tray.rs` — 系统托盘

| 逻辑 | 现状 | 平台适配点 | 级别 |
|---|---|---|---|
| 图标 | `include_image!("icons/tray.png")` + `icon_as_template(true)` | **macOS 专用**:template 图标是单色模板,由系统渲染;Windows/Linux 的 tray 图标用普通彩色 PNG,`icon_as_template(true)` 在非 mac 平台会被忽略(或表现异常),需按平台选择图标/去掉 template | 中 |
| 点击事件 | `TrayIconEvent::Click`(左键显示窗口) | 跨平台 API 一致;Linux 上左键事件因 DE 差异可能不触发,需同时保留菜单入口 | 低 |
| 托盘可用性 | 依赖 `tauri` crate 的 `tray-icon` feature | **Linux 需要 libappindicator3 / StatusNotifier 支持**:GNOME 默认不显示 AppIndicator,需 `AppIndicator and KStatusNotifierItem` 扩展;tauri 文档明确 Linux 托盘依赖该服务 | **阻塞(Linux)** |
| 菜单 | `MenuItem` 文本菜单 | Windows/Linux 支持;中文/英文文案保持 | — |

### 1.3 `src-tauri/src/updater.rs` — 自动更新

| 逻辑 | 现状 | 平台适配点 | 级别 |
|---|---|---|---|
| 检查/下载/安装 | `updater.check()` + `download_and_install()` | tauri-plugin-updater 跨平台;**安装行为按平台不同**:macOS 替换 `.app`(现网已跑通);Windows 安装 NSIS 生成的更新包(下载 `.nsis.zip` 解压覆盖,无需交互);**Linux 仅 AppImage 可自更新**(替换 AppImage 文件),**deb/rpm 安装的无法自更新**(需 root 重装) | **阻塞(Linux 安装方式决策)** |
| 签名 | 同一 Ed25519 key(`TAURI_SIGNING_PRIVATE_KEY`)签 updater 归档 | **跨平台同一 key**:Windows `.nsis.zip.sig` / Linux `.AppImage.tar.gz.sig` 均用同一 key 签,当前配置即可复用 | — |
| 代码签名 | macOS 无 Developer ID 配置(ad-hoc) | **Windows 无 Authenticode 证书 → SmartScreen 警告**;Linux 无代码签名。内部测试可接受,公测前需决策证书采购 | 中(策略) |
| `download_and_install` 进度 | 空回调 | 跨平台一致 | — |

### 1.4 `src-tauri/src/notify.rs` — 会话完成通知

| 逻辑 | 现状 | 平台适配点 | 级别 |
|---|---|---|---|
| `dsh_home()` | `DSH_HOME` → `$HOME/.dsh` → `.dsh` | **Windows 上 `$HOME` 通常未设置 → 回退到相对 cwd 的 `.dsh`,轮询失效**。需补 `USERPROFILE` 回退(server.rs 的 `home_dir()` 已有此分支,notify.rs 漏了) | **阻塞(Windows)** |
| zstd 解码 | `zstd` crate | 跨平台 | — |
| 通知点击 | 注释:macOS 点击激活 app → `RunEvent::Reopen` → `jump_to_last()` | **Windows/Linux 无 Reopen**:Windows 点击通知激活已运行窗口(需 `single_instance` 回调或前台窗口事件),Linux libnotify 点击回调支持有限。`jump_to_last` 目前只在 Reopen 触发——Windows/Linux 需新的激活入口(见 1.5) | **阻塞(Windows/Linux,体验级)** |
| 轮询间隔/开关 | env 控制 | 跨平台 | — |

### 1.5 `src-tauri/src/lib.rs` — 壳层

| 逻辑 | 现状 | 平台适配点 | 级别 |
|---|---|---|---|
| `local_app_url()` | `#[cfg(target_os = "macos")] tauri://localhost`;非 mac 用 `http://tauri.localhost` | **已适配** ✓(Tauri 2 标准;Windows 需 WebView2 的 host 映射,Tauri 自动处理) | — |
| `RunEvent::Reopen` | `#[cfg(target_os = "macos")]` | **仅 macOS**:Windows/Linux 的通知/图标激活无此事件。建议抽象一个 `on_activate()`(macOS=Reopen;Windows/Linux=窗口 Focused 事件 + single_instance 回调 + 托盘点击),统一调 `tray::show_window` + `notify::jump_to_last` | **阻塞(Windows/Linux,体验级)** |
| `windows_subsystem` | `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` | **已适配** ✓(Windows 无控制台窗口) | — |
| 菜单 | `SubmenuBuilder`(Edit/Window 等) | Windows/Linux 菜单渲染在窗口内(非全局菜单栏),行为一致 | — |
| `single_instance` | `tauri_plugin_single_instance` | Windows(mutex)/Linux(socket)均支持 | — |
| Plugin Manager 窗口 + IPC | `invoke_handler` 自定义命令 + 本地 `plugins.html` | 跨平台一致 | — |
| `INIT_JUMP_SCRIPT` | localStorage 写 `dsh.sessions.current` | 跨平台一致 | — |

### 1.6 `src-tauri/src/plugins.rs` — 插件管理(业务,仅记录适配点)

| 逻辑 | 现状 | 平台适配点 | 级别 |
|---|---|---|---|
| **pnpm shim** | 写 `pnpm/bin/pnpm`(`#!/bin/sh` 脚本,exec 打包的 node + pnpm.cjs) | **Windows 上 `#!/bin/sh` 不可执行**;`spawnSync("pnpm")` 在 Windows 按 PATHEXT 找 pnpm.exe/.cmd/.bat。需同时生成 `pnpm.cmd`(或 .bat)shim,内容 `@"node" "pnpm.cjs" %*` | **阻塞(Windows)** |
| `http_get`(GitHub 搜索/manifest 校验) | `Command::new("curl")` | Windows 10 1803+ 自带 curl.exe;但依赖系统工具。建议后续换 Rust `reqwest`/`ureq`(或 Node helper) | 低 |
| `pnpm` 子进程 | `Command::new("node") … dsh plugin` | 跨平台一致 | — |
| 审计日志/搜索缓存 | `$DSH_HOME/desktop-audit.log` + 内存缓存 | 路径解析与 notify.rs 相同问题(见 1.4) | 中 |

### 1.7 `src-tauri/tauri.conf.json` / `Cargo.toml` / `capabilities` / `icons`

| 项 | 现状 | 适配点 | 级别 |
|---|---|---|---|
| `bundle.targets: "all"` | macOS→app/dmg;Windows→msi/nsis;Linux→deb/rpm/appimage | 各平台自动展开,无需改 | — |
| `createUpdaterArtifacts: true` | 各平台生成对应 updater 归档 | 无需改 | — |
| `bundle.icon` | 32x32/128x128/128x128@2x/icns/ico | **Linux 打包需要 PNG 图标**(tauri 2 从 icon 列表选 PNG):32/128 已够,建议补 512x512(`icon.png` 已存在但未列入数组,核对是否被自动采用) | 低 |
| `resources: runtime.tar.gz` | 平台特定(内嵌平台 node 二进制) | **每平台构建各自生成自己的 runtime.tar.gz**,无需改 | — |
| updater `endpoints` | `…/updater-latest/latest.json` | 多平台共用同一 manifest,客户端按 target 取对应平台键 | — |
| `Cargo.toml` | 无平台特定依赖;`zstd` 跨平台 | 无需改 | — |
| `capabilities` | `core:default`,`windows: [main, plugins]` | 跨平台一致 | — |
| `icons/` | 已有 `icon.png`、tray.png 等 | tray.png 是 macOS 单色模板;Windows/Linux 托盘建议用彩色变体(可复用 icon.png) | 低 |

### 1.8 `scripts/assemble-runtime.mjs` — 运行时装配

| 逻辑 | 现状 | 适配点 | 级别 |
|---|---|---|---|
| `installApp()` | `npm install --omit=dev`(npm 布局=真实目录) | **跨平台 OK**(CI 的 ci.yml 已在 ubuntu 上跑通 assemble+smoke) | — |
| `pruneApp()` | node-pty 按 `process.platform-arch` 保留当前平台 prebuild;junk sweep | **已跨平台**(按当前平台剪裁) | — |
| `stripNodeBin()` | `platform !== 'darwin'` 跳过;darwin 用 strip+codesign | **已适配**;注释已提出 Linux 可用 `strip --strip-unneeded`(可选优化,见 X3 分工) | — |
| 打包 | `execFileSync('tar', ['-czf', …])` | Windows 用系统 bsdtar(兼容),但依赖外部工具;建议后续换 Node `zlib`+`tar` 纯 JS 打包 | 中 |
| `manifest.json` | `platform: ${process.platform}-${process.arch}` | **已跨平台** | — |
| pnpm 下载 | `downloadPnpm()` | 见 1.9 | — |

### 1.9 `scripts/download-node.mjs` / `scripts/download-pnpm.mjs`

| 逻辑 | 现状 | 适配点 | 级别 |
|---|---|---|---|
| `distFileName()` | darwin/win32/linux 三平台都有(win 用 zip) | **已跨平台** ✓ | — |
| 下载 | `execFileSync('curl', …)` | Windows 自带 curl.exe(1803+);建议后续换 Node 原生 `fetch` + 流式写文件(verify-update.mjs 已有 fetch 先例) | 中 |
| 解压 | `execFileSync('tar', ['-xf'/'-xzf', …])`(bsdtar 同时处理 tar.gz/zip) | Windows bsdtar 可解两者;zip 用 bsdtar 解在 Windows 上验证过吗?CI 未跑过 Windows → **需在 windows-latest 上实测**(计划 §4) | 中 |
| `chmodSync` | `platform !== 'win32'` 才 chmod | **已适配** ✓ | — |
| checksum | `SHASUMS256.txt` + sha256 | 跨平台 | — |

### 1.10 `scripts/smoke.mjs` / `scripts/build-local.sh`

| 文件 | 现状 | 适配点 | 级别 |
|---|---|---|---|
| smoke.mjs | `spawn(node, [appEntry, '--profile web', …])` + HTTP GET | 跨平台;`child.kill('SIGKILL')` 在 Windows 无 SIGKILL 语义(Node 近似处理),超时兜底可改用 `child.kill()`;Windows 上如强杀失败用 `taskkill /T /F` | 低 |
| build-local.sh | bash,安装到 /Applications | **macOS 专用**:Windows/Linux 本地构建脚本需另写(pwsh / bash 用于 WSL);发布轮次前非必须 | 低(仅本地体验) |

### 1.11 `.github/workflows/` — CI/CD

| 文件 | 现状 | 适配点 | 级别 |
|---|---|---|---|
| ci.yml | ubuntu 上跑 assemble+smoke+cargo check(Linux 依赖已装:webkit2gtk-4.1、libappindicator3、librsvg2、patchelf) | **已证明 Linux 构建链可用**;后续可加 windows-latest 验证 job(WebView2 无需装) | — |
| release.yml | **macOS 单平台硬编码**(build job `runs-on: macos-latest` + `--bundles app,dmg`;latest.json 只含 darwin-aarch64;资产命名/校验/发布均 macOS) | 见 §3 矩阵方案;beta.8 发布仍 macOS-only,矩阵后续轮次启用 | — |
| check-upstream.yml | ubuntu 跑 npm registry 检查 | 跨平台 | — |

---

## 2. 阻塞项与风险清单(分级)

### 🔴 阻塞(必须先解决才能出 Windows/Linux 版本)

| # | 项 | 位置 | 说明 |
|---|---|---|---|
| B1 | **pnpm shim 仅 `#!/bin/sh`** | plugins.rs `pnpm_shim_dir` | Windows 无法执行;需生成 `pnpm.cmd`。这是 Windows 插件管理功能的第一阻塞 |
| B2 | **notify.rs `dsh_home()` 无 `USERPROFILE` 回退** | notify.rs | Windows 上会话目录解析错误,通知轮询完全失效 |
| B3 | **Linux 托盘依赖 AppIndicator** | tray.rs + CI 依赖 | GNOME 默认不显示;AppImage 用户需装扩展;文档需写明,打包依赖 libappindicator3 已在 ci.yml |
| B4 | **Linux 自更新仅 AppImage 可行** | updater.rs + 发布决策 | deb/rpm 无法自更新;AppImage 替换要求以可写路径运行。发布形态决策:Linux 出 AppImage(+deb 静态安装) |
| B5 | **通知点击跳转依赖 macOS Reopen** | lib.rs/notify.rs | Windows/Linux 需新的激活入口(§1.5 的 `on_activate()` 抽象),否则"点击通知→跳转会话"在 Win/Linux 缺失 |

### 🟡 风险(不阻塞首版,但影响体验/稳健)

| # | 项 | 位置 | 说明 |
|---|---|---|---|
| R1 | 外部工具依赖(curl/tar) | server.rs、download-*.mjs、assemble-runtime.mjs、plugins.rs | Windows 系统自带 curl.exe(1803+)与 bsdtar,基本可用;但行为/报错与 Unix 不同,且未来 Windows 精简镜像可能缺失。建议分阶段替换:先实测,再逐步改 Node 原生 |
| R2 | tray 图标 template 语义 | tray.rs | macOS 的 `icon_as_template(true)` 对 Win/Linux 无意义,需按平台选择图标 |
| R3 | Windows SmartScreen 警告 | 发布 | 无 Authenticode 证书;内部测试可接受,公测前决策 |
| R4 | Linux AppImage 图标尺寸 | tauri.conf.json | 核对 icon.png(512)是否被 Linux bundler 采用 |
| R5 | smoke.mjs SIGKILL | smoke.mjs | Windows 上强杀语义不同,超时兜底需验证 |
| R6 | linux-x86_64 glibc 基线 | CI | ubuntu-22.04 构建的 AppImage 兼容较老发行版;若目标老系统需降 runner |

### 🟢 已就位(无需改)

`home_dir()`(server.rs)、`windows_subsystem`、`local_app_url()` 平台分支、`distFileName()`
三平台资产、`nodeBinName()`、`chmodSync` 平台分支、`pruneApp()` 平台剪裁、`manifest.json`
platform 字段、`single_instance`、zstd crate、ci.yml 的 Linux 依赖、Cargo.toml 无平台依赖。

---

## 3. CI 多平台扩展方案(release.yml)

> 方案只写设计;**beta.8 发布保持 macOS-only**,矩阵后续轮次启用(避免未验证平台破坏发布)。

### 3.1 build job 改为矩阵

```yaml
build:
  needs: [version]
  strategy:
    fail-fast: false
    matrix:
      include:
        - target: darwin-aarch64
          os: macos-latest
          bundles: app,dmg
          updater_key: darwin-aarch64
          asset_os: macOS
          asset_arch: aarch64
          archive_glob: '*.app.tar.gz'
          sig_suffix: '.sig'
        - target: windows-x86_64
          os: windows-latest
          bundles: nsis
          updater_key: windows-x86_64
          asset_os: Windows
          asset_arch: x86_64
          archive_glob: '*.nsis.zip'      # createUpdaterArtifacts 生成的更新归档
          sig_suffix: '.sig'
        - target: linux-x86_64
          os: ubuntu-22.04
          bundles: appimage,deb
          updater_key: linux-x86_64
          asset_os: Linux
          asset_arch: x86_64
          archive_glob: '*.AppImage.tar.gz'
          sig_suffix: '.sig'
  runs-on: ${{ matrix.os }}
  steps:
    # …checkout / pnpm / node / rust 同现状…
    - name: Install system dependencies (Linux)
      if: matrix.target == 'linux-x86_64'
      run: sudo apt-get update && sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
    # …pnpm install / pnpm build / pnpm run runtime (prune) / pnpm run smoke…
    # tauri build --bundles ${{ matrix.bundles }}
    # 资产重命名按矩阵:DSH-DP_${TAG}_${asset_os}_${asset_arch}.{ext}
    - uses: actions/upload-artifact@v7
      with:
        name: desktop-${{ matrix.target }}
        path: src-tauri/target/release/bundle/
```

要点:
- 每平台一个 `upload-artifact`(name 带 target),release job 用 `download-artifact` + `merge-multiple: true` 合并(现状已支持);
- Windows:WebView2 由系统/安装器提供,无需安装;NSIS 安装器由 tauri 自动生成;
- 签名:三个平台共用同一 `TAURI_SIGNING_PRIVATE_KEY`(Ed25519 签 updater 归档),env 照旧;
- **Windows 上 `pnpm run runtime` 需先修 B1/B2 之外的工具链(R1:curl/tar 实测)**;
- 资产重命名脚本需按矩阵参数化(现状是 macOS 硬编码)。

### 3.2 release job — latest.json 多平台键

```bash
jq -n \
  --arg version "$VERSION" --arg pub_date "$pub_date" \
  --arg darwin_sig "$(cat …darwin-aarch64…sig)" \
  --arg darwin_url "$base/DSH-DP_${TAG}_macOS_aarch64.app.tar.gz" \
  --arg win_sig "$(cat …windows-x86_64…sig)" \
  --arg win_url "$base/DSH-DP_${TAG}_Windows_x86_64.nsis.zip" \
  --arg linux_sig "$(cat …linux-x86_64…sig)" \
  --arg linux_url "$base/DSH-DP_${TAG}_Linux_x86_64.AppImage.tar.gz" \
  '{version:$version, notes:"…", pub_date:$pub_date, platforms:{
     "darwin-aarch64":{signature:$darwin_sig,url:$darwin_url},
     "windows-x86_64":{signature:$win_sig,url:$win_url},
     "linux-x86_64":{signature:$linux_sig,url:$linux_url}
   }}' > latest.json
```

- **updater 平台键**:`darwin-aarch64` / `windows-x86_64` / `linux-x86_64`(tauri-updater 按
  target triple 匹配);
- 资产 URL 用 updater 归档(`.app.tar.gz` / `.nsis.zip` / `.AppImage.tar.gz`),不是安装器;
- "Verify required assets" 按矩阵 glob 循环检查;
- 发布 notes 增加 Windows/Linux 下载段;
- **verify-update.mjs 需扩展**:循环验证 `platforms` 下所有键的 url 可达(当前只查 darwin-aarch64;
  脚本已支持任意 manifest URL,扩展逻辑后兼容旧行为)。

### 3.3 启用节奏(建议)

1. 本轮(beta.8):只交付本预研文档;CI 不动;
2. 下一轮:修 B1/B2 + Windows 实测工具链(在 windows-latest 跑一次完整 build 看阻塞),ci.yml
   加 windows 验证 job;release.yml 加矩阵但 `if: false`/注释保持 macOS-only;
3. 再下一轮:矩阵全开,beta 发布验证三平台更新链。

---

## 4. 建议实施顺序(MVP)

| 步骤 | 内容 | 归属 | 验证 |
|---|---|---|---|
| 1 | pnpm shim 补 `pnpm.cmd`(B1) | X4/Rust | Windows 上跑 `dsh plugin` 安装一个测试插件 |
| 2 | notify.rs `dsh_home()` 补 `USERPROFILE`(B2) | X4/Rust | Windows 会话完成出通知 |
| 3 | `on_activate()` 抽象 + Win/Linux 激活入口(B5) | X4/Rust | 点击通知/托盘恢复窗口并跳转 |
| 4 | tray 图标按平台选择、去 template(R2) | X4/Rust | Win/Linux 托盘图标正确显示 |
| 5 | server.rs 解压改 Rust crate、download-* 改 fetch+纯 JS 解压(R1) | X4/脚本 | 三平台 assemble+smoke 全绿 |
| 6 | ci.yml 加 windows-latest 验证 job | X3(CI) | windows CI 全绿 |
| 7 | release.yml 矩阵 + latest.json 多平台键(§3) | X3(CI)+X4(verify-update 扩展) | 三平台 beta 发布 + verify-update 全键通过 |
| 8 | Linux 发布形态决策(AppImage only vs +deb)+ 文档 | X0 决策 | 发布清单更新 |
| 9 | Windows 代码签名证书、SmartScreen 策略 | X0 决策 | 公测前 |

文件边界(与 X3 协调):**X4 负责 src-tauri/src/* 的 Rust 适配 + 本文档**;X3 负责
scripts/*(assemble/download/smoke 的跨平台改造)+ .github/workflows 的 CI 矩阵。
(注:本轮 X4 只出文档,不落代码;实施轮次按上表分工。)

---

## 5. 附录:updater 平台键与资产对照(tauri-plugin-updater v2)

| 平台 | latest.json 键 | 更新归档(createUpdaterArtifacts) | 签名文件 | 安装器 |
|---|---|---|---|---|
| macOS arm64 | `darwin-aarch64` | `*.app.tar.gz` | `.sig` | .dmg |
| Windows x86_64 | `windows-x86_64` | `*.nsis.zip`(或 `*.msi.zip`) | `.sig` | NSIS .exe(或 MSI) |
| Linux x86_64 | `linux-x86_64` | `*.AppImage.tar.gz` | `.sig` | AppImage / deb |

- 三个平台共用同一 Ed25519 签名 key(现 `TAURI_SIGNING_PRIVATE_KEY`);
- pubkey 已在 tauri.conf.json 的 updater 插件配置中,跨平台复用;
- 客户端按自身 target 匹配键;缺失某键则该平台"无更新"。

---

## 6. 参考资料

- 现状代码:src-tauri/src/{server,tray,updater,notify,lib,plugins}.rs;tauri.conf.json;
  scripts/{assemble-runtime,download-node,download-pnpm,smoke,build-local}.*;.github/workflows/{release,ci,check-upstream}.yml
- Tauri 2:Linux 依赖(webkit2gtk-4.1/libappindicator3)、tray 平台差异、updater 平台键
  (v2 文档 "Update manifests");AppImage 自更新限制(v2 已知问题)
- Node 官方二进制分布:nodejs.org/dist(win zip / linux tar.gz),脚本已三平台覆盖
