# DashDrop — 详细架构设计文档

本文档描述 DashDrop 的工程架构、模块设计与关键设计决策。

> **文档版本**：v0.4（2026-03）
> **主要修订**：五区 IA 与非阻塞接收模型、TransferStatus 与终态事件映射统一、接收方 Cancel 语义对齐、TransferHistoryEntry 与持久化约束、TrustedPeer/AppConfig 结构补全、Reachable Probe 归属与线协议约束

> **实现状态快照（2026-03-10）**：
> - 已实现：DTO 边界（`DeviceView/SessionView/TransferView`）、`transfer_accepted`、终态事件统一映射、`revision` 仅状态跃迁递增。
> - 已实现：CI 门禁（`cargo check`、`cargo clippy --all-targets --all-features`、`cargo test`、`npm run build`、`npm run test:e2e`）。
> - 已实现：GitHub 安全与发布自动化（`security-audit`、Code Scanning default setup、`Dependabot`、installers + checksums 上传）。
> - 已实现：`connect_by_address`、trusted-only auto-accept、Probe(ALPN `dashdrop-probe/1`) 与 `Offline` 状态收敛、security_events 审计落地。
> - 已实现：真实浏览器自动化 Playwright E2E（mock IPC 驱动的 UI 流程）。
> - 已实现：sender accept 超时、60s 用户响应时限、目录 complete/ack 生命周期、fingerprint 级 offer 限流、Probe `0xD0` close code。
> - 已实现：配置驱动的文件冲突策略（overwrite/rename/skip）与并发流上限（`max_parallel_streams`）。
> - 已实现：前端任务管理增强（批量取消、发送任务重试）、配对别名与最近使用时间展示。
> - 已实现：partial 终态失败项按文件级重试（基于 `failed_file_ids` 与源路径映射）。
> - 已实现：配置/信任持久化收口 SQLite（legacy `state.json` 仅迁移读取）。
> - 未完成：真实双端 QUIC 多机编排压测（当前仍以合同测试+UI mock E2E 为主）。
> - 说明：本文中若出现与代码细节不一致的结构定义，以 `src-tauri/src/*.rs` 与 `src/*.ts` 的当前实现为准。

---

## 一、分层架构总览

```
+---------------------------------------------------------+
|                    UI Layer（前端）                      |
|         Vue 3 + TypeScript + CSS Design Tokens         |
|     设备卡片 · 拖拽投递 · 请求队列卡片 · 进度反馈        |
+-------------------------+-------------------------------+
                          | Tauri IPC (invoke / listen)
+-------------------------v-------------------------------+
|                 Backend Core（Rust）                    |
|  +-------------+  +--------------+  +---------------+  |
|  |  Discovery  |  |  Transport   |  |  Crypto       |  |
|  |(mDNS+beacon)| |  (QUIC/quinn)|  |  (Ed25519)    |  |
|  +-------------+  +--------------+  +---------------+  |
|                   +--------------+                      |
|                   |  AppState    |                      |
|                   | (Arc<RwLock>)|                      |
|                   +--------------+                      |
+---------------------------------------------------------+
        | UDP/5353 + UDP/53318  | QUIC/UDP (当前随机端口)
  +-----v------+          +-----v------+
  | LAN发现双通道|         | QUIC 传输  |
  | mDNS+beacon |         | TLS 1.3   |
  +------------+          +------------+
```

### 1.1 进程模型与可靠性边界

DashDrop 是一个**单进程 Tauri 应用**。后端逻辑运行在 Rust 异步线程（tokio runtime），前端运行在同一进程的 WebView 线程。

**实际保证**：
- UI 线程卡顿**不影响**后端 tokio 任务（异步隔离）
- 后端 panic 导致整个进程退出，正在进行的传输**随之终止**

**不保证**：
- "传输核心崩溃不影响 UI" — 无进程边界，无此保证
- 应用退出后后台暂停恢复 — MVP 不支持守护进程模式

如需进程级隔离，未来可将 transport/discovery 拆为系统服务，通过 Unix socket / named pipe 与 UI 通信。  
目标态 IPC 与权限模型见 [docs/AIRDROP_SEAMLESS_EXPERIENCE_DESIGN.md](./docs/AIRDROP_SEAMLESS_EXPERIENCE_DESIGN.md) §4.1。**当前 MVP 不实现 daemon 拆分**。

### 1.2 当前实现与目标态差异（文档对齐）

为避免“当前实现文档”和 AirDrop-like 目标文档混淆，以下差异明确保留：
1. 当前 QUIC 监听端口为运行时动态端口；目标态优先固定 `53319/udp`（占用时再回退随机端口）。
2. 当前通知链路不承担系统通知过期撤回与 `E_REQUEST_EXPIRED` 完整闭环；目标态要求强制闭环。
3. 当前 MVP 不实现断点续传；目标态要求恢复前执行 `source_snapshot(size/mtime/head_hash)` 一致性校验。
4. 当前为单进程 Tauri；目标态为 daemon + 本地 IPC（Unix socket / Named Pipe）。
5. 当前主路径仍以 1:1 传输为核心；目标态要求 batch 级 1:N 调度与单读多发扇出能力。

---

## 二、Discovery 模块（`src-tauri/src/discovery/`）

### 2.1 Discovery 通道规范（mDNS + beacon）

主发现服务名：`_dashdrop._udp.local`

**TXT Record 内容：**
```
id=<session UUID v4>         # 本次运行的会话 ID（每次启动随机）
name=<Host 显示名>            # 设备名（如 "Young's MacBook"）
port=<QUIC 监听端口>          # 先启动 transport 取得实际端口，再注册 mDNS
fp=<公钥指纹 Base64>          # Ed25519 公钥 SHA256，稳定设备身份
platform=mac|win|linux       # 平台类型
caps=file                    # MVP 能力集
```

> **无 `v=` 字段**：版本协商在 QUIC 连接后的 `Hello` 消息中进行，不在 mDNS 层。

UDP beacon 兜底通道：
1. 端口：`53318/udp`
2. 作用：在 mDNS 组播被网络策略限制时提供同网段广播发现
3. 状态：beacon session 与 mDNS session 统一汇聚进同一个 `DeviceInfo.sessions`

> **启动顺序约束**：transport server **必须先**绑定端口，获得实际端口号后，再注册 mDNS 服务并启动 browse；随后启动 beacon fallback。

### 2.2 设备身份双层模型

| 概念 | 字段 | 值来源 | 生命周期 |
|------|------|--------|----------|
| **会话 ID** | `id` (TXT) | 每次启动随机 UUID v4 | 本次运行 |
| **稳定设备身份** | `fp` (TXT) + TLS 证书 | Ed25519 公钥 SHA256 | 长期（跨重启）|

- 信任列表以 `fingerprint` 为主键
- 传输状态以 `peer_fingerprint` 标识 peer，而非 session_id

### 2.3 设备状态建模（多网卡 / 多会话）

**问题**：同一台设备可能在多个网卡接口上同时广播，产生多个 `session_id` 但相同 `fp`；`SessionRemoved(session_id)` 仅代表某个接口/会话的 mDNS 记录消失，不代表设备真正离线。

**解决方案**：`AppState.devices` 按 `fingerprint` 索引，每个 `DeviceInfo` 维护一个 `sessions: HashMap<String, SessionInfo>`（键为 session_id）：

```rust
pub struct DeviceInfo {
    pub fingerprint: String,              // 稳定设备主键
    pub name: String,
    pub platform: Platform,
    pub trusted: bool,                    // 由 trust 列表投影并回写到设备视图
    pub sessions: HashMap<String, SessionInfo>,  // session_id -> 会话信息
    pub reachability: ReachabilityStatus,         // discovered/reachable/offline_candidate/offline
    pub probe_fail_count: u32,                    // 连续探活失败次数
    pub last_probe_at: Option<u64>,               // 最近一次探活时间（unix）
}

pub struct SessionInfo {
    pub session_id: String,
    pub addrs: Vec<SocketAddr>,           // 同一 session 的地址集合（IPv4/IPv6）
    pub last_seen_unix: u64,
    pub last_seen_instant: Instant,
}

pub enum ReachabilityStatus {
    Discovered,
    Reachable,
    OfflineCandidate,
    Offline,
}

pub enum Platform {
    Mac,
    Windows,
    Linux,
    Android,
    Ios,
    Unknown,
}
```

**上线逻辑**：`ServiceResolved/BeaconReceived(session_id, fp, addrs, ...)` -> 若 `devices[fp]` 存在则插入/更新 `.sessions[session_id]`，否则新建 `DeviceInfo`（`reachability=Discovered`，`probe_fail_count=0`）。

**下线逻辑**：`ServiceRemoved(session_id)` -> 通过反向索引 `session_index[session_id]` 取得 `fp`，再从 `devices[fp].sessions` 移除该 session。仅当 `sessions` 变为空时才清理 `devices[fp]` 并 emit `device_lost`。若没有反向索引，实现方需全表扫描 `devices`，状态机会变脆弱。

**连接地址选取（启发式，非正确性规则）**：发起连接时聚合全部 session 地址并去重，按最近 session 优先，默认 IPv4 优先，IPv6 作为后备。

**探活写回规则**：
1. Probe 成功：`reachability_status=Reachable`，`probe_fail_count=0`。
2. Probe 失败：`probe_fail_count += 1`；达到阈值后置 `reachability_status=OfflineCandidate`。
3. 会话全丢失且超过宽限期：`reachability_status=Offline`。

**Trust 关联规则**：`trusted` 在 `DeviceInfo` 中作为展示字段维护，并在配对/取消配对时同步回写。

> 已知局限：mDNS 最近活跃的接口未必是最优路径 — 高延迟、单向可达、临时断开的接口同样可能刚刚广播过。MVP 阶段接受此启发式；v0.2 可引入主动探活（ICMP/TCP probe）辅助选路。

### 2.4 多网卡策略

过滤规则：
1. 排除 `flags` 中不含 `UP` 或不含 `MULTICAST` 的接口
2. 排除 Loopback（`lo0`, `lo` 等）
3. 排除已知虚拟/隧道接口（`utun*`、`awdl*`、`llw*`、`docker*`、`vmnet*`、`vEthernet*`、`tun/tap` 等）
4. 满足条件的接口**全部**注册广播

### 2.5 生命周期与事件推送

```
AppStart
  -> transport::server::start()   // 先绑端口，返回实际 port
  -> discovery::register(port)    // 再注册 mDNS
  -> discovery::browse()          // 持续发现
  -> discovery::beacon()          // UDP 广播兜底发现

SessionResolved(session_id, fp, name, addrs, ...)
  -> session_index.insert(session_id, fp)          // 维护反向索引 session_id -> fp
  -> devices[fp].sessions.insert(session_id, ...)
  -> Tauri Event: "device_discovered" | "device_updated"

BeaconReceived(instance_id, fp, addr, ...)
  -> session_id = "beacon:<instance_id>"
  -> 写入同一 devices/session_index
  -> Tauri Event: "device_discovered" | "device_updated"

SessionRemoved(session_id)
  -> fp = session_index.remove(session_id)         // O(1) 反向查找
  -> devices[fp].sessions.remove(session_id)
  -> IF devices[fp].sessions.is_empty():
      devices.remove(fp)
      Tauri Event: "device_lost"
```

### 2.6 Reachable Probe 归属与写回

Probe 采用“Discovery 调度 + Transport 执行”的分层：
1. Discovery 负责触发时机与节流（首次发现、进入 Nearby、发送前预检）。
2. Transport 负责执行轻量 QUIC preflight（不发送 Offer，不进入传输状态机）。
3. Probe 结果由 Discovery 写回 `DeviceInfo`（`discovered/reachable/offline_candidate`）并广播 `device_updated`。

约束：
1. Probe 不得触发 `transfer_incoming`，接收端不得显示传输请求 UI。
2. Probe 失败只影响可达性状态，不直接修改信任状态。

线协议行为（强制）：
1. Probe 连接使用独立 ALPN：`dashdrop-probe/1`（普通传输为 `dashdrop-transfer/1`）。
2. Probe 只建立 QUIC + TLS，并执行身份绑定校验；不发送 `Hello/Offer`。
3. 接收端识别到 `dashdrop-probe/1` 后立即回 `CONNECTION_CLOSE(code=0xD0)` 并释放资源，不进入“等待 Offer”流程。
4. Probe 连接不占用业务传输并发槽位，不计入 `PendingAccept` 超时计时。

---

## 三、Transport 模块（`src-tauri/src/transport/`）

### 3.1 协议消息类型

所有消息使用 **CBOR** 序列化（`ciborium`），走 QUIC 可靠流传输。

```rust
enum DashMessage {
    Hello(HelloPayload),          // 连接后首消息：版本协商
    Offer(OfferPayload),          // 发送端：我要传这些文件
    Accept(AcceptPayload),        // 接收端接受（含选定版本）
    Reject(RejectPayload),        // 接收端拒绝（含原因码）
    Chunk(ChunkPayload),          // 文件数据块
    Complete(CompletePayload),    // 某文件传输完成 + 整文件 BLAKE3 哈希
    Ack(AckPayload),              // 接收端确认落盘+校验结果（SUCCESS 则发送端 UI 成功）
    Cancel(CancelPayload),        // 发送/接收端取消
}

// ── Hello 消息：永不改变的固定外壳 ──────────────────────────────
// wire_version 是 Hello 消息格式本身的版本，当前固定为 0，永不改变。
// 任何实现都必须能解析 wire_version=0 的 Hello，否则版本协商无法启动。
// 若未来 Hello 结构需完全重构，通过 wire_version 区分解析路径，避免将
// 版本协商问题从 Offer 挪到 Hello 后再次重演。
struct HelloPayload {
    wire_version: u8,              // 固定 0，格式永不变
    supported_versions: Vec<u32>, // 业务协议版本列表（如 [1, 2]）
}

struct OfferPayload {
    transfer_id: Uuid,
    items: Vec<FileItem>,
    total_size: u64,
}

struct AcceptPayload {
    chosen_version: u32,
}

struct FileItem {
    file_id: u32,                  // 本次传输内唯一，从 0 递增
    name: String,
    rel_path: String,              // 安全约束见下方 §3.1a
    size: u64,
    file_type: FileType,           // 明确类型，不再仅用 is_dir（已废弃）
    modified: u64,
}

// MVP 只允许普通文件和目录，其余类型在 Offer 时拒绝发送、接收端验证后拒绝落地
enum FileType {
    RegularFile,
    Directory,
    // 以下类型 MVP 不传输；发送端不得将其放入 Offer，
    // 接收端遇到时对该 file_id 返回 Ack{ok:false, reason:E_UNSUPPORTED_FILE_TYPE}
    // Symlink, HardLink, DeviceFile, Socket, MacOsBundle — 留作 v0.2 扩展
}

// MVP: 一条 QUIC 连接只承载一个 transfer（见 §3.2 连接范围规则）
// 因此 Chunk/Complete/Ack 无需携带 transfer_id，连接上下文即为唯一标识
struct ChunkPayload {
    file_id: u32,
    chunk_id: u32,                 // 文件内序号（从 0 开始，每文件独立计数）
    offset: u64,
    data: Bytes,
}

struct CompletePayload {
    file_id: u32,
    file_hash: [u8; 32],          // 整文件 BLAKE3 哈希
}

struct AckPayload {
    file_id: u32,
    ok: bool,
    reason: Option<ErrorCode>,
}

// Cancel 语义见 §3.2a
struct CancelPayload {
    reason: CancelReason,
}

enum CancelReason {
    UserCancelled,
    Error(ErrorCode),
}
```

#### §3.1a — `rel_path` 安全约束（接收端强制执行）

接收端在处理每个 `FileItem.rel_path` 时，**必须**先通过以下校验，任一失败则对该文件回复 `Ack { ok: false, reason: E_INVALID_PATH }`（不影响其他文件继续传输）：

1. **拒绝绝对路径**：`rel_path` 不得以 `/`、`\`、`C:\` 等形式起始
2. **拒绝路径穿越**：`rel_path` 的任何路径段不得为 `..` 或包含 `..` 跳转
3. **拒绝空路径段**：`rel_path` 不得包含连续分隔符（如 `a//b`）或首尾分隔符
4. **拒绝保留名**：在 Windows 目标平台上，路径段不得为 `CON`、`PRN`、`AUX`、`NUL`、`COM[1-9]`、`LPT[1-9]`（大小写不敏感）
5. **规范化后重新校验**：将 `rel_path` 规范化（统一分隔符、折叠 `.` 段），再对规范化结果重复上述校验
6. **最终落盘路径计算**：`final_path = canonical(save_root / rel_path)`，要求 `final_path` 必须以 `save_root` 为前缀，否则拒绝（防止符号链接竞争等绕过场景）

`save_root` 固定为 `Downloads/DashDrop/<transfer_id>/`，每次传输独立子目录，避免跨传输的文件名覆盖冲突。

同一传输中，若两个 `file_id` 规范化后落盘路径相同，接收端拒绝后一个并回复 `Ack { ok: false, reason: E_PATH_CONFLICT }`。

新增错误码：
- `E_INVALID_PATH`：路径未通过安全校验
- `E_PATH_CONFLICT`：同一传输内落盘路径冲突
- `E_UNSUPPORTED_FILE_TYPE`：文件类型不在 MVP 允许范围内

#### §3.2a — Cancel 语义

`Cancel` 消息的作用范围：**取消整个 transfer**（不支持取消单个 file_id）。

**发送方发起 Cancel**：
- 可在 Offer 发出后、最后一个 Ack 收到前的任意时刻发送
- 接收端收到 Cancel 后，**停止接收新 Chunk**，关闭连接
- 已通过 `Ack { ok: true }` 确认落盘的文件：**保留**（不删除），对接收用户可见
- 已写入但尚未发 Ack 的文件：**删除**（未确认不保留，避免残留不完整文件）
- 接收端**不需要**发送回执（无 CancelAck 消息），连接直接关闭

**接收方发起 Cancel**（用户中途点击"拒绝"或"取消"）：
- 整个 transfer 终止
- 已通过 Ack 的文件：**默认保留**（符合用户直觉）
- 已写入但未 Ack 的临时文件：**删除**
- 发送端收到 Cancel 后 emit `transfer_cancelled_by_receiver { reason: E_CANCELLED_BY_RECEIVER, succeeded_count, failed_count }`

**MVP 不支持**：取消特定 file_id（单文件取消留作 v0.2）。

### 3.2 连接范围规则（MVP 强制）

**一条 QUIC 连接只承载一个 transfer（一次 Offer/Accept 交换）。**

`ChunkPayload` 和 `AckPayload` 不携带 `transfer_id`，消息的归属由连接上下文隐式提供。若允许同连接复用或并发多个 transfer，这两个消息会立刻产生歧义。

连接复用是 v0.2 的扩展点，届时需在 Chunk/Complete/Ack 中加入显式 `transfer_id`。

### 3.3 分块策略

- 块大小：**1 MiB**
- `(file_id, chunk_id)` 二元组唯一标识一个块，不同文件间 chunk_id 可重复
- 多文件并发：每文件一个 QUIC stream，`max_parallel_streams` 运行时可配置（默认 `4`，允许 `1..32`）
- 整文件 BLAKE3 哈希在 `Complete` 中携带，接收端 `Ack` 前完成校验

### 3.4 传输成功语义（含多文件部分成功）

**文件级**（逐 Ack 判定）：
```
Chunk* -> Complete -> 等待 Ack
  Ack { ok: true }  -> 该文件成功
  Ack { ok: false } -> 该文件失败
  Timeout           -> 该文件超时失败
```

**传输整体结果**（所有文件 Ack 收齐后汇总）：
```rust
enum TransferOutcome {
    Completed,                        // 所有文件均 Ack ok=true
    PartialCompleted(Vec<FailedFile>, Option<ErrorCode>), // 有成功且有失败/中断
    Rejected(ErrorCode),              // 对端显式拒绝
    CancelledBySender,
    CancelledByReceiver,
    Failed(ErrorCode),                // 零成功 + 异常
}
struct FailedFile { file_id: u32, name: String, reason: ErrorCode }
```

**IPC 事件映射**：
- `Completed` → emit `transfer_complete`
- `PartialCompleted` → emit `transfer_partial { succeeded_count, failed: Vec<FailedFile>, terminal_cause? }`
- `Rejected` → emit `transfer_rejected { reason }`
- `CancelledBySender` → emit `transfer_cancelled_by_sender { reason }`
- `CancelledByReceiver` → emit `transfer_cancelled_by_receiver { reason }`
- `Failed` → emit `transfer_failed { reason }`

**UI 规范**：`transfer_partial` 时显示"X 个文件成功，Y 个失败"并列出失败文件名。不得将 `PartialCompleted` 静默归并为 `Completed` 或 `Failed`。

**成功判定时机**：最后一个 `Ack` 收到后汇总，不以"最后一个 Complete 发出"为成功标志。

### 3.5 断点续传（MVP 不实现）

见 PROTOCOL.md §4.2。

### 3.6 QUIC 连接配置

```rust
max_idle_timeout:         30s
keep_alive_interval:      10s
initial_max_data:         100 MiB
initial_max_stream_data:  16 MiB
```

---

## 四、Crypto / Identity 模块（`src-tauri/src/crypto/`）

### 4.1 设备稳定身份

首次启动：
1. 生成 Ed25519 密钥对（长期，跨重启持久化）
2. 用 `rcgen` 签发自签 X.509 证书（有效期 10 年）
3. 私钥存入系统凭据：macOS Keychain / Windows DPAPI / Linux `keyring`
4. fingerprint = SHA256(DER 公钥)，Base64 编码

### 4.2 QUIC TLS 自定义验证

quinn 使用 rustls，默认不接受自签证书。需实现自定义 `ServerCertVerifier`：
- 接受任意自签证书（跳过 CA 链校验）
- 提取对端 cert fingerprint，供后续 fp 绑定校验使用

### 4.3 发现层与连接层身份绑定（当前实现）

TLS 握手完成后立即执行：

```
cert_fp = SHA256(peer_tls_cert.public_key_der)

[A] 发送端（已选择目标 fingerprint）:
    要求 cert_fp == selected_peer_fp
    不匹配 -> 关闭连接，emit "identity_mismatch"

[A2] 接收端（按来源 IP 匹配 discovery 设备）:
    若发现 mdns_fp != cert_fp -> emit "identity_mismatch" + security_events 审计
    注：接收端该路径当前用于告警与审计，不作为握手硬拒绝条件

[B] 查询信任列表:
    若 cert_fp in trusted_peers -> 已配对设备，正常继续
    若 cert_fp not in trusted_peers -> 新设备（未配对），走首次连接流程
```

> 说明：上面是当前实现。目标态安全收敛（v0.2+）会将“可确定预期身份”的接收侧 mismatch 升级为硬拒绝，详细策略见 AirDrop 目标设计文档 §7.1。

**`fingerprint_changed` 的正确触发条件**：

`fingerprint_changed` **不按设备名判断**（名字可变可伪造，同名不代表同一台机器）。触发条件唯一：

```
某个已配对的 fingerprint A 明确被同一个会话/连接上下文替换为 fingerprint B
```

当前实现中，`fingerprint_changed` 告警依赖“同一会话上下文”与历史配对关系：同一 session 上一次记录 fp 与本次 cert fp 不同，且旧 fp 已在 trusted 列表中。**不得**用"同名设备的不同 fp"作为 `fingerprint_changed` 的判断依据，以下情形均应视为"新未配对设备"而非"证书更换告警"：
- 设备名重复的不同机器
- 用户修改了设备名
- 攻击者故意使用相同名字

实现规则：
```
若 mDNS session_id 在 devices 表中有历史记录:
    若 devices[session_id].prev_fp != cert_fp:
        IF prev_fp in trusted_peers -> emit "fingerprint_changed"（已配对关系的证书演进）
        IF prev_fp not in trusted_peers -> 视为普通新设备，不告警
若无历史记录:
    新会话，查信任列表（见 [B]）
```

此规则确保：只有真正存在过"明确配对关系"的 fingerprint 发生演进时，才会产生安全告警。

### 4.4 手动 IP:port 连接的首次身份规则

手动连接（用户输入 `IP:port`）没有 mDNS fp 可比对，**绑定规则 [A] 不适用**。首次连接流程：

1. 建立 TLS 连接，提取 `cert_fp`
2. 查信任列表：
   - `cert_fp` 已在信任列表 → 已配对，正常继续
   - `cert_fp` 不在信任列表 → 首次连接弹窗，**向用户展示完整 fingerprint**，要求手动核验（如与对方设备屏幕上显示的 fp 比对）
3. 用户确认 → 加入信任列表，之后与 mDNS 发现的已配对设备等同处理

**不允许的实现**：手动连接时偷偷跳过 fp 展示或自动信任，这会使手动和 mDNS 连接变成两套不同安全模型。

### 4.5 安全模型（MVP：LAN Proximity Trust）

**提供的保证**：
- TLS 1.3 加密，被动嗅探无法读取
- 已配对设备重连时，真正的"配对关系中的 fp 演进"触发告警

**不提供的保证**：
- 首次配对（mDNS 或手动）无法防御主动 MITM

**UI 措辞规范**：
- 首次连接（mDNS 发现）：显示"首次连接，无法自动验证身份，请确认对方在你身边"
- 首次连接（手动 IP）：显示"首次连接，请核对设备指纹"
- 已配对：显示"已配对"，不使用"已验证"
- `fingerprint_changed`：显示"此设备证书已更换，可能存在安全风险"
- `identity_mismatch`：显示"连接身份与广播信息不符，请核验后重试"

---

## 五、AppState 模块（`src-tauri/src/state.rs`）

> 以下结构为“当前实现要点摘录”（非完整字段清单），以 `src-tauri/src/state.rs` 为准。

```rust
pub struct AppState {
    pub identity: Identity,
    pub devices: Arc<RwLock<HashMap<String, DeviceInfo>>>, // key: fingerprint
    pub session_index: Arc<RwLock<HashMap<String, SessionIndexEntry>>>,
    pub transfers: Arc<RwLock<HashMap<String, TransferTask>>>, // key: transfer_id
    pub trusted_peers: Arc<RwLock<HashMap<String, TrustedPeer>>>,
    pub config: Arc<RwLock<AppConfig>>,
    pub local_port: Arc<RwLock<u16>>,
    pub mdns_service_fullname: Arc<RwLock<Option<String>>>,
}

pub struct TransferTask {
    pub id: String,
    pub direction: TransferDirection,
    pub peer_fingerprint: String,
    pub peer_name: String,
    pub status: TransferStatus,
    pub revision: u64,
    pub terminal_reason_code: Option<String>,
}

pub struct TrustedPeer {
    pub fingerprint: String,
    pub name: String,
    pub paired_at: u64,
    pub alias: Option<String>,
    pub last_used_at: Option<u64>,
}

pub struct AppConfig {
    pub device_name: String,
    pub auto_accept_trusted_only: bool, // default: false
    pub download_dir: Option<String>,
    pub file_conflict_strategy: FileConflictStrategy,
    pub max_parallel_streams: u32, // default: 4, clamp: 1..32
}
```

补充说明：
- `Verifying` 不作为外部状态枚举，而是 `Transferring` 阶段的 `subphase`（如 `uploading | verifying`），用于 UI 细粒度展示。
- `Failed` 仅用于“零成功”终态；有成功也有失败必须归为 `PartialCompleted`。
- 持久化基线为 SQLite（`transfers_history` / `trusted_peers_store` / `app_config_store` / `security_events`）。

---

## 六、可靠性设计

### 6.1 连接状态机

```
Idle -> discover -> Connecting -> Hello -> Offer -> Transferring
  ^                                                      |
  +-- Recovering <-- network_change / timeout -----------+

Recovering: 指数退避重试（1s, 2s, 4s），最多 3 次
3 次失败 -> Idle，UI 显示"连接中断"，传输标记 Failed
```

### 6.2 QUIC 路径迁移（有条件支持）

QUIC 原生支持路径迁移（RFC 9000 §8），但实际效果受限于：
- 双方 quinn 版本
- 系统网卡切换时机与 QUIC 心跳竞争
- NAT/防火墙是否允许 UDP 源地址变更

**实际建议**：路径迁移是"可能生效的优化"，非"保证不中断"。迁移失败由状态机重建连接。

---

## 七、前端信息架构与组件设计（v1）

### 7.1 五区导航（与 DESIGN_V1 一致）

1. `Nearby`：设备发现与发起发送
2. `Transfers`：进行中任务 + Incoming Requests（唯一处理入口）
3. `History`：终态记录与重试入口
4. `Trusted Devices`：信任关系管理
5. `Settings`：全局配置

### 7.2 接收请求处理模式（非阻塞）

1. `transfer_incoming` 到达后写入 `IncomingQueue`
2. 在 `Transfers` 页展示 `IncomingRequestCard`
3. 非当前页时仅显示通知，点击跳转
4. 禁止“阻塞弹窗作为唯一入口”

### 7.3 DeviceCard 状态与视觉

设备状态至少区分：
1. `Discovered`：已发现未探活（可见但弱可用）
2. `Reachable`：探活通过（主可交互态）
3. `OfflineCandidate`：连续探活失败（疑似离线）
4. `Risk`：身份冲突/可疑变更（高优先级警示样式）
5. `Offline`：离线

信任标签由 Trust Join 得出：`Trusted | Untrusted | TrustSuspended`。

### 7.4 手动 IP 连接入口

手动连接入口放在 `Transfers` 页 `Connect by Address`：
1. 输入 `IP:port`
2. 建连并展示完整 fingerprint
3. 用户确认后继续
4. 可选“记住此设备”写入 Trusted Devices（`source=manual`）

---

## 八、平台差异适配清单

| 功能 | macOS | Windows | Linux |
|------|-------|---------|-------|
| 开机自启 | LaunchAgent | 注册表 Run key | XDG autostart |
| 系统托盘 | NSStatusItem | Shell_NotifyIcon | AppIndicator |
| 系统通知 | UserNotifications | WinRT Toast | libnotify |
| 权限存储 | Keychain | DPAPI | secret-service |
| VPN 接口识别 | `utun*` | `wintun`, `tap-*` | `tun*`, `tap*` |

> 开机自启、系统托盘、系统通知：**P2，不在 MVP 范围**。
