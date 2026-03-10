


# OpenBolt 跨平台协同系统架构与实现规范 (Technical Specification)

本文档提供 OpenBolt (Thunderbolt Share 开源替代方案) 的具体技术实现规范。剔除了所有非技术描述，专为作为 AI (如 ChatGPT/Claude) 生成代码的上下文（Context Prompt）而设计。

---

## 1. 系统架构与技术栈 (Tech Stack)

*   **GUI 框架:** Tauri v2 (跨平台，原生 OS 交互能力强，包体积小)
*   **前端:** React (TypeScript) + TailwindCSS
*   **后端核心:** Rust (Edition 2021)
*   **异步运行时:** `tokio`
*   **通信架构:**
    *   底层控制面: `mDNS` (设备发现), `gRPC` 或 REST (`axum`) (指令下发)
    *   数据面: TCP Streaming (文件), UDP/RTP (屏幕镜像/KVM)
*   **核心外部依赖 (Rust Crates):**
    *   系统网络: `pnet` / `sysinfo` / `windows-rs` (Windows) / `core-foundation` (macOS)
    *   服务发现: `mdns-sd`
    *   HTTP 服务: `axum`, `reqwest`
    *   文件系统: `tokio::fs`, `notify` (监听变化)
    *   剪贴板: `arboard` (基础文本/图片), OS Native API (文件对象)

---

## 2. 模块实现规范 (Module Specifications)

### ✅ 2.1 网络发现与链路层 (Network & Link Layer)
**目标:** 无需用户干预，自动接管并配置雷电网卡，建立 `10.99.99.0/24` 子网连接。

**✅ 2.1.1 网卡识别逻辑 (NIC Identification)**
*   **Windows:**
    *   调用 `windows-rs` 库中的 `GetAdaptersAddresses` API。
    *   过滤条件: `IfType == IF_TYPE_ETHERNET_CSMACD` 且 `FriendlyName` 包含关键字 `Thunderbolt`, `雷电`, 或驱动描述匹配 Intel Thunderbolt Networking。
*   **macOS:**
    *   执行系统命令或调用 `SystemConfiguration` 框架。
    *   过滤条件: 查找 `Device` 为 `bridge0` 且 `Hardware Port` 包含 `Thunderbolt Bridge` 的接口。

**✅ 2.1.2 静态 IP 配置 (IP Configuration)**
需要提权 (UAC / `sudo` 提权弹窗)。设定本端随机 IP `10.99.99.[2-254]`。
*   **Windows (Rust `std::process::Command`):**
    ```powershell
    netsh interface ip set address name="<NIC_NAME>" static 10.99.99.x 255.255.255.0
    ```
*   **macOS (Rust `std::process::Command`):**
    ```bash
    networksetup -setmanual "Thunderbolt Bridge" 10.99.99.x 255.255.255.0
    ```

**✅ 2.1.3 服务发现 (Service Discovery - `mdns-sd`)**
*   **Service Type:** `_openbolt._tcp.local.`
*   **TXT Records:** `{ "os": "windows/macos", "hostname": "DESKTOP-X", "kvm_port": "xxxx", "api_port": "yyyy" }`
*   **逻辑:** 监听局域网。当发现对端且 IP 为 `10.99.99.x` 段时，触发握手事件。

### ✅ 2.2 KVM 子进程管理 (KVM Daemon Manager)
**目标:** 包装第三方 KVM 核心（推荐 `lan-mouse` 或编译好的 `input-leap` CLI 工具）。

**✅ 2.2.1 进程生命周期管理**
*   使用 Rust `tokio::process::Command` 管理子进程。
*   必须使用 `Stdio::piped()` 捕获 `stdout/stderr` 以便在 Tauri 界面显示日志。
*   **Host端 (提供鼠标):**
    *   启动命令示例: `lan-mouse --daemon --server --bind 10.99.99.x:4242`
*   **Client端 (接收鼠标):**
    *   启动命令示例: `lan-mouse --daemon --client --connect 10.99.99.y:4242`
*   **进程清理:** 注册系统信号处理器 (`Ctrl+C` 或应用退出事件)，强制杀死 KVM 子进程，防止僵尸进程占用端口。

### ✅ 2.3 高速文件传输 API (`axum` Server)
**目标:** 利用 TCP 跑满 20Gbps 带宽。使用 RESTful 架构供前端双面板 UI 调用。

**✅ 2.3.1 API 路由定义 (绑定到 `10.99.99.x`)**
*   `GET /api/fs/list?path=<URL_ENCODED_PATH>`
    *   **Response:** JSON 数组。`[{"name": "a.txt", "is_dir": false, "size": 1024, "mtime": 1690000000}, ...]`
*   `GET /api/fs/download?path=<URL_ENCODED_PATH>`
    *   **实现:** 使用 `tokio_util::io::ReaderStream` 包装 `tokio::fs::File`。
    *   **Response:** `Transfer-Encoding: chunked`, `Content-Type: application/octet-stream`。实现零拷贝级别的高效流传输。
*   `POST /api/fs/upload?path=<DEST_DIR>`
    *   **实现:** 接收 `multipart/form-data` 或 raw stream。使用 `tokio::io::copy` 将 stream 写入到 `tokio::fs::File::create` 句柄中。

**✅ 2.3.2 跨设备拖拽与文件剪贴板 (黑客级实现)**
*   **Windows Hook (`windows-rs`):**
    *   监听剪贴板格式 `CF_HDROP`。
    *   解析出绝对路径。调用对端 API (`POST /api/fs/upload`) 推送文件到对方的 `~/.openbolt/temp/`。
*   **macOS Hook (`NSPasteboard` via `objc` crate):**
    *   监听 `NSPasteboardTypeFileURL`。
    *   接收文件完毕后，Rust 调用 macOS API 将 `~/.openbolt/temp/<filename>` 写入本地 `NSPasteboard`，并将状态设为 copy。

### ✅ 2.4 文件夹实时同步机制 (File Sync Engine)
**目标:** 监听特定目录变动并增量同步。

**✅ 2.4.1 文件系统监听 (`notify` crate)**
*   初始化 `notify::RecommendedWatcher`。
*   监听模式: `RecursiveMode::Recursive`。
*   **防抖机制 (Debounce):**
    *   创建 `tokio::sync::mpsc` 通道接收事件。
    *   在接收端使用 `tokio::time::timeout` 实现 500ms 到 1000ms 的聚合。丢弃中间的 `Modify` 事件，只保留最终的变动结果。

**✅ 2.4.2 同步策略**
*   获取变动文件的完整路径。
*   对比远端 (`GET /api/fs/stat?path=...`) 获取对端文件的修改时间和大小。
*   若本地更新，调用 `POST /api/fs/upload` 覆盖远端文件。

### ✅ 2.5 屏幕共享封装 (Screen Sharing Wrapper)
**目标:** 自动化配置 Sunshine 和 Moonlight，对用户隐藏细节。

**✅ 2.5.1 服务端 (Windows - Sunshine)**
*   **配置文件修改:** Rust 启动前读取 Sunshine 的 `sunshine.conf`。
*   强制写入参数:
    *   `min_log_level = 1`
    *   `address = 10.99.99.x` (强制绑定雷电网卡，防止外网暴露)
    *   `port = 47989` (或自定义端口以避开冲突)
*   **启动与停止:** `tokio::process::Command::new("sunshine.exe").spawn()`。

**✅ 2.5.2 客户端 (macOS - Moonlight)**
*   使用命令行直接唤起 Moonlight 连接。
*   唤起命令 (`std::process::Command`):
    ```bash
    # 假设内置了 Moonlight-QT
    ./moonlight stream 10.99.99.x --audio-on-host --display-mode windowed
    ```

---

## 3. OS 权限要求与打包规范 (Permissions & Packaging)

### ✅ 3.1 macOS (`Info.plist` & Entitlements)
应用必须配置以下权限，否则 KVM 和投屏将直接 Crash：
*   **Accessibility (无障碍):** 用于 `lan-mouse` 模拟鼠标键盘输入。
    `<key>NSAppleEventsUsageDescription</key>`
    `<string>Need accessibility to control mouse and keyboard.</string>`
*   **Screen Recording (屏幕录制):** 用于 Sunshine/投屏捕获画面。
    `<key>NSScreenCaptureUsageDescription</key>`
    `<string>Need screen capture for display sharing.</string>`

### ✅ 3.2 Windows (Application Manifest)
*   必须配置 UAC 提权以允许配置网卡。
*   修改 `tauri.conf.json` (或通过 `windows-rs` 构建脚本) 嵌入 manifest：
    `<requestedExecutionLevel level="requireAdministrator" uiAccess="false" />`

---

## 4. 给 AI 的代码生成指南 (AI Code Generation Prompting Guide)

当要求 AI 生成代码时，请**每次仅截取上述文档的一个小节（如 2.3.1）**，并附带以下系统级提示词：

> **System Prompt:**
> "你是一个精通 Rust 和跨平台系统级编程的高级工程师。请严格按照上述提供的技术规范（Technical Specification）生成代码。
> 要求：
> 2. 使用 `tokio` 进行异步处理。
> 3. 对系统 API 的调用必须处理平台差异 (`#[cfg(target_os = "windows")]` 等)。
> 4. 所有可能阻塞的操作必须放在 `tokio::task::spawn_blocking` 中。
> 5. 所有的 Error 必须实现自定义错误枚举，并派生 `thiserror::Error`。
> 6. 不要使用伪代码，给出完整的、可编译的 Rust 模块代码。"