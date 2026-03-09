# DashDrop — 协议规范 v0.3

本文件为 DashDrop 的网络协议与状态机规范。任何对协议的修改需要更新此文档并升级协议版本号。

> **v0.3 修订摘要**（在 v0.2 基础上）：
> - **传输成功语义闭环**：接收端落盘+校验成功后发 `Ack`，发送端以此为唯一成功判据
> - **版本协商前移**：QUIC 连接后先交换 `Hello` 消息，再发 `Offer`，避免跨版本 Offer 结构无法安全解析
> - **发现层与连接层身份绑定**：TLS 握手后强制校验 `cert_fp == mDNS fp`，两套身份不允许分歧
> - **速率限制改为基于 fingerprint**，IP 在多网卡/IPv6 场景下不可靠

> **实现状态快照（2026-03-09）**：
> - 已落地：`transfer_started / transfer_incoming / transfer_accepted / transfer_progress` 进行中事件。
> - 已落地：`transfer_complete / transfer_partial / transfer_rejected / transfer_cancelled_by_sender / transfer_cancelled_by_receiver / transfer_failed` 终态事件。
> - 已落地：`transfer_progress` 不递增 revision，revision 仅在状态跃迁递增。
> - 已落地：Probe ALPN `dashdrop-probe/1` 接线（Discovery 调度、Server 识别后快速关闭）。
> - 已落地：sender `Accept/Reject` 超时控制、`USER_RESPONSE_TIMEOUT_SECS=60`、目录 `Complete/Ack` 生命周期、`reason_code` 协议编码、fingerprint 限流、probe close `0xD0`。
> - 已落地：接收端冲突策略执行（覆盖/重命名/跳过）与并发流上限配置接线（运行时可配）。
> - 已落地：partial 结果失败项可被发送端按文件级重试（无需整任务重发）。
> - 已落地：工程门禁与发布自动化（CI + clippy、security audit、CodeQL、installers/release checksum）；协议行为不变，仅增强交付质量。
> - 部分待补：协议文档中的“真实端到端集成测试要求”尚未完全达成（当前为单测+契约测试增强）。

---

## 1. 协议版本

当前实现版本：**v0.1（实现中已吸收 v0.3 的关键状态契约约束）**

**版本协商在 QUIC 连接后的 `Hello` 消息中进行，早于 `Offer`**。

原因：`Offer` 消息本身已按发送端版本的结构编码发出，若版本在 `Offer` 层才对比，跨版本结构差异已无法安全解析。版本协商必须先于任何业务消息。

**Hello 消息的固定外壳（bootstrap）**：

`Hello` 本身也需要一个"永远可解析的固定外壳"，否则将来若 Hello 格式变了，老版本连协商都开始不了 — 这只是把问题从 Offer 挪到了 Hello。解决方式：`HelloPayload` 在 CBOR 序列化前，新增一个固定的第一字段 `wire_version: u8`，规定：

- **`wire_version` 当前固定为 `0`，永不改变**
- 任何实现**必须**能解析 `wire_version=0` 的 Hello，否则拒绝连接
- 若未来 Hello 格式需彻底重构，通过 `wire_version` 区分解析路径（如 `wire_version=1` 走新逻辑），不破坏旧客户端对 `wire_version=0` 的解析能力

**业务版本协商流程**（见 §3）：
1. 连接建立后，双方各发 `Hello { wire_version: 0, supported_versions: Vec<u32> }`
2. Responder 选定 `chosen_version = 双方 supported_versions 交集中最高版本`，写入 `Accept`
3. 若交集为空 → `Reject { reason: E_VERSION_MISMATCH }` 后关闭连接
4. 双方此后按 `chosen_version` 编解码后续消息

---

## 2. 传输层协议栈

```
[ 文件数据 / 控制消息 ]
[ CBOR 序列化 (ciborium) ]
[ QUIC 流 (Stream) ]
[ TLS 1.3 ]
[ UDP ]
[ IP (IPv4 / IPv6) ]
```

- **QUIC 实现**：`quinn` crate（基于 tokio）
- **TLS 证书**：设备长期 Ed25519 密钥签发的自签 X.509 证书（`rcgen`）
- **序列化**：CBOR（`ciborium` crate）

---

## 3. 连接建立流程

```
Sender                                  Receiver
  |                                         |
  |---- QUIC Connect (TLS 握手) ----------->|
  |<--- TLS Cert Exchange ------------------|
  |                                         |
  |  ★ 双方执行身份绑定校验（强制）：          |
  |    cert_fp = SHA256(peer_tls_cert.pubkey)|
  |    IF 来自 mDNS: ASSERT cert_fp == mDNS fp |
  |    ELSE: 关闭连接，UI 告警              |
  |                                         |
  |---- Hello { supported_versions } ------>|
  |<--- Hello { supported_versions } -------|
  |     (双方取交集最高版本 = chosen_version) |
  |                                         |
  |---- Offer { transfer_id,              |
  |             items, total_size } ------->|
  |                                         |
  |<--- Accept { chosen_version } ----------|  (用户接受，确认协议版本)
  |  OR Reject { reason } -----------------|  (拒绝 / 版本无交集)
  |                                         |
  |=== 传输阶段（见第4节）=================|
  |                                         |
  |---- Complete { file_id, hash } -------->|
  |                                         |  (落盘 -> BLAKE3 校验)
  |<--- Ack { file_id, ok: true } ----------|  (校验通过 -> 发送端 UI 成功)
  | OR  Ack { file_id, ok: false,           |
  |           reason: E_HASH_MISMATCH } ----|  (校验失败 -> 发送端 UI 失败)
  |                                         |
  QUIC Connection Close（在最后一个 Ack 收到之后）
```

**关键规则**：
- 发送端须等到所有文件 `Ack` 收到后再关闭连接。`Ack { ok: true }` 是唯一成功判据。
- **一条 QUIC 连接只承载一个 transfer**（MVP 强制规则）。`ChunkPayload` 和 `AckPayload` 不携带 `transfer_id`，消息归属由连接上下文隐式提供；连接复用留待 v0.2（届时需在这两个消息中加入 `transfer_id`）。
- 发送端**必须先收到接收端 `Hello` 并完成交集版本选择**后，才能发送 `Offer`；禁止在未收到对端 `Hello` 前发送 `Offer`。

### 3.1a Probe 连接识别（Reachable 探活）

为避免 Probe 与真实传输连接混淆，Probe 使用独立 ALPN：
1. 传输连接：`dashdrop/1`
2. Probe 连接：`dashdrop-probe/1`

Probe 行为：
1. 双方仅完成 QUIC + TLS 与身份绑定校验，不发送 `Hello/Offer`。
2. 接收端识别 `dashdrop-probe/1` 后立即返回 `CONNECTION_CLOSE(code=0xD0)` 并释放连接。
3. Probe 连接不得触发 `transfer_incoming` 或占用业务传输并发配额。

---

## 4. 数据传输流程（MVP）

### 4.1 单文件 / 多文件

多文件时每个文件占一个独立 QUIC stream，并发上限 4 个。

```
Sender                              Receiver
  |  [stream for file_id=0]             |
  |---- Chunk{file_id=0, chunk_id=0} -->|
  |---- Chunk{file_id=0, chunk_id=1} -->|
  |  ...                                |
  |---- Complete{file_id=0, hash} ----->|
  |                                     |  (落盘 -> BLAKE3 校验)
  |<--- Ack{file_id=0, ok:true} --------|
  |
  |  [stream for file_id=1, 并发]       |
  |---- Chunk{file_id=1, chunk_id=0} -->|
  |  ...
```

**`chunk_id` 命名空间**：每个 `file_id` 独立计数（从 0 开始）。`(file_id, chunk_id)` 二元组唯一标识一个块。

**文件级 Ack 语义**：
- `ok: true` — 落盘完成且 BLAKE3 校验通过
- `ok: false, reason` — 字节收完但验收失败（哈希不符 / 写盘错误）

**传输整体结果**（所有文件 Ack 收齐后汇总，非按文件分别汇报）：

| 结果 | 条件 | IPC 事件 |
|------|------|----------|
| 全部成功 | 所有文件 Ack ok=true | `transfer_complete` |
| 部分成功 | `succeeded_count > 0 && failed_count > 0`（含中途网络中断） | `transfer_partial { succeeded_count, failed_count, failed: [{file_id, name, reason}], terminal_cause? }` |
| 全部失败 / 连接级错误 | `succeeded_count = 0` 且传输终止 | `transfer_failed { reason, terminal_cause? }` |
| 对端拒绝 | 收到 `Reject` | `transfer_rejected { reason }` |
| 发送方取消 | 本端发送 `Cancel` | `transfer_cancelled_by_sender { reason }` |
| 接收方取消 | 对端发送 `Cancel` | `transfer_cancelled_by_receiver { reason }` |

**UI 规范**：
- `transfer_partial` 时显示"X 个文件成功，Y 个失败"并列出失败文件名与 `terminal_cause`（若有）
- 不得将 `PartialSuccess` 静默归并为成功或失败
- `transfer_failed` 仅用于“零成功”的传输

### 4.2 断点续传（MVP 不实现）

**原因**：
1. `ChunkPayload` 不含块级哈希，接收端无法判断已有块是否正确
2. 重连后进程状态已丢失，无持久化块清单
3. 正确实现需：持久化 `(transfer_id, file_id, chunk_id)` + 块级哈希 + 原子写入

**v0.2 目标**：补充块级哈希字段、持久化至本地 SQLite，再实现断点续传。

**MVP 行为**：传输中断后须重新发送整个文件。

---

## 5. 错误码

| 错误码 | 含义 |
|--------|------|
| `E_REJECTED` | 用户主动拒绝（兼容旧版本） |
| `E_REJECTED_BY_PEER` | 对端拒绝传输请求（推荐） |
| `E_DISK_FULL` | 接收端磁盘空间不足 |
| `E_HASH_MISMATCH` | 文件校验失败（数据损坏）|
| `E_VERSION_MISMATCH` | 协议版本无兼容交集 |
| `E_RATE_LIMITED` | 此设备发送请求过于频繁 |
| `E_TIMEOUT` | 连接超时 |
| `E_CANCELLED` | 取消（兼容旧版本） |
| `E_CANCELLED_BY_SENDER` | 发送端主动取消（推荐） |
| `E_CANCELLED_BY_RECEIVER` | 接收端主动取消（推荐） |
| `E_PERMISSION_DENIED` | 接收端无写入权限 |
| `E_IDENTITY_MISMATCH` | TLS cert fp 与 mDNS 广播 fp 不一致 |
| `E_INVALID_PATH` | `rel_path` 未通过安全校验（路径穿越/绝对路径/保留名等）|
| `E_PATH_CONFLICT` | 同一传输内多个文件规范化后落盘路径相同 |
| `E_UNSUPPORTED_FILE_TYPE` | 文件类型不在 MVP 允许范围（如符号链接、设备文件）|

错误码发射规则（强制）：
1. 发送新事件时，必须优先使用新码：`E_REJECTED_BY_PEER`、`E_CANCELLED_BY_SENDER`、`E_CANCELLED_BY_RECEIVER`。
2. 旧码 `E_REJECTED`、`E_CANCELLED` 仅用于**入站兼容解析**，不得作为新实现默认出站码。

### 5.1 rel_path 安全规则（接收端强制执行）

接收端处理每个 `FileItem.rel_path` 之前**必须**通过以下全部校验。任一失败 → 对该文件回复 `Ack { ok: false, reason: E_INVALID_PATH }`，不影响其他文件继续传输：

1. **拒绝绝对路径**：不得以 `/`、`\`、`C:\` 等形式起始
2. **拒绝路径穿越**：任何路径段不得为 `..`
3. **拒绝空路径段**：不得含连续分隔符（`a//b`）或首尾分隔符
4. **拒绝 Windows 保留名**（跨平台强制）：段名不得为 `CON`、`PRN`、`AUX`、`NUL`、`COM[1-9]`、`LPT[1-9]`（大小写不敏感）
5. **规范化后重新校验**：统一分隔符、折叠 `.` 段，对规范化结果重复校验 1-4
6. **前缀断言**：`final_path = canonical(save_root / rel_path)` 必须以 `save_root` 为前缀（防符号链接竞争绕过）

`save_root` 固定为 `Downloads/DashDrop/<transfer_id>/`（每次传输独立子目录，防跨传输覆盖）。

同传输内若两个 `file_id` 落盘路径相同 → 拒绝后一个，回复 `E_PATH_CONFLICT`。

### 5.2 允许的文件类型（MVP）

`FileItem.file_type` 枚举值：
- `RegularFile`：普通文件 — 允许
- `Directory`：目录 — 允许（仅创建目录结构，不传 Chunk）
- 其余类型（符号链接、硬链接、设备文件、socket、macOS bundle）— **MVP 不允许**

发送端**不得**将非 MVP 类型文件放入 `Offer.items`；接收端遇到不支持的 `file_type` 时，对该文件回复 `Ack { ok: false, reason: E_UNSUPPORTED_FILE_TYPE }`，连接维持，其他文件继续传输。

### 5.2a 目录（Directory）Complete 语义

目录不发送 `Chunk`，但仍必须发送 `Complete`，用于统一状态收敛。

规范：
1. `file_type=Directory` 时，`file_hash` 定义为“空字节流的 BLAKE3 值”（由实现计算，不使用随意魔法常量）。
2. 接收端对目录不做内容哈希比对，仅校验目录创建结果与路径合法性。
3. 目录 `Ack`：
   1. 创建成功 -> `ok=true`
   2. 创建失败（权限、路径冲突等）-> `ok=false` + 对应错误码

### 5.3 Cancel 语义

`Cancel` 消息作用范围：**整个 transfer**（不支持取消单个 file_id，留作 v0.2）。

**发送方发起 Cancel**（用户中途取消或出错）：
- 可在 Offer 发出后、最后 Ack 收到前任意时刻发送
- 接收端停止接收新 Chunk，关闭连接
- 已 `Ack { ok: true }` 的文件 → **保留**（已落盘，对接收用户可见）
- 已写入但未发 Ack 的文件 → **删除**（不保留不完整文件）
- 接收端无需发送回执，连接直接关闭

**接收方发起 Cancel**（用户中途拒绝）：
- 整个 transfer 终止，发送端收到后 emit `transfer_cancelled_by_receiver { reason: E_CANCELLED_BY_RECEIVER, succeeded_count, failed_count }`
- 已 `Ack { ok: true }` 的文件默认**保留**（符合用户直觉）
- 已写入但未 Ack 的临时文件**删除**
- 若产品需要“取消并删除已接收文件”，必须作为显式二次确认操作，不得作为 Cancel 隐式副作用

### 5.3a 选择性接收（非 MVP，协议预留）

当前 MVP 仍是整批接收或整批拒绝。为解决大批量混合文件场景，预留扩展：
1. `AcceptPayload` 增加可选字段 `accepted_file_ids: Vec<u32>`。
2. 缺省（字段缺失）表示接受全部，保持与旧版本兼容。
3. 发送端仅发送被接受文件，其余标记为 `RejectedByReceiverSelection` 写入结果。

### 5.4 PendingAccept 超时

`PendingAccept` 必须有双端超时，避免任一端失联导致无限等待：
1. 接收端等待用户处理超时：60 秒（从发出 `transfer_incoming` 起计时）。
2. 接收端超时行为：等价 `Reject { reason: E_TIMEOUT }`。
3. 发送端等待 `Accept/Reject` 超时：60 秒（从发出 `Offer` 起计时）。
4. 发送端收到对端超时 Reject 后：emit `transfer_rejected { reason: E_TIMEOUT }`。
5. 若发送端等待超时且未收到任何响应：本地终止，emit `transfer_failed { reason: E_TIMEOUT }`。
6. 由于 `PendingAccept` 阶段尚未传输文件，`succeeded_count` 必然为 0，不存在 `transfer_partial` 分支。

---

## 6. 安全边界

### MVP 安全模型：LAN Proximity Trust

**提供的保证**：
- 传输内容 TLS 1.3 加密，被动嗅探者无法读取
- 已配对关系内发生 fp 演进时触发告警（触发条件见下）

**发现层与连接层身份绑定规则（强制执行）**：

TLS 握手完成后立即校验：
```
cert_fp = SHA256(peer_tls_cert.public_key_der)

[A] 若本次连接来自 mDNS 发现的设备:
    要求 cert_fp == mDNS_advertised_fp
    不匹配 -> 关闭连接，emit "identity_mismatch"，UI 告警

[B] 查信任列表:
    cert_fp in trusted_peers -> 已配对，正常继续
    cert_fp not in trusted_peers -> 新设备，走首次连接流程
```

**`fingerprint_changed` 触发规则**：

`fingerprint_changed` **不按设备名判断**。设备名可变、可伪造、可重复，用设备名推断身份连续性会产生误报（同名两台机器）和漏报（改名后不告警），反而削弱真正的安全提示。

唯一正确的触发条件：**同一 mDNS session_id 下，本次握手的 cert_fp 与上次该 session 记录的 fp 不同，且上次记录的 fp 在信任列表中**。

```
若 session_id 在历史记录中存在（prev_fp 有记录）:
    若 cert_fp != prev_fp:
        IF prev_fp in trusted_peers:
            emit "fingerprint_changed"  # 已配对关系发生证书演进，真正的安全告警
        ELSE:
            视为新未配对设备，不告警   # prev_fp 本就不在信任列表，无配对关系可演进
若无历史记录:
    新会话，执行 [B] 信任列表查询
```

**手动 IP:port 连接的首次身份规则**：

手动连接无 mDNS fp，规则 [A] 不适用。流程：
1. 建立 TLS 连接，提取 `cert_fp`
2. 查信任列表（规则 [B]）：
   - 已在列表 → 已配对，正常
   - 不在列表 → 首次连接弹窗，**向用户展示完整 fingerprint**，要求人工核验
3. 用户确认 → 加入信任列表，后续等同已配对设备

手动连接不得偷偷自动信任或跳过 fp 展示，否则手动和 mDNS 两条路径形成两套安全模型。

**MVP 明确不提供的保证**：
- 首次配对（mDNS 或手动）无法防御主动 MITM
- 不使用共享密码，但不等同于"不能伪造身份"
- 速率限制计数为内存态，应用重启后重置（可被重启绕过）

**UI 规范**：
- 首次连接（mDNS）：显示"首次连接，无法自动验证身份，请确认对方在你身边"
- 首次连接（手动 IP）：显示完整 fingerprint，提示"请与对方设备屏幕上的指纹核对"
- 已配对：显示"已配对"，不使用"已验证"
- `fingerprint_changed`：显示"此设备证书已更换，可能存在安全风险"
- `identity_mismatch`：显示"该连接身份与广播信息不符，已拒绝"

---

## 7. 速率限制

速率限制需要**两层叠加**：

**精细层（按 fingerprint）**：
- 已配对设备：每 fingerprint 每分钟最多 **10 次** Offer
- 未配对设备：每 fingerprint 每分钟最多 **3 次** Offer
- 作用：约束诚实客户端的重复请求

**粗粒度兜底层（按来源地址 / 并发连接数）**：
- 每来源 /24 子网（IPv4）或 /48（IPv6）每分钟最多 **20 次**入站连接（含握手）
- 全局同时在握手中的未配对连接数不超过 **10 个**（超出直接拒绝 QUIC 握手）
- 作用：阻止攻击者通过换 fingerprint（生成新自签证书）绕过精细层限制，因为 fingerprint 是可自由生成的，纯 fingerprint 限流对恶意客户端无效

超限时精细层返回 `E_RATE_LIMITED`；粗粒度层在 QUIC 握手阶段直接拒绝，不进入协议层。速率计数内存维护，重启后重置。

---

## 8. 扩展点与路线

| 功能 | 扩展方式 |
|------|----------|
| 断点续传 | v0.2：ChunkPayload 补块级哈希 + SQLite 持久化块清单 |
| 带外验证（二维码配对）| v0.2：生成配对码，解决首次配对 MITM |
| 剪贴板同步 | 新增 `caps=clipboard`，新消息类型 `ClipboardSync` |
| BLE 发现 | 替换/补充发现层，Hello 握手在连接层，天然兼容 |
| 手动添加（IP:port）| 已纳入 M1：先连接并展示 fp 人工核验，确认后进入 PendingAccept |
| 中继模式 | 新增 `relay` 连接类型，数据仍 E2E 加密 |

---

## 9. 协议演进与迁移策略

### 9.1 版本字段职责

- `wire_version`：仅用于 `Hello` 固定外壳可解析性；当前固定 `0`，不得随业务变更而修改。
- `supported_versions`/`chosen_version`：业务协议版本协商字段；允许随版本演进扩展。

### 9.2 兼容策略

1. Minor 演进（向后兼容）：
- 仅新增可选字段或新增消息变体，旧端可忽略。
- 不允许删除已有必选字段，不允许改变现有字段语义。

2. Major 演进（可能不兼容）：
- 必须通过 `supported_versions` 协商；交集为空时返回 `E_VERSION_MISMATCH`。
- 新旧协议并行期至少保留 `N` 与 `N-1` 两个版本。

3. 事件契约稳定性：
- 终态事件名固定为 6 个：`transfer_complete` / `transfer_partial` / `transfer_rejected` / `transfer_cancelled_by_sender` / `transfer_cancelled_by_receiver` / `transfer_failed`。
- 新版本禁止引入额外终态事件名；扩展信息通过 payload 字段增加。

### 9.3 弃用流程

1. 标记阶段：在文档和代码注释中标记 `deprecated_since`。
2. 双写阶段：发送端可同时写新旧字段，接收端优先读新字段并兼容旧字段。
3. 移除阶段：至少跨一个 minor 版本后移除旧字段。

### 9.4 升级回滚策略

1. 客户端升级后若协商失败，必须在 UI 显示“版本不兼容”并给出下一步（升级另一端或降级当前端）。
2. 任一版本迁移不得改变 `transfer_id`/`reason_code(E_*)`/终态事件名语义，保证历史和监控统计可连续。
3. 数据库迁移必须可回滚：新列默认可空，旧列保留至少一个版本周期。
