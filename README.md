# DashDrop ⚡

> **极速、加密、无需配置的跨平台近场文件传输工具**
> Blazing-fast, encrypted, zero-config local file sharing — for macOS, Windows, and Linux.

Current project status (single source of truth): [STATUS.md](./STATUS.md)

---

## 📌 当前状态（截至 2026-03-09）

- 构建与测试：
  - `cargo check` 已接入 CI 并通过
  - `cargo clippy --all-targets --all-features -- -D warnings` 已接入 CI
  - `cargo test` 已接入 CI 并通过
  - `npm run build` 已接入 CI 并通过
  - `npm run test:e2e`（Playwright UI E2E）已接入 CI 并通过
  - `npm run test:e2e:contract`（契约脚本级 E2E）已接入 CI 并通过
  - 已新增安全扫描：`security-audit.yml`（cargo audit + npm audit）+ GitHub Code Scanning default setup
  - 已新增 `dependabot.yml`（Actions/npm/cargo 依赖周更）
  - 已新增安装包发布流水线增强：标准化产物命名、烟测、可选签名/公证钩子、发布校验和
  - 已新增发布与升级模板文档：`docs/RELEASE_NOTES_TEMPLATE.md`、`docs/UPGRADE_MIGRATION_TEMPLATE.md`
- 状态契约：
  - 已收口进行中/终态事件契约，已包含 `transfer_accepted`
  - `revision` 仅在状态跃迁递增，`transfer_progress` 不再推动 revision 递增
- 架构边界：
  - 已引入稳定 DTO（`DeviceView/SessionView/TransferView`），前端不再依赖 `SocketAddr/Instant`
- 功能补齐：
  - 已实现 `connect_by_address`（返回远端指纹摘要并进入确认/可选配对流程）
  - 已实现 trusted-only auto-accept（`auto_accept_trusted_only`）
  - 已实现 Reachable Probe（ALPN `dashdrop-probe/1`）与 `Offline` 状态收敛（15s 宽限）
  - History 终态自动刷新、incoming 大小格式化、本地筛选已完成
  - History 已支持时间窗口筛选（24h/7d/30d）
  - Transfers 已支持批量取消活动任务与发送任务重试
  - PartialCompleted 场景已支持“失败文件级重试”（不是整任务重发）
  - Trusted Devices 已支持配对时间、最近使用时间与别名编辑
  - Settings 已支持冲突策略（重命名/覆盖/跳过）与并发流配置
  - 指标已持久化聚合（平均耗时/失败分布/终态计数/收发字节）
  - 持久化已收口为 SQLite（`state.json` 仅用于一次性迁移读取）
- 安全闭环：
  - 已落地 `security_events` 审计存储与查询命令
  - `fingerprint_changed` / `identity_mismatch` 均有 UI 告警消费路径
  - Linux/Windows keyring 不可用时支持降级并在设置页显示高风险提示
- 用户体验：
  - 已提供首启 Onboarding（本地持久化关闭）
- 仍在进行：
  - 双端 QUIC 多机编排压测扩展（当前已有跨模块合同测试）
  - 真实 Tauri runtime 级 E2E 编排（当前 Playwright 为 mock IPC 驱动）

---

## 🎯 Vision

DashDrop 的目标是把 AirDrop 的核心体验带到每一台桌面电脑上，无论它运行的是 macOS、Windows 还是 Linux。

不需要扫码、不需要账号、不需要 USB 线、不需要打开浏览器。

只要打开 DashDrop，**拖拽，松手，完成。**

---

## ✨ 核心体验指标

| 指标 | 目标值 |
|------|--------|
| 发现延迟 | 打开即见，< 2 秒看到附近设备 |
| 传输速度 | 满跑局域网带宽（Wi-Fi 6 下 > 500 Mbps） |
| 睡眠唤醒恢复 | 自动重连 < 3 秒 |
| 安装包体积 | < 10 MB（Tauri 轻量优势） |
| 内存占用 | 日常待机 < 30 MB |

---

## 🗺️ 产品路线图

### MVP — v0.1（同一局域网内核心流程跑通）

> 目标：文件能发出去、能收到、加密、有进度反馈

- [x] 项目骨架初始化（Tauri 2.0 + Vue 3 + Rust）
- [x] mDNS 局域网广播与设备发现（`_dashdrop._udp.local`）
- [x] QUIC + TLS 1.3 加密传输通道
- [x] Ed25519 设备长期密钥对（系统凭据存储）
- [x] 单/多文件发送（顺序传输 + BLAKE3 整文件校验）
- [x] 接收请求队列卡片（Transfers 页处理，Accept / Decline）
- [x] 主界面设备卡片列表（实时发现、上下线感知）
- [x] 拖拽发送（单通道事件模型）
- [x] 传输进度条与完成通知

> **MVP 不包含**：断点续传（v0.2）、开机自启、系统托盘、带外身份验证

### v0.2 — 可靠性与安全
- [ ] 断点续传（块级 BLAKE3 + SQLite 持久化块清单）
- [ ] 带外验证（二维码配对码，解决首次配对 MITM 问题）
- [ ] 信任设备体系（免确认自动接收）

### v1 — 体验精打磨
- [ ] 图片/视频缩略图预览
- [ ] 传输历史记录
- [ ] 右键菜单集成（Windows Shell Extension）
- [ ] macOS Finder Quick Action
- [ ] 开机自启与系统托盘

### v2 — 进阶
- [ ] BLE 近场发现
- [ ] 临时热点直连 fallback
- [ ] 剪贴板同步

---

## 🏗️ 系统架构

```
DashDrop（单进程 Tauri 应用）
├── 前端 UI（WebView 线程）
│   ├── 设备发现列表（实时 mDNS 推送）
│   ├── 拖拽 DropZone
│   ├── 接收请求队列（Incoming Requests）
│   └── 传输进度
│
├── Discovery（Rust tokio 异步任务）
│   ├── mdns-sd 广播自身服务
│   └── 多网卡监听，多会话设备聚合
│
├── Transport Core（Rust tokio 异步任务）
│   ├── quinn (QUIC) Server — 监听传入连接
│   ├── quinn (QUIC) Client — 发起传输
│   └── BLAKE3 整文件完整性校验
│
└── Identity & Crypto（Rust）
    ├── Ed25519 设备长期密钥对
    ├── TLS 自签证书（rcgen）
    └── 系统安全存储（Keychain / DPAPI / keyring）
```

**进程模型说明**：所有模块运行于同一进程内，UI 线程与后端异步任务相互隔离。进程退出时正在进行的传输会终止；MVP 不支持后台守护模式。详见 [ARCHITECTURE.md](./ARCHITECTURE.md)。

---

## 🛠️ 技术栈

| 层 | 技术 | 理由 |
|----|------|------|
| 应用框架 | **Tauri 2.0** | 极小体积（< 10 MB），调用原生系统 API |
| 前端 | **Vue 3 + TypeScript** | 生态成熟，开发高效 |
| 样式 | **TailwindCSS v4** | 极速开发，统一 Design Token |
| 动画 | **CSS @keyframes** | 无额外依赖，与 Vue 原生集成 |
| 图标 | **Lucide Vue Next** | 精简现代图标库 |
| 状态管理 | **Pinia** | Vue 官方推荐 |
| 后端语言 | **Rust** | 极低内存、极高性能、跨平台编译 |
| 传输协议 | **QUIC (quinn crate)** | 低延迟、多路复用、内置 TLS |
| 服务发现 | **mDNS (mdns-sd crate)** | 纯代码实现，不依赖系统 Avahi/Bonjour |
| 序列化 | **CBOR (ciborium crate)** | 紧凑二进制，serde 生态兼容 |
| 加密 | **Ed25519 (ed25519-dalek) + rcgen** | 密钥对 + 自签证书 |
| 文件完整性 | **BLAKE3** | 高速哈希，整文件传输后校验 |
| 运行时 | **Tokio** | Rust 异步生态事实标准 |

---

## 🔒 安全设计

DashDrop MVP 采用 **"局域网物理接近信任"** 模型（LAN Proximity Trust）：

- **端到端加密**：所有传输走 QUIC + TLS 1.3，局域网截包无法获得明文
- **设备身份绑定**：连接建立后，强制校验 TLS 证书 fingerprint 与 mDNS 广播 fp 一致
- **诚实的首次配对提示**：UI 明确告知用户"首次连接无法自动验证身份，请确认对方在你身边"
- **已配对设备告警**：重连时 fingerprint 变化会触发安全告警
- **文件落地隔离**：接收文件只写入 `Downloads/DashDrop`

> **注意**：MVP 阶段首次配对无法防御同一 LAN 上的主动 MITM 攻击。带外身份验证（二维码配对）将在 v0.2 实现。详见 [PROTOCOL.md](./PROTOCOL.md)。

---

## 🧩 模块接口（Tauri IPC Commands）

```typescript
// 前端调用后端
invoke('get_local_identity')                       // 返回本机 name/fingerprint/port
invoke('get_devices')                              // 返回当前在线设备列表
invoke('send_files_cmd', { peerFp, paths })        // 发起传输
invoke('connect_by_address', { address })          // 手动地址连接+身份摘要
invoke('accept_transfer', { transfer_id })
invoke('reject_transfer', { transfer_id })

// 后端推送给前端
listen('device_discovered', handler)   // 有新设备或设备信息更新
listen('device_lost', handler)         // 设备所有会话均已离线
listen('transfer_incoming', handler)   // 收到传输请求（含来源fp、是否已配对）
listen('transfer_progress', handler)   // 传输进度更新
listen('transfer_complete', handler)   // 传输成功（Ack ok=true 收到）
listen('transfer_partial', handler)    // 部分成功（成功+失败混合）
listen('transfer_rejected', handler)   // 被对端拒绝
listen('transfer_cancelled_by_sender', handler)   // 发送方取消
listen('transfer_cancelled_by_receiver', handler) // 接收方取消
listen('transfer_failed', handler)     // 失败（仅零成功）
listen('identity_mismatch', handler)   // TLS fp 与 mDNS fp 不一致
listen('fingerprint_changed', handler) // 已配对设备证书更换
```

---

## 📁 项目结构

```
dashdrop/
├── src/                    # Vue 3 前端
│   ├── components/
│   │   ├── DeviceCard.vue       # 设备卡片（已配对/首次连接/离线）
│   │   ├── DropZone.vue         # 拖拽区域
│   │   ├── IncomingRequestCard.vue # Transfers 页接收请求卡片（设计基线）
│   │   └── TransferProgress.vue # 传输进度
│   ├── stores/
│   │   └── dashdrop.ts          # Pinia 状态管理
│   ├── App.vue
│   └── main.ts
│
├── src-tauri/src/          # Rust 后端
│   ├── main.rs
│   ├── lib.rs
│   ├── crypto.rs           # Ed25519 密钥对 + 自签证书 + fp 绑定校验
│   ├── state.rs            # AppState（多会话设备建模）
│   ├── discovery.rs        # mDNS 广播与多网卡发现
│   └── transport/
│       ├── mod.rs          # 协议消息类型（Hello/Offer/Chunk/Complete/Ack）
│       ├── server.rs       # QUIC 接收端
│       └── client.rs       # QUIC 发送端
│
├── ARCHITECTURE.md          # 详细架构文档（v0.4）
├── PROTOCOL.md              # 协议规范（当前版本见文档头）
├── CONTRIBUTING.md          # 贡献指南
└── README.md                # 本文件
```

---

## 🚀 开发启动

### 前提条件
- [Rust](https://rustup.rs/) 1.75+
- [Node.js](https://nodejs.org/) 20+
- Tauri 系统依赖（[官方文档](https://tauri.app/start/prerequisites/)）

### 启动开发模式
```bash
npm install
npm run tauri dev
```

### 构建生产包
```bash
npm run tauri build
```

---

## 📄 License

MIT License — 开源、自由、永远免费。
