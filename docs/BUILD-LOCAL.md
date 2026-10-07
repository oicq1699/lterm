# lterm 本地构建指南（无 CI，全部自产）

产物形态：**默认只出便携版可执行文件**（单文件，前端资源已内嵌）。日常构建一律用 `npm run tauri build -- --no-bundle`，不再生成 deb/rpm/AppImage/NSIS 安装包（除非另行要求）。

---

## 一、Windows 10 x64 构建机 — 需要安装的组件

### 必装（4 项）
| # | 组件 | 用途 | 下载 |
|---|---|---|---|
| 1 | **Git for Windows** | 取代码 | https://git-scm.com/download/win |
| 2 | **Node.js LTS (≥20，含 npm)** | 前端构建 (Vite/Svelte) | https://nodejs.org |
| 3 | **Visual Studio 2022 Build Tools**，勾选 workload「使用 C++ 的桌面开发」(MSVC v143 + Windows SDK) | Rust MSVC 工具链的编译器/链接器 | https://visualstudio.microsoft.com/zhdownloads/ 页面选 Build Tools |
| 4 | **Rust (rustup)**，安装时选默认 `x86_64-pc-windows-msvc` | 后端编译 | https://rustup.rs |

### 运行侧需要（1 项）
| 组件 | 说明 |
|---|---|
| **WebView2 Runtime** | Tauri 运行时。Win10 较新的更新一般已内置；「程序和功能」里找 *Microsoft Edge WebView2 Runtime*，没有就装微软常青独立安装包 |

### 明确不需要
- ~~NASM / CMake / Perl~~（已把 russh 加密后端从 aws-lc-rs 换成 ring，只有 MSVC 就够）
- ~~MinGW / GNU 工具链~~（不要用 GNU target，wry/tauri 对 MSVC 支持最稳）
- 打安装包**不需要**单独装 NSIS/WiX：`tauri build` 首次会自动下载 NSIS（要求构建时能访问 GitHub；访问不了就用便携版，见下）

### 构建命令（PowerShell）
```powershell
cd lterm
npm install
npm run tauri build -- --no-bundle    # 便携版：只出 exe，不下载 NSIS
# 产物：src-tauri\target\release\lterm.exe  ← 单文件拷到任何 Win10/11 机器直接双击运行

# 可选：安装包（需要 GitHub 网络）
npm run tauri build                    # 产物在 src-tauri\target\release\bundle\{msi,nsis}\
```

### 国内网络加速（建议先配）
```powershell
# cargo 镜像
mkdir $env:USERPROFILE\.cargo -Force
@'
[source.crates-io]
replace-with = "rsproxy"
[source.rsproxy]
registry = "https://rsproxy.cn/crates.io-index"
registries.rsproxy.url = "https://rsproxy.cn/crates.io-index"
[net]
git-fetch-with-cli = true
'@ | Set-Content $env:USERPROFILE\.cargo\config.toml

# npm 镜像
npm config set registry https://registry.npmmirror.com
```

### 拷贝源码到 Win10 的注意
排除这三个目录（体积大且平台相关）：`src-tauri/target`、`node_modules`、`.svelte-kit`。
配置/数据落在 `%APPDATA%\lterm\data\`（servers.json / known_hosts / panic.log）。

---

## 二、Linux（mint / 本容器）构建

### 组件（一行装完）
```bash
sudo apt install -y curl git build-essential pkg-config libssl-dev \
  libwebkit2gtk-4.1-dev librsvg2-dev libayatana-appindicator3-dev
curl -fsSL https://sh.rustup.rs | sh -s -- -y
# Node LTS：官方源或 apt 自带 ≥18 即可
```

### 构建
```bash
cd lterm
npm install
cargo tauri build --bundles deb     # .deb（已验证可产）
cargo tauri build                   # 全部（appimage 需能下载 linuxdeploy，网络受限时跳过）
```

### 便携版
`src-tauri/target/release/lterm` 单文件 ELF，拷到 mint 直接运行
（依赖系统 webkit2gtk-4.1，mint 22+ 默认可装）。

---

## 三、Windows 注意事项（与构建无关的，都标了）

**构建/测试相关**
1. `cl.exe/link.exe not found` → VS Build Tools 没装「C++ 桌面开发」workload，或 rustup 装成了 GNU host
2. 测试零前置条件：密钥已内嵌进 `tests/e2e.rs`，`cargo test` 在新 Windows 上直接 7/7 绿（ssh-agent、sshd 用例自动跳过，不需要任何服务）
3. 若 Windows 上编译报错，大概率集中在 `auth.rs` 的 cfg(windows) 分支（命名管道/Pageant），把输出发回来即可修

**运行功能相关（不是编译要求）**
4. 要用「ssh-agent 登录」这个功能时，需要 Windows 的「OpenSSH Authentication Agent」服务在跑：`Get-Service ssh-agent` → `Start-Service ssh-agent` + `Set-Service ssh-agent -StartupType Automatic`；Pageant（PuTTY 系）作兜底已实现。密码/私钥登录不依赖任何服务

## 四、当前已产出物（本容器）
- `src-tauri/target/release/bundle/deb/lterm_0.1.0_amd64.deb`（4.4MB，含全部功能）
- `src-tauri/target/release/lterm`（Linux 便携 ELF）
