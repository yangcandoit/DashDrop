# DashDrop 设计蓝图（v1）

本文档定义 DashDrop 的产品与系统设计基线，目标是把“能跑通”升级为“可长期演进、可稳定交付”的方案。

---

## 当前实现状态（2026-03-11）

1. 已完成：
- 状态契约收口（含 `transfer_accepted`）与终态事件统一。
- DTO 边界替换（前端不再直接依赖 `SocketAddr/Instant`）。
- 拖拽发送改单通道，`sendingTo` 生命周期绑定全局传输状态。
- CI 基础门禁与测试执行链路上线。
- Playwright UI E2E（mock IPC 驱动）与 Security Events 视图。
- 失败项按文件级重传（failed-file-only retry）已落地。
- 发现链路已升级为 mDNS + UDP beacon fallback。
- 前端状态读取已补齐契约兜底，Transfers/History 不再因空载荷直接崩溃。
- 首次未配对发送失败时保留确认上下文，避免误导成“已完成发送”。
- History / Security Events / Transfers 的关键失败路径已补用户可见反馈与重试入口。
- 主壳层已补窄窗口导航重排，避免固定侧栏在小窗口下挤压内容区。
- 本地 IPC 启动链路已修正为非 Tokio runtime 预绑定路径，`npm run test:tauri:smoke` 已覆盖真实启动链路并验证 setup 阶段不再因 listener 绑定崩溃。
- Windows 本地 IPC named pipe 基线实现已补齐，不再停留在 placeholder。
- 首次信任相关 UI 已统一显示双方一致的 shared verification code，且未信任发送/接收/按地址连接要求显式确认后才能继续。
- 外部文件打开/分享路径现在可进入 Nearby 的待发送队列，作为 system share/daemon 方向的第一段基础能力。
- 第二实例启动时现在会优先走本地 IPC handoff，把激活/分享路径转交给已运行实例，作为 single-instance 基础。

2. 进行中：
- 真实双端集成测试（QUIC 多机编排与压力验证）。
- 设备离线/丢失语义与长时运行策略细化（`Offline` 保留窗口与 `device_lost` 生命周期权衡）。

3. 待完成：
- 真实多端压测闭环（含跨平台长时稳定性与异常注入）。

---

## 1. 产品目标与边界

### 1.1 产品目标

DashDrop 是一款跨平台桌面端近场文件传输工具，核心价值是：

1. 零配置：同一局域网内自动发现设备。
2. 安全默认：传输全程加密，身份变化可感知。
3. 低心智负担：发送和接收流程尽量一步完成。
4. 可恢复：异常后用户知道“发生了什么”和“下一步怎么做”。

### 1.2 非目标（当前阶段）

1. 不做云同步，不依赖中心服务。
2. 不做跨公网/NAT 穿透。
3. 不承诺后台守护传输（应用退出即中断）。
4. 不做移动端深度适配（先保证桌面三平台）。

---

## 2. 设计原则

1. 先保证正确性，再追求峰值性能。
2. 安全提示要“诚实”，不伪造“已验证”语义。
3. 用户心智优先于技术结构，状态必须可解释。
4. 所有跨层契约必须可版本化、可兼容演进。
5. 失败是常态，恢复路径必须成为一等设计对象。

---

## 3. 功能域划分（Domain）

### 3.1 Discovery（发现域）

职责：
1. 广播本机可达信息。
2. 聚合多网卡多会话为“单个设备视图”。
3. 输出“可连接候选地址集合”而非单地址。

边界：
1. 不决定是否信任设备。
2. 不承诺连接一定成功（仅提供候选）。

### 3.2 Trust（信任域）

职责：
1. 管理设备长期身份（fingerprint）及配对关系。
2. 处理身份变化告警与用户确认。
3. 输出“首次设备/已配对设备/风险设备”三态。

边界：
1. 不负责文件传输。
2. 不接管发现策略。

### 3.3 Transfer（传输域）

职责：
1. 管理传输会话生命周期（发起、等待、进行、完成、失败、取消）。
2. 输出进度、结果和失败原因。
3. 支持“全部成功/部分成功/全部失败”三态。

边界：
1. 不维护长期信任关系。
2. 不负责设置持久化策略。

### 3.4 History（历史域）

职责：
1. 记录传输摘要、结果、时间和目标设备。
2. 支持问题追溯与简单筛选。

边界：
1. 不保存文件内容。
2. 不作为安全审计替代品。

### 3.5 Settings（配置域）

职责：
1. 管理设备名、下载目录、默认接收策略。
2. 提供全局策略配置入口，并跳转至 `Trusted Devices` 管理信任关系。

### 3.6 Diagnostics（诊断域）

职责：
1. 聚合系统错误、网络错误、协议错误。
2. 输出可操作建议（重试、改目录、检查权限、重新配对）。

---

## 4. 信息架构（IA）

主导航应从“二页切换”升级为“五区”：

1. Nearby（附近）
2. Transfers（进行中）
3. History（历史）
4. Trusted Devices（已配对）
5. Settings（设置）

设计理由：
1. Nearby 专注“当前可见与可发”。
2. Transfers 专注“正在发生”。
3. History 专注“发生过什么”。
4. Trusted Devices 专注“关系治理”。
5. Settings 专注全局配置，避免塞入安全管理。

---

## 5. 核心用户流程

### 5.1 首次发送（未配对）

1. 用户在 Nearby 看到目标设备。
2. 选择文件并发起传输请求。
3. 对端在 `Transfers` 页 `Incoming Requests` 出现请求卡片，显示发送方名称、指纹摘要、文件清单。
4. 对端接受后开始传输。
5. 完成后双方显示结果，若首次可引导“加入已信任”。

### 5.2 首次接收（被动）

1. 收到请求时进入 `IncomingQueue`，以请求卡片形式可延迟处理（队列化）。
2. 用户可接受、拒绝、接受并配对。
3. 若请求超时，发送方收到明确超时失败原因。

### 5.3 已配对直传

1. 发送方直传，接收方按配置可自动接收或静默确认。
2. 出现指纹变化时，自动降级为高风险流并强制人工确认。

### 5.4 异常恢复

1. 网络断开：显示可重试入口。
2. 路径/权限错误：显示可修改设置入口。
3. 身份不匹配：显示“中止并重新验证”入口。

---

## 6. 统一状态模型

### 6.1 设备状态

1. `Offline`
2. `Discovered`（仅发现）
3. `Reachable`（探活通过，可连接）
4. `OfflineCandidate`（探活连续失败，疑似离线）
5. `Risk`（身份冲突或可疑变更）

### 6.2 信任状态

1. `Untrusted`
2. `Trusted`
3. `TrustSuspended`（指纹变化后冻结）
4. `Stale`（长期未见，待清理）
5. `Replaced`（已被新指纹替换，归档）

### 6.3 传输状态

1. `Draft`（待发起）
2. `PendingAccept`
3. `Transferring`
4. `Completed`
5. `PartialCompleted`
6. `Rejected`
7. `CancelledBySender`
8. `CancelledByReceiver`
9. `Failed`

约束：
1. `Completed` 与 `PartialCompleted` 互斥。
2. `Failed` 必须携带 `reason_code` 与 `user_message`。
3. 任何终态都要记录 `ended_at`。

---

## 7. 契约治理（前后端/协议）

### 7.1 IPC 契约分层

1. Command：请求-响应，必须幂等定义清晰。
2. Event：状态推送，必须可重入、可乱序恢复。
3. Snapshot：可随时拉取当前真相，用于前端自愈。

### 7.2 版本策略

1. 命令与事件分别维护 `schema_version`。
2. 新字段只增不删，旧字段至少保留一个小版本周期。
3. 客户端遇到未知字段必须忽略，遇到未知关键枚举必须降级显示。

### 7.3 错误模型

1. `DomainError`：业务拒绝（用户取消、未授权、不信任）。
2. `InfraError`：基础设施异常（网络、磁盘、权限）。
3. `ProtocolError`：协议不兼容/格式无效。

所有错误输出统一结构：
1. `code`
2. `phase`
3. `retryable`
4. `user_hint`

---

## 8. 安全与信任设计（产品层）

1. 默认加密是基础能力，不作为“高级功能”呈现。
2. 首次连接统一称“未验证设备”，避免“已验证”误导。
3. 指纹变化默认高优先级告警，阻断自动接收。
4. 手动 IP 连接必须展示完整指纹并要求用户确认。
5. 信任列表必须支持查看来源、配对时间、最后通信时间。

---

## 9. 可观测性与运维友好性

### 9.1 用户可见

1. 近期错误卡片（可复制诊断信息）。
2. 最近传输结果与失败分布。
3. 网络状态提示（仅提示，不做网络控制）。

### 9.2 开发可见

1. 统一事件日志 ID（transfer_id / peer_fp / session_id）。
2. 关键路径埋点：发现、连接、授权、传输、落盘、完成。
3. 崩溃恢复后可回放最近 N 条关键事件摘要。

---

## 10. 里程碑路线图（设计驱动）

### M1：MVP Hardening

1. 固化五区信息架构。
2. 固化统一状态机与错误模型。
3. 增加 Transfers/History/Trusted Devices 三页最小可用版本。

验收：
1. 关键流程可闭环。
2. 失败路径均有用户可执行下一步。

### M2：Beta Quality

1. 自动接收策略（仅 trusted 生效）。
2. 传输冲突与并发策略可配置。
3. 关键埋点与诊断导出。

验收：
1. 常见异常场景恢复成功率达标。
2. 用户无需看日志可定位主要失败原因。

### M3：GA Readiness

1. 首次配对增强（带外验证）。
2. 历史与信任治理完善（检索、清理、风险复核）。
3. 协议与 IPC 兼容策略文档化并落地测试。

验收：
1. 跨版本节点互通可控。
2. 安全提示与行为一致，无“提示正确、行为越权”问题。

---

## 11. 当前设计改进优先级（建议）

P0（实现阻断）：
1. IPC 终态事件映射落地（Rejected/Cancelled*/Failed 可区分）。
2. History 持久化（SQLite）与字段完整性落地。
3. 手动连接 fp 确认子状态与超时落地。

P1（近期实现）：
1. Reachable Probe 机制（含 UI 状态联动）。
2. Risk 设备完整交互（确认、阻断、恢复）。
3. 诊断可复制与错误追踪链路。

P2（中期演进）：
1. 选择性接收（per-file accept）。
2. 带外验证与更强配对模型。
3. 后台模式/守护进程评估（如仍追求更强恢复能力）。

---

## 12. 设计评审清单（每次迭代必过）

1. 这个改动属于哪个功能域？是否跨域越权？
2. 状态是否可解释？终态是否唯一？
3. 出错时用户是否知道下一步怎么做？
4. 是否引入新契约字段？兼容策略是否定义？
5. 是否影响信任模型？提示和行为是否一致？

---

## 13. v1.1 补丁：传输状态机完整语义（强制）

本节覆盖前端、后端、历史记录三处定义，作为唯一真相来源。

### 13.1 统一状态集合

`TransferStatus`：
1. `Draft`
2. `PendingAccept`
3. `Transferring`
4. `Completed`（终态）
5. `PartialCompleted`（终态）
6. `Rejected`（终态）
7. `CancelledBySender`（终态）
8. `CancelledByReceiver`（终态）
9. `Failed`（终态）

### 13.2 终态判定规则

1. 对端明确拒绝请求：进入 `Rejected`，错误码 `E_REJECTED_BY_PEER`。
2. 发送端主动取消：进入 `CancelledBySender`，错误码 `E_CANCELLED_BY_SENDER`。
3. 接收端在传输中主动取消：进入 `CancelledByReceiver`，错误码 `E_CANCELLED_BY_RECEIVER`。
4. 仅当全部文件 `Ack ok=true`：进入 `Completed`。
5. 存在成功文件且存在失败文件（无论错误中断还是文件级 Ack 失败）：进入 `PartialCompleted`，并附带 `terminal_cause`（如 `NetworkDropped` / `HashMismatch` / `DiskFull`）。
6. 网络中断/协议错误/磁盘错误等非用户主动动作，且成功文件数为 0：进入 `Failed`。

### 13.2a 终态优先级（冲突消解）

同一传输出现多类信号时按以下优先级归一，避免 `Failed` 与 `PartialCompleted` 冲突：
1. 用户明确动作终态：`Rejected` / `CancelledBySender` / `CancelledByReceiver`
2. 数据结果终态：`Completed` / `PartialCompleted`
3. 兜底异常终态：`Failed`

### 13.2b 结果模型（History 与 UI 共用）

每条传输记录输出：
1. `outcome`：`Completed | PartialCompleted | Rejected | CancelledBySender | CancelledByReceiver | Failed`
2. `terminal_cause`：可选，例 `NetworkDropped | DiskFull | HashMismatch | Timeout`
3. `succeeded_count`
4. `failed_count`

约束：
1. `outcome=Failed` 时 `succeeded_count` 必须为 `0`。
2. `succeeded_count>0 && failed_count>0` 必须归为 `PartialCompleted`，不得归为 `Failed`。

### 13.3 状态转移表（发送端视角）

1. `Draft -> PendingAccept`：发送请求成功发出。
2. `PendingAccept -> Transferring`：收到 `Accept`。
3. `PendingAccept -> Rejected`：收到对端 `Reject`。
4. `PendingAccept -> CancelledBySender`：本端用户取消。
5. `PendingAccept -> Failed`：握手/连接异常。
6. `Transferring -> Completed`：全部文件成功 Ack。
7. `Transferring -> PartialCompleted`：至少一个文件失败且至少一个成功。
8. `Transferring -> CancelledBySender`：本端用户取消。
9. `Transferring -> CancelledByReceiver`：收到对端取消。
10. `Transferring -> Failed`：网络/协议/存储异常。

### 13.4 状态转移表（接收端视角）

1. `Draft -> PendingAccept`：收到 `transfer_incoming`。
2. `PendingAccept -> Transferring`：本端用户接受。
3. `PendingAccept -> Rejected`：本端用户拒绝。
4. `PendingAccept -> Failed`：请求超时或请求体非法。
5. `Transferring -> Completed`：全部文件落盘并校验成功。
6. `Transferring -> PartialCompleted`：部分文件落盘成功。
7. `Transferring -> CancelledByReceiver`：本端用户取消。
8. `Transferring -> CancelledBySender`：收到发送端取消。
9. `Transferring -> Failed`：写盘/校验/协议异常。

### 13.5 History 映射（不可折叠）

History 列表必须保留以下可区分结果，不得把 `Rejected`、`Cancelled*` 归并成 `Failed`：
1. 成功：`Completed`
2. 部分成功：`PartialCompleted`
3. 被拒绝：`Rejected`
4. 我取消：`CancelledBySender`（若当前用户为发送方）或 `CancelledByReceiver`（若当前用户为接收方）
5. 对方取消：与上条相反方向
6. 异常失败：`Failed`

---

## 14. v1.1 补丁：IA 交互路径与入口归属

### 14.1 事件到界面的路由规则

`transfer_incoming` 触发后，必须同时发生：
1. 写入 `IncomingQueue`（数据层）。
2. 在 `Transfers` 页 `Incoming Requests` 分组出现卡片（主处理入口）。
3. 若当前不在 `Transfers` 页，显示非阻塞通知，点击跳转到 `Transfers`。

禁止策略：
1. 禁止堆叠阻塞弹窗作为唯一入口。
2. 禁止请求只能在瞬时弹窗中处理。

### 14.2 区域间流转

1. `Nearby`：发起传输入口，只展示“可发”信息。
2. `Transfers`：处理中任务与请求队列的唯一处理入口。
3. `History`：终态只读视图，可进入详情与重试。
4. `Trusted Devices`：信任关系管理，可回跳 `Nearby` 发起传输。
5. `Settings`：全局行为配置，不处理单次请求。

### 14.3 最小组件结构（建议）

1. `TransfersPage`
2. `IncomingRequestList`
3. `ActiveTransferList`
4. `TransferResultBanner`
5. `TransferDetailDrawer`

### 14.4 手动连接入口（IP:Port）

手动连接不属于“Nearby”空间隐喻，应单独建入口：
1. 入口位置：`Transfers` 页顶部 `Connect by Address`。
2. 发起后先进入 `ConnectingManual -> AwaitingFingerprintConfirm`，确认后才进入 `PendingAccept`；不创建“伪 Nearby 设备”。
3. 若用户选择“记住此设备”，写入 `Trusted Devices`，并标记来源 `source=manual`。
4. 手动连接首连文案不得使用“请确认对方在你身边”，应改为“请核对设备指纹”。

---

## 15. v1.1 补丁：Trust 与 Discovery 连接协议

### 15.1 数据分层约束

1. Discovery 实体不持久化 `trusted` 布尔值。
2. Trust 维护 `TrustRecord(fp, alias, paired_at, last_seen_at, status, source)`。
3. UI 展示层通过 `fp` 关联 `DiscoveryDevice` 与 `TrustRecord` 生成 `DeviceViewModel`。

### 15.2 合并规则（Join）

`DeviceViewModel = DiscoveryDevice LEFT JOIN TrustRecord ON fp`

输出字段：
1. `identity_label`：`Trusted` / `Untrusted` / `TrustSuspended`
2. `display_name`：优先 `Trust.alias`，其次 `Discovery.name`
3. `risk_flags`：`name_collision`、`fingerprint_changed`、`manual_unverified`

### 15.3 同名不同 FP（幽灵设备）处理

当同时出现 `name` 相同但 `fp` 不同设备时：
1. Nearby 卡片必须显示短指纹后缀（如 `...A1C9`）。
2. 旧 trusted 同名设备若不在线，仍在 Trusted 页保留但标 `Offline`。
3. 新同名未配对设备显示 `Untrusted`，且默认不自动接收。
4. 若 trusted 设备发生 fp 演进，置 `TrustSuspended` 并告警，不自动合并到新 fp。

### 15.4 旧配对治理（重装/换机/离线尸体）

为避免 Trusted 列表堆积“同名离线旧指纹”，必须提供治理闭环：
1. `last_seen_at` 超过阈值（固定 30 天，M1 不可配置）标记 `Stale`。
2. 当出现同名新设备且旧设备为 `Stale` 时，提供“替换信任关系”向导：
   1. 展示旧 fp 与新 fp 的短指纹对比。
   2. 用户确认后：旧记录转 `Replaced`，新记录升为 `Trusted`。
3. 不做静默自动合并，必须由用户确认。

---

## 16. v1.1 补丁：设备 Reachable 状态定义

### 16.1 Reachable 进入条件

满足以下任一条件进入 `Reachable`：
1. 最近一次主动探测成功（QUIC preflight 或轻量握手）且在 TTL 内。
2. 最近一次真实传输连接成功且在 TTL 内。

仅有 mDNS 发现事件时进入 `Discovered`，不能直接进入 `Reachable`。

### 16.2 探测策略

1. 触发时机：设备首次出现、用户进入 Nearby、用户 hover/聚焦卡片、发送前预检。
2. 探测频率：同一设备最短间隔 10 秒，避免网络噪声。
3. 成功 TTL：30 秒；超过后自动降级到 `Discovered`。
4. 连续 3 次探测失败且最近无成功连接：降级为 `OfflineCandidate`，UI 显示“可能离线”。

### 16.3 退出条件

1. 探测超时或失败超过阈值：`Reachable -> Discovered` 或 `OfflineCandidate`。
2. Session 全部移除且超过宽限期（例如 15 秒）：`-> Offline`。

---

## 17. v1.1 补丁：IPC 时序一致性与反撕裂规则

### 17.1 事件信封（Event Envelope）

所有状态型事件必须带：
1. `entity_type`（`device` / `transfer` / `trust` / `system`）
2. `entity_id`（如 `fp` 或 `transfer_id`）
3. `revision`（实体单调递增）
4. `emitted_at_ms`（服务端毫秒时间戳）
5. `schema_version`
6. `payload`

### 17.2 Snapshot 结构

`get_snapshot` 必须返回：
1. `snapshot_revision`（全局水位）
2. `entities`（每个实体含当前 `revision`）
3. `server_time_ms`

### 17.3 前端合并规则（必须实现）

1. 对同一 `entity_id`，仅应用 `revision` 更大的更新。
2. `revision` 相同则幂等覆盖，禁止重复副作用。
3. 处理快照后，只接收 `revision > local_revision` 的事件。
4. 若检测到跳号过大或回退（例如收到更小 revision），触发一次全量 `get_snapshot` 自愈。

### 17.4 事件语义收敛

1. `transfer_failed`：仅表示 `Failed` 终态，必须带 `reason_code`。
2. `transfer_rejected`：仅表示 `Rejected` 终态。
3. `transfer_cancelled_by_sender`：仅表示 `CancelledBySender` 终态。
4. `transfer_cancelled_by_receiver`：仅表示 `CancelledByReceiver` 终态。
5. `transfer_error`：非终态诊断事件，不改变状态机终态。
6. 若一次异常同时产生终态事件与 `transfer_error`，前端必须先处理终态，再附加显示诊断信息。

---

## 18. v1.2 补丁：未定义项收敛

### 18.1 Settings 与 Trusted Devices 职责边界

1. `Trusted Devices`：唯一的信任关系列表与操作入口（查看、移除、替换、重新验证）。
2. `Settings`：只放全局策略开关（如“仅对 trusted 自动接收”），不展示完整设备列表。
3. Settings 可提供跳转按钮“Manage Trusted Devices”，但不重复承载列表操作。

### 18.2 PendingAccept 超时规范

1. 默认超时：60 秒。
2. 接收端用户未处理超时：等价 `Reject(E_TIMEOUT)`，发送端终态为 `Rejected`。
3. 发送端等待 `Accept/Reject` 超时（未收到任何响应）：终态为 `Failed(E_TIMEOUT)`。
4. `PendingAccept` 阶段尚未进入数据传输，`succeeded_count` 恒为 `0`，不存在 `PartialCompleted` 分支。

### 18.3 History 重试语义

1. 可重试终态：`Failed`、`PartialCompleted`、`CancelledBySender`。
2. 不可直接重试终态：`Rejected`、`CancelledByReceiver`（需重新发起）。
3. 重试始终创建新的 `transfer_id`，旧记录保持不可变审计。
4. 重试默认复用“原文件列表快照”；若文件已不存在，提示用户重新选择。

### 18.4 Risk 设备交互流程

1. `Risk` 卡片样式：警示色边框 + 风险图标 + 文案“身份待确认”。
2. 点击 `Risk` 设备时不直接发送，先进入 `RiskConfirmDialog`。
3. `TrustSuspended` 设备默认阻断自动接收，发送前必须显式确认。

### 18.5 传输中信任状态变化

1. 若 `Transferring` 期间收到 `fingerprint_changed` / `identity_mismatch`：
   1. 立即停止后续块发送。
   2. 当前传输按结果模型收敛为 `PartialCompleted` 或 `Failed`。
   3. 对应设备信任状态切为 `TrustSuspended`。
2. UI 必须给出“已因身份风险中止”提示，不得伪装成普通网络失败。

### 18.6 发送方首次连接安全提示

1. 发送方在向 `Untrusted` 设备发起前，必须看到一次风险提示（非阻塞可确认）。
2. 提示内容：
   1. 该设备尚未验证。
   2. 建议核对指纹后再发送敏感文件。
3. 发送方可“继续一次”或“取消并查看设备详情”。
4. 触发频率：
   1. 默认同一设备每会话提示一次（基于 `peer_fp`）。
   2. 会话内连续发送不重复弹出，除非该设备进入 `Risk/TrustSuspended` 后再次尝试发送。
   3. 应用重启后重新计次。

### 18.7 Discovered / Reachable 的视觉与可交互性

1. `Discovered`：可见但弱强调，点击发送前必须先做预检探活。
2. `Reachable`：主交互态，可直接发送。
3. `OfflineCandidate`：点击时先触发一次即时重探活。
   1. 若重探活成功：升级为 `Reachable` 并继续发送流程。
   2. 若重探活失败：阻断发送，提示“设备可能已离线”，提供 `Retry Probe` 按钮。
4. `Offline`：默认不可发送（仅可查看详情/诊断）。

### 18.8 Verifying 展示规则

1. `Verifying` 定义为 `Transferring` 的子阶段，不单独作为终态。
2. UI 表现：进度可到 100%，但状态标签显示“Verifying integrity...”，直到 Ack 收齐进入终态。

### 18.9 手动连接中间状态（fp 确认）

手动连接在 `Draft -> PendingAccept` 之间必须经过指纹确认步骤：
1. `Draft -> ConnectingManual`：用户输入 `IP:port` 并发起连接。
2. `ConnectingManual -> AwaitingFingerprintConfirm`：TLS 建连成功，展示完整 fingerprint。
3. `AwaitingFingerprintConfirm -> PendingAccept`：用户确认后才允许发送 Offer。
4. `AwaitingFingerprintConfirm -> CancelledBySender`：用户取消。
5. `AwaitingFingerprintConfirm` 超时：60 秒，超时自动取消并关闭连接，终态为 `Failed(E_TIMEOUT)`。

注：`ConnectingManual` 与 `AwaitingFingerprintConfirm` 为“发送端预协商子状态”，不进入通用传输终态枚举。

History 记录规则：
1. 预协商阶段失败（`ConnectingManual` / `AwaitingFingerprintConfirm`）不写入传输 History。
2. 预协商失败写入 Diagnostics（`attempt_id` 维度），用于问题排查。
3. 仅当进入 `PendingAccept`（即已发送 Offer）后，才创建 transfer 记录并进入 History 统计口径。

### 18.10 History 持久化约束（M1 必须）

1. History 必须跨重启持久化，禁止仅内存存储。
2. 最小字段：
   1. `transfer_id`
   2. `peer_fingerprint`
   3. `peer_name_snapshot`
   4. `outcome`
   5. `terminal_cause`
   6. `succeeded_count`
   7. `failed_count`
   8. `started_at`
   9. `ended_at`
3. 存储介质：建议 SQLite（M1），至少支持最近 1000 条记录滚动保留。
