# DashDrop ⚡

> **极速、加密、无需配置的跨平台近场文件传输工具**
> Blazing-fast, encrypted, zero-config local file sharing — for macOS, Windows, and Linux.

Canonical detailed project status: [STATUS.md](./STATUS.md)
Seamless experience target design (AirDrop-like): [docs/AIRDROP_SEAMLESS_EXPERIENCE_DESIGN.md](./docs/AIRDROP_SEAMLESS_EXPERIENCE_DESIGN.md)
Daemon refactor plan: [docs/DAEMON_REFACTOR_PLAN.md](./docs/DAEMON_REFACTOR_PLAN.md)
Network troubleshooting: [docs/NETWORK_TROUBLESHOOTING.md](./docs/NETWORK_TROUBLESHOOTING.md)
Architecture note: current releases still keep the current product baseline in a Tauri shell, but packaged builds now bundle `dashdropd` as a sidecar and prefer a daemon-backed control plane by default; dev sessions still default to `in_process` unless explicitly forced.

---

## 📌 当前状态摘要（截至 2026-03-12）

- 构建与测试：
  - `cargo check` 已接入 CI；当前工作区于 2026-03-12 本地复核通过
  - `cargo clippy --all-targets --all-features -- -D warnings` 已接入 CI
  - `cargo test` 已接入 CI；当前工作区于 2026-03-12 本地复核通过
  - `npm run build` 已接入 CI；当前工作区于 2026-03-12 本地复核通过
  - `npm run test:e2e`（Playwright UI E2E）当前工作区于 2026-03-12 本地复核通过
  - `npm run test:e2e:contract`（契约脚本级 E2E）已接入 CI
  - `npm run test:tauri:smoke` 真实运行时启动烟测已于 2026-03-12 本地复核通过（覆盖 `tauri dev` 拉起链路，不再在启动阶段因 local IPC listener 绑定而 panic）
  - `npm run test:tauri:daemon-smoke` 已于 2026-03-12 本地复核通过，覆盖真实 `dashdropd + tauri dev` daemon-backed 启动链路
  - `npm run test:tauri:bundle-smoke` 已于 2026-03-12 本地复核通过，用于验证平台 bundle 内实际包含 `dashdropd` sidecar
  - `tauri dev` smoke 现已改为动态分配 Vite/HMR 端口，不再因为本机 `1420/1421` 被占而误报失败
  - 已新增安全扫描：`security-audit.yml`（cargo audit + npm audit）+ GitHub Code Scanning default setup
  - 已新增 `dependabot.yml`（Actions/npm/cargo 依赖周更）
  - 已新增安装包发布流水线增强：标准化产物命名、烟测、可选签名/公证钩子、发布校验和
  - 已新增发布与升级模板文档：`docs/RELEASE_NOTES_TEMPLATE.md`、`docs/UPGRADE_MIGRATION_TEMPLATE.md`
- 共享入口 contract（已冻结，供多分支并行开发使用）：
  - runtime shell event 名称保持为：`external_share_received`、`pairing_link_received`、`app_navigation_requested`、`app_window_revealed`
  - `app/activate` / second-instance handoff 的共享 payload 仍只转交两类数据：`paths[]` 与 `pairing_links[]`
  - 外部分享路径的壳层语义保持为“排队到 Nearby 等待用户选设备”，不会在接收 handoff 时自动发送
  - tray/shell attention 命令 payload 仍保持四字段：`pendingIncomingCount`、`activeTransferCount`、`recentFailureCount`、`notificationsDegraded`
  - 以上 contract 已实现并在当前工作区冻结；Windows/Linux 的系统级分享入口与真实 OS 投递行为仍需实机验收，不应视为 fully verified
- 状态契约：
  - 已收口进行中/终态事件契约，已包含 `transfer_accepted`
  - `revision` 仅在状态跃迁递增，`transfer_progress` 不再推动 revision 递增
- 架构边界：
  - 已引入稳定 DTO（`DeviceView/SessionView/TransferView`），前端不再依赖 `SocketAddr/Instant`
  - AirDrop-like 目标文档已补齐关键约束：固定端口+防火墙策略、通知过期闭环、恢复前源快照校验、功耗与隐私广播策略（目标态，未默认上线）
- 功能补齐：
  - 已实现 `connect_by_address`（返回远端指纹摘要并进入确认/可选配对流程）
  - Nearby 对未信任设备发送前增加指纹确认弹窗（可选立即配对）
  - 已实现 trusted-only auto-accept（`auto_accept_trusted_only`）
  - 已实现 UDP beacon 发现兜底（当 mDNS 受限时仍可在同网段互见）
  - 已实现 Reachable Probe（ALPN `dashdrop-probe/1`）与 `Offline` 状态收敛（15s 宽限）
  - History 终态自动刷新、incoming 大小格式化、本地筛选已完成
  - History 已支持时间窗口筛选（24h/7d/30d）
  - Transfers 已支持批量取消活动任务与发送任务重试
  - PartialCompleted 场景已支持“失败文件级重试”（不是整任务重发）
  - Trusted Devices 已支持配对时间、最近使用时间与别名编辑
  - Settings 已支持冲突策略（重命名/覆盖/跳过）与并发流配置
  - 前端状态读取已补齐契约兜底，Transfers/History 对空载荷更稳健
  - Transfers / History / Security Events 已补关键失败路径的用户可见错误反馈
  - 后端配置入口已拒绝空设备名，trust/config 写路径已统一委托到核心服务层
  - 本地 IPC Unix socket 启动链路已改为先用 `std` 绑定再进入 Tokio 监听，Tauri `setup` 阶段不再要求预先存在 reactor
  - Windows 本地 IPC 已补 named pipe server/client 基线实现，和 Unix socket 共用同一套 framed wire 协议
  - 主壳层已支持窄窗口导航重排，避免固定侧栏压缩内容区
  - Settings、Nearby、Transfers 与接收确认弹窗已补短验证码；未信任发送/接收/按地址确认现在要求显式比对双方一致的 shared verification code
  - Settings 现可导出本机配对二维码与 `dashdrop://pair?...` pairing URI，也可通过粘贴、导入二维码图片或直接启用摄像头扫码来完成 pairing link 导入与 trust + alias 落库；新生成的 pairing link 现已由本机长期身份签名，导入时会先校验 freshness/验证码/签名元数据，再允许落 trust；导入界面还会给出基于双方指纹的 shared pair code，便于双方做双向带外核对；扫码在原生 `BarcodeDetector` 不可用时回退到 `jsQR`，pairing link 为短时凭据，默认约 10 分钟过期
  - Pairing 导入入口不再局限于 Settings；Nearby 与 Trusted Devices 现在也能直接导入或扫码配对，减少为配对来回切页
  - Nearby 里刚完成配对的设备现在会短时前置并高亮，方便配对后立刻发文件
  - 摄像头扫码现在带有取景框、识别高亮和成功过渡反馈，不再只是“扫到了就突然填内容”
  - 启动参数 / `app/activate` 现在也会转交 `dashdrop://pair?...` pairing link；另外，运行中的应用也会处理操作系统投递的 URL open 事件，收到 deep link 时前端会自动切到 Settings 并弹出导入确认，而不是只能手动粘贴
  - Tauri bundle 现已通过 `tauri-plugin-deep-link` 正式登记 `dashdrop://` desktop scheme；Linux 与 Windows 开发态也会在启动时补注册，减少“打包前 scheme 不可用”的落差
  - 系统通知不可用时，incoming request 现在会继续留在 Transfers 队列里，并把 pending 数同步到托盘 tooltip/title，同时前台给出明确降级提示，不再变成“静默等到超时”
  - 托盘 attention 现会同时聚合 pending incoming、活动传输和最近失败事项，后台运行时也能看出当前是“有请求待处理”还是“有任务在跑/有问题待看”
  - 托盘菜单顶部现会动态显示这些摘要状态，不打开主窗口也能先确认后台当前到底是在等待接收、正在传、还是刚有失败事项
  - 当后台确实有待处理事项时，托盘菜单还会直接给出一个动态 “Review …” 入口，点一下就进 Transfers，不需要先判断该去哪一页
  - 启动参数与本地 IPC 现在都能把外部分享路径排队到 Nearby，直接选设备发送，作为 system share / daemon 方向的基础能力
  - 第二实例启动时会优先通过本地 IPC 把激活/分享路径交给已运行实例，作为 single-instance handoff 基础
  - daemon 重构线已进入 UI 壳层 + daemon-client 收口阶段：`docs/DAEMON_REFACTOR_PLAN.md` 已同步到当前实现，运行时初始化、宿主接口和 runtime supervisor 已抽到 `src-tauri/src/runtime/`
  - 已新增 `dashdropd` headless daemon binary；当前已能启动 local IPC、后台维护循环以及 discovery/transport network runtime，daemon 模式下 discovery/transfer/trust/config 已由 daemon 持有
  - UI daemon-client 第二步已落地：设置 `DASHDROP_CONTROL_PLANE_MODE=daemon` 后，`get_devices`、`get_trusted_peers`、`get_pending_incoming_requests`、`get_app_config`、`get_local_identity`、`get_runtime_status`、`get_transfers`、`get_transfer`、`get_transfer_history`、`get_security_events` 会优先通过 local IPC 读取 daemon 状态
  - UI daemon-client 第三步已推进：daemon 侧运行时事件现已写入 `runtime_event_feed`，前端 store 与 History 在 daemon 模式下会通过 local IPC 轮询消费这些事件；UI 壳层自身的 reveal/share/system notice 事件则继续走本地 shell 监听
  - Settings 的 discovery diagnostics / transfer metrics 现在也能在 daemon 模式下通过 local IPC 读取；Security Events 页面在 daemon 模式下会跟随安全类事件自动刷新
  - UI daemon-client 第四步已开始：`send_files_cmd`、`connect_by_address`、`accept/reject`、`cancel/retry`、`pair/unpair`、`set_trusted_alias`、`set_app_config` 在 daemon 模式下已优先通过 local IPC 写入 daemon，而不是直接操作 UI 进程内状态
  - `trusted_peer_updated` / `app_config_updated` 已加入 runtime event feed，Trusted Devices 与 Settings 在 daemon 模式和普通模式下都会随 trust/config 变更自动刷新
  - daemon service endpoint 现在对读/写/事件回放命令强制要求短时 `access_token`；UI 侧会在内存中缓存 grant、按 `refresh_after_unix_ms` 预刷新，并在 `unauthorized` 后自动重握手一次
  - token 刷新现在会携带旧 grant 并由 daemon 吊销旧 token，避免旧 token 一直存活到 TTL 结束；grant 自身仍不落盘
  - daemon service endpoint 现已支持显式 `auth/revoke`；UI 在真正退出时会 best-effort 撤销当前缓存 token，进一步缩短 grant 存活窗口
  - Unix service endpoint 现会额外校验 peer uid；Windows named pipe 现在既会在创建期挂 owner-only DACL，也会在连接后校验 client SID 是否属于当前用户
  - `app/get_event_feed` 现在返回 `generation + oldest/latest seq + resync_required` 快照；当前端检测到 ring buffer 截断或 daemon 重启时，会主动回拉权威快照而不是继续盲吃增量事件
  - runtime event feed 现在采用“1024 条内存热窗口 + 10,000 条 SQLite 持久基线窗口”，持久 journal 还引入了正式的 segment metadata / compaction watermark：当最近仍活跃的 consumer checkpoint 需要更老历史时，daemon 会按 segment 保留更老事件，最多扩到 100,000 条，并记录当前 compacted segment 数量、watermark 与最后压缩时间；daemon 侧还会持久化共享 UI poller 的 replay checkpoint，UI 重启后会优先从 daemon checkpoint 恢复，并保留本地 cursor 作为兜底，但它仍不是无限持久日志或 push subscription
  - daemon 现在还会对 replay checkpoint 做生命周期管理：共享 UI poller 即使没有新事件也会定期 heartbeat 续约；过期 checkpoint 会自动清理，diagnostics 会暴露 feed 请求数、persisted catch-up 次数、resync 次数、heartbeat 次数，以及每个 consumer 的 lag / age / recovery state，方便判断谁已经落后到必须 resync
  - daemon 模式下关闭主窗口现在会改为隐藏窗口而不是退出进程；macOS `Reopen`、second-instance handoff、external share intake 都由 UI 壳层事件负责前台恢复/入队，但不再触发本地 runtime 启动
  - 本地 IPC 现在已拆成 `service` 与 `ui activation` 双端点，避免 UI shell 和 `dashdropd` 争用同一个 socket / named pipe
  - UI 启动链路已补 `auto/daemon/in_process` 控制平面模式；`auto` 会优先接入已运行的 daemon，并在可用时尝试自动拉起 `dashdropd`
  - daemon 模式下，UI 不再启动本地 network runtime supervisor，只保留 UI activation server 和前端壳层；runtime 读/写/事件以 daemon 控制面为准，UI 只保留窗口激活/分享入队等壳层职责
  - `npm run tauri build` 现在会自动准备 `dashdropd` sidecar，并通过 Tauri `externalBin` 将它一起打进平台 bundle
  - Runtime Status / Discovery Diagnostics 现在会直接暴露 `control_plane_mode`、`daemon_status` 与 `daemon_binary_path`，便于确认当前是否真的处于 daemon-backed 运行态
  - 若启动时检测到 `dashdropd` sidecar 存在但未能接入 daemon 控制面，前端现在会直接显示系统级告警，而不是只在 Settings 被动查看状态
  - 指标已持久化聚合（平均耗时/失败分布/终态计数/收发字节）
  - 持久化已收口为 SQLite（`state.json` 仅用于一次性迁移读取）
- 安全闭环：
  - 已落地 `security_events` 审计存储与查询命令
  - `fingerprint_changed` / `identity_mismatch` 均有 UI 告警消费路径
  - Linux/Windows keyring 不可用时支持降级并在设置页显示高风险提示
  - 限流策略已同时覆盖 IP 级与 fingerprint 级连接/Offer窗口
  - 启动配置目录解析失败时改为显式报错，不再静默回退到当前目录
- 用户体验：
  - 已提供首启 Onboarding（本地持久化关闭）
- 仍在进行：
  - 双端 QUIC 多机编排压测扩展（当前已有跨模块合同测试）
  - 原生窗口级 Tauri runtime E2E 编排（当前已补 local IPC 启动回归测试与 `tauri dev` 启动烟测，UI 自动化仍以 mock IPC 为主）
  - 更强的首次信任方案（二维码/短码交换流程）；当前已提供双方一致的 shared verification code，但仍属于 TOFU + 人工带外核对

---

## 🎯 Vision

DashDrop 的目标是把 AirDrop 的核心体验带到每一台桌面电脑上，无论它运行的是 macOS、Windows 还是 Linux。

不需要扫码、不需要账号、不需要 USB 线、不需要打开浏览器。

只要打开 DashDrop，**拖拽，松手，完成。**

---

## ✨ 核心体验指标

| 指标 | 目标值 |
|------|--------|
| 发现延迟 | 双端服务在线后，< 2 秒看到附近设备 |
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
- [x] UDP beacon 发现兜底（同网段广播）
- [x] QUIC + TLS 1.3 加密传输通道
- [x] Ed25519 设备长期密钥对（系统凭据存储）
- [x] 单/多文件发送（顺序传输 + BLAKE3 整文件校验）
- [x] 接收请求队列卡片（Transfers 页处理，Accept / Decline）
- [x] 主界面设备卡片列表（实时发现、上下线感知）
- [x] 拖拽发送（单通道事件模型）
- [x] 传输进度条与完成通知

> **MVP 不包含**：断点续传（v0.2）、带外身份验证

### v0.2 — 可靠性与安全 (Current RC)
- [x] 断点续传（块级 BLAKE3 + SQLite 持久化）
- [x] 带外验证（二维码配对、签名链接、双向验证码核对）
- [x] 信任设备体系增强（配对迁移、别名、最近使用时间、自动接收）

### v1 — 体验精打磨
- [ ] 图片/视频缩略图预览
- [x] 传输历史记录
- [~] 右键菜单集成（Windows 已实现基线，macOS/Linux 待补）
- [ ] macOS Finder Quick Action
- [x] 开机自启基线（macOS LaunchAgent / Windows Run key / Linux XDG autostart）
- [x] 系统托盘（点击恢复主窗口、托盘菜单导航、托盘退出）

### v2 — 进阶
- [~] BLE 近场辅助发现 (Mac/Win 已实现原生桥接，Linux 待补)
- [~] 临时热点直连 fallback (SoftAP 调度器框架已就绪)
- [ ] 剪贴板同步
- [x] daemon + local IPC 控制平面
- [ ] 系统分享入口与完整常驻化体验

---

## 🏗️ 系统架构

```
DashDrop（当前支持 in_process 与 daemon-backed 两种运行形态）
├── Tauri UI Shell
│   ├── 前端 UI（WebView 线程）
│   ├── UI activation 本地 IPC 端点
│   └── daemon client / shell-local 事件桥
│
├── dashdropd（daemon 模式下持有 runtime）
│   ├── Discovery（mDNS + UDP beacon）
│   ├── Transport Core（QUIC / BLAKE3）
│   ├── Trust / Config / History / Security state
│   └── service 本地 IPC 端点
│
└── in_process 回退路径（开发态默认）
    ├── Discovery（Rust tokio 异步任务）
    ├── Transport Core（Rust tokio 异步任务）
    └── Identity & Crypto（Rust）
```

**进程模型说明**：开发态默认仍可运行单进程 `in_process` 基线，便于本地调试；打包态默认优先接入/拉起 `dashdropd`，由 daemon 持有 discovery/transfer/trust/config runtime，Tauri 进程主要作为 UI 壳层与客户端。daemon 模式下关闭 UI 不会终止 daemon 持有的后台 runtime。详见 [ARCHITECTURE.md](./ARCHITECTURE.md)。

---

## 🛠️ 技术栈

| 层 | 技术 | 理由 |
|----|------|------|
| 应用框架 | **Tauri 2.0** | 极小体积（< 10 MB），调用原生系统 API |
| 前端 | **Vue 3 + TypeScript** | 生态成熟，开发高效 |
| 样式 | **原生 CSS + Design Tokens** | 低依赖、易控、跨平台 UI 一致性 |
| 动画 | **CSS @keyframes** | 无额外依赖，与 Vue 原生集成 |
| 图标 | **Lucide Vue Next** | 精简现代图标库 |
| 状态管理 | **Vue Composition API（`src/store.ts`）** | 轻量、无额外状态库依赖 |
| 后端语言 | **Rust** | 极低内存、极高性能、跨平台编译 |
| 传输协议 | **QUIC (quinn crate)** | 低延迟、多路复用、内置 TLS |
| 服务发现 | **mDNS + UDP beacon fallback** | 组播优先，广播兜底，提升受限网络可发现性 |
| 序列化 | **CBOR (ciborium crate)** | 紧凑二进制，serde 生态兼容 |
| 加密 | **Ed25519 (ed25519-dalek) + rcgen** | 密钥对 + 自签证书 |
| 文件完整性 | **BLAKE3** | 高速哈希，整文件传输后校验 |
| 运行时 | **Tokio** | Rust 异步生态事实标准 |

---

## 🔒 安全设计

DashDrop MVP 采用 **"局域网物理接近信任"** 模型（LAN Proximity Trust）：

- **端到端加密**：所有传输走 QUIC + TLS 1.3，局域网截包无法获得明文
- **设备身份绑定**：发送侧对“选中设备 fp”与 TLS 证书 fp 做强绑定校验；接收侧对 mDNS/TLS 不一致发出安全告警与审计记录
- **诚实的首次配对提示**：UI 明确告知用户"首次连接无法自动验证身份，请确认对方在你身边"
- **已配对设备告警**：重连时 fingerprint 变化会触发安全告警
- **文件落地隔离**：接收文件只写入 `Downloads/DashDrop`

> **注意**：首次配对建议在受信任网络下进行，或使用 **二维码/签名链接** 进行带外验证（OOB），以彻底防御局域网 MITM 攻击。详见 [PROTOCOL.md](./PROTOCOL.md)。

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
invoke('get_discovery_diagnostics')                // 复制发现链路诊断 JSON
invoke('get_transfer_metrics')                     // 聚合传输指标

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
│   │   ├── TransferModal.vue    # 发送确认与指纹确认
│   │   ├── ConfirmModal.vue     # 通用确认弹窗
│   │   └── SystemNotice.vue     # 系统级告警条
│   ├── store.ts                 # 全局状态（Composition API）
│   ├── views/                   # Nearby/Transfers/History/Trusted/Security/Settings
│   ├── App.vue
│   └── main.ts
│
├── src-tauri/src/          # Rust 后端
│   ├── main.rs
│   ├── lib.rs
│   ├── crypto/             # 身份、证书、验证器与安全存储
│   ├── state.rs            # AppState（多会话设备建模）
│   ├── discovery/          # mDNS + beacon 发现链路
│   └── transport/
│       ├── mod.rs          # 协议消息类型与传输子模块
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
# 真实运行时启动烟测
npm run test:tauri:smoke
# 真实 daemon-backed 启动烟测
npm run test:tauri:daemon-smoke
# 打包态 sidecar 烟测
npm run test:tauri:bundle-smoke
```

如需在开发态手动验证 daemon 接入路径，可先执行：
```bash
npm run tauri:prepare-sidecar
```

这会为当前主机平台生成 `src-tauri/binaries/dashdropd-<target>`。开发态运行时现在会自动搜索这些 repo 内路径：
- `src-tauri/binaries`
- `src-tauri/target/debug`
- `src-tauri/target/release`

也就是说，显式测试 `DASHDROP_CONTROL_PLANE_MODE=daemon` 时，不再只认打包 sidecar；如果你已经本地构建过 `dashdropd`，运行时也会自动找到它。若这些路径里仍没有 daemon binary，开发态启动会再尝试一次本地 `cargo build --bin dashdropd`，成功后继续 attach。

### 构建生产包
```bash
npm run tauri build
```

## 安装排障（未签名构建）

如果你安装的是 CI 生成的未签名包（尤其是 macOS），系统可能拦截启动。

- macOS 出现 `"DashDrop" is damaged and can’t be opened`：
  ```bash
  xattr -dr com.apple.quarantine /Applications/DashDrop.app
  codesign --force --deep --sign - /Applications/DashDrop.app
  open /Applications/DashDrop.app
  ```
- Windows 双击后立即退出：
  请查看启动日志：
  - `%APPDATA%\\DashDrop\\startup-error.log`
  - `%TEMP%\\dashdrop-startup-error.log`
  常见原因：
  - WebView2 Runtime 未就绪
  - Windows 防火墙尚未放行 `dashdropd.exe`
  - 旧版 Windows 缺少运行时组件（本仓库现已改为 Windows MSVC CRT 静态链接以降低该风险）

---

## 📶 Network & Discovery Diagnostics

DashDrop uses a combination of **mDNS (Multicast DNS)**, **UDP Beacons**, and **Bluetooth LE** to find nearby devices. If discovery fails:

### 1. Common LAN Blockers
- **AP Isolation**: Public or guest Wi-Fi often disables device-to-device communication. Use a private network or **Mobile Hotspot**.
- **VLAN/Subnet Segregation**: Devices must be on the same subnet for mDNS to work.
- **Firewalls**: 
  - Ensure **UDP 5353** (mDNS) is allowed.
  - Ensure **UDP 53318** (beacon fallback) is allowed.
  - Ensure **UDP 53319** (QUIC) is allowed.
  - If `Settings -> Runtime` shows a fallback random listener port, allow that UDP port too.

### 2. VPN Interference
- Many VPNs disable local network access. Try enabling **Split Tunneling** or turning off the VPN during transfer.

### 3. Bluetooth Support
- **macOS/Windows**: Fully supported via native helper processes.
- **Linux**: Currently experimental; relies primarily on network beacons.

---

## 🛠️ Known Limitations

- **Large File Performance**: Transfers over 50GB may experience slowdowns on slower Wi-Fi networks (recommend 5GHz/6GHz).
- **Daemon Idle Exit**: The background daemon will automatically exit after 2 hours of inactivity to save power (configurable in Settings).
