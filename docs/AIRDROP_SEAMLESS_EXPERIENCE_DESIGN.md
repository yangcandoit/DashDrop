# DashDrop 无缝体验设计（AirDrop-like）

更新时间：2026-03-10  
状态：Proposed（面向下一阶段实施）

---

## 1. 目标定义

本设计的目标不是“像 LocalSend 一样能传文件”，而是尽可能实现接近 AirDrop 的无缝体验：

1. 发送动作发生在系统上下文，不要求先打开主应用。
2. 接收动作通过系统通知即可完成，不要求切换到主界面。
3. 设备发现与可用性由后台常驻服务维护，不依赖前台窗口生命周期。
4. 对可信设备做到低摩擦直达，对未知设备保持安全确认。
5. 网络抖动与短时中断可自动恢复，用户无需重复操作。

---

## 2. “无缝”体验标准（体验契约）

### 2.1 用户侧 SLO

1. 同网段设备可见延迟：P50 <= 2s，P95 <= 5s。
2. 从“系统分享”到“对端收到可操作通知”延迟：P50 <= 2s，P95 <= 6s。
3. 已配对设备发送步骤：不超过 2 步（选择设备 -> 发送）。
4. 未配对设备发送步骤：不超过 3 步（选择设备 -> 指纹确认 -> 发送）。
5. 网络恢复后设备在线状态恢复：<= 8s（无需重启应用）。

### 2.2 工程侧 SLO

1. 后台常驻发现可用性：>= 99.9%（进程级健康自恢复）。
2. 传输成功率（同网段、目标在线、权限正常）：>= 99%。
3. 接收确认超时与失败均可解释（可操作文案 + 诊断字段）。
4. 无“静默失败”：任意失败路径必须沉淀可追踪 reason_code 与 phase。

---

## 3. 关键差距（当前实现 -> AirDrop-like）

当前已具备：QUIC 加密传输、mDNS + beacon 发现、基本配对与探活、诊断导出。  
关键差距在“系统级产品形态”而非单点协议能力：

1. 缺少常驻后台服务（UI 进程退出后发现和接收能力下降）。
2. 缺少系统分享入口（Finder/Explorer/Share Target）。
3. 缺少通知级接收动作（Accept/Reject 在通知中心直接处理）。
4. 缺少带外配对（TOFU 仍需升级为可验证配对）。
5. 缺少断点续传与后台队列恢复。

---

## 4. 总体架构（目标态）

```mermaid
flowchart LR
  UI["UI Shell (Tauri Window)"] <-->|"Local IPC"| DAEMON["DashDrop Service (always-on)"]
  SHARE["System Share Entry\n(Finder/Explorer/Share Target)"] -->|"Local IPC"| DAEMON
  NOTIFY["System Notification Center"] <-->|"accept/reject actions"| DAEMON
  DAEMON --> DISC["Discovery Engine\nmDNS + UDP Beacon (+ BLE optional)"]
  DAEMON --> TRANS["Transfer Engine\nQUIC/TLS + Resume + Queue"]
  DAEMON --> TRUST["Trust Engine\nPairing + Fingerprint Policy"]
  DAEMON --> STORE["SQLite + Secure Store"]
```

### 4.1 架构原则

1. 传输与发现必须以后台 service 为真源（source of truth）。
2. UI 仅做控制面和可视化，不承载关键网络状态机。
3. 所有入口（主界面、系统分享、右键菜单）都走同一服务 API。
4. 协议与状态事件向后兼容，终态事件命名保持稳定。

---

## 5. 平台集成设计

### 5.1 macOS

1. 菜单栏常驻（Menu Bar）+ 登录启动（LaunchAgent）。
2. Finder 分享入口（Share Extension / Quick Action）。
3. 接收通知支持 action button：`Accept` / `Decline`。
4. 权限模型：
   1. Local Network（Bonjour/mDNS）
   2. Notifications
   3. 文件访问（用户选择路径范围）

### 5.2 Windows

1. 托盘常驻 + 开机启动。
2. Explorer 右键发送入口（Shell Extension/Context Menu）。
3. Windows Notification with actions（Toast action callbacks）。
4. 防火墙规则引导：
   1. mDNS: UDP 5353
   2. Beacon: UDP 53318
   3. QUIC listener: 随机 UDP 端口（应用进程）

### 5.3 Linux（阶段性）

1. 托盘常驻 + autostart `.desktop`。
2. 文件管理器入口优先支持常见桌面环境（Nautilus/Dolphin）。
3. 通知动作依赖 Desktop Notifications 能力矩阵，先提供统一降级路径。

---

## 6. 发现链路设计（Seamless Discovery）

### 6.1 双通道策略

1. 主通道：mDNS（设备身份 + 会话信息 + 端口）。
2. 兜底通道：UDP beacon（组播受限时保持可见性）。
3. 统一会话模型：`sessions[session_id]` 聚合，同一 fingerprint 合并显示。

### 6.2 地址与可达性策略

1. 地址候选来自所有 session，按最新 session 优先，IPv4 优先，IPv6 作为后备。
2. Probe 采用轻量 QUIC preflight，不触发业务传输状态机。
3. 降级规则：
   1. 单次 probe 失败不立刻判离线。
   2. 连续失败阈值后进入 `offline_candidate`。
   3. session 全失活 + 宽限期后进入 `offline`。

### 6.3 浏览器自恢复

1. `Timeout` 视为正常 idle，不中断。
2. `Disconnected` 触发 browse 重建，指数退避（上限 5s）。
3. 重建状态写入 diagnostics：`active/restart_count/last_disconnect_at`。

---

## 7. 信任与安全设计（Seamless but Safe）

### 7.1 身份校验基线

1. 发送侧强绑定：`selected_peer_fp == cert_fp`，不匹配直接中止。
2. 接收侧差异告警：`mdns_fp != cert_fp` 记录安全事件并提示用户。
3. 已配对关系中 fingerprint 演进触发 `fingerprint_changed` 风险提示。

### 7.2 配对升级（摆脱纯 TOFU）

1. 引入带外配对（二选一）：
   1. 二维码扫描配对
   2. 6 位短码双端确认
2. 配对完成后授予“低摩擦发送/接收”能力。
3. 配对关系支持：
   1. alias
   2. paired_at / last_used_at
   3. 一键撤销 + 风险冻结

### 7.3 策略矩阵

1. Trusted:
   1. 可配置自动接收
   2. 发送默认免二次确认
2. Untrusted:
   1. 发送前指纹确认
   2. 接收必须显式确认
3. Suspicious（identity mismatch / fingerprint changed）:
   1. 强制人工确认
   2. 持久化安全事件

---

## 8. 传输体验设计（Seamless Transfer）

### 8.1 统一入口

所有发送入口（主界面拖拽、系统分享、右键菜单）统一进入同一发送管线：

1. 目标选择
2. 身份策略检查
3. 连接建立与 Hello
4. Offer/Accept
5. 数据传输与结果聚合

### 8.2 断点续传与后台恢复

1. 每文件分块持久化块清单（SQLite）。
2. 连接中断后可按块恢复，不重传已确认块。
3. 应用前台退出不影响后台进行中的任务（由 service 持有状态）。

### 8.3 性能与稳定

1. 并发流按网络质量自适应（而非固定值）。
2. 大批小文件走批次调度，减少控制流抖动。
3. 失败重试优先文件级，而非整任务重发。

---

## 9. 通知与交互设计

### 9.1 发送侧

1. 发起后立即显示系统 toast：`Sending to <device>`。
2. 失败时 toast 包含“下一步建议”（重试/检查防火墙/重新配对）。
3. 完成后可一键“Open History”。

### 9.2 接收侧

1. 收到请求时系统通知卡片包含：
   1. 发送方设备名
   2. 文件数量/总大小
   3. Trust 状态
2. 通知直接操作：
   1. `Accept`
   2. `Decline`
3. 过期策略：超时自动拒绝并反馈明确 reason_code。

---

## 10. 可观测性与诊断

### 10.1 用户可导出诊断（已存在并继续扩展）

1. listener 模式与地址族
2. browser 状态与重建计数
3. discovery 事件计数与失败分类
4. 每设备 resolve/probe 摘要
5. quick_hints 自动归因

### 10.2 开发/运维指标

1. 发现成功率（mDNS vs beacon）
2. 连接成功率（首次成功率、重试成功率）
3. 平均首包时间、平均完成时间
4. 失败原因分布（timeout/handshake/connect/protocol）
5. 平台分布与版本分布

---

## 11. 分阶段实施计划

### Phase A（系统化底座）

1. 拆分后台 service 与 UI shell。
2. 建立本地 IPC 协议与权限边界。
3. 托盘/菜单栏常驻 + 开机自启。

验收：
1. UI 关闭后发现与接收能力仍在线。
2. 重开 UI 可秒级恢复完整状态。

### Phase B（系统入口与通知闭环）

1. macOS Finder/Share Extension。
2. Windows Explorer 右键发送。
3. 通知中心 Accept/Decline 动作回调。

验收：
1. 全流程无需先打开主窗口。
2. 接收动作可在通知中心完成。

### Phase C（可信无感）

1. QR/短码带外配对。
2. trusted 低摩擦策略（可配置自动接收）。
3. 风险设备策略（冻结/强提醒）。

验收：
1. 已配对设备发送 <= 2 步。
2. 风险场景无静默放行。

### Phase D（可靠性与恢复）

1. 断点续传。
2. 后台队列持久化恢复。
3. 自适应并发与重试策略。

验收：
1. 中断恢复后不从 0 重传。
2. 长时运行无明显状态漂移。

---

## 12. 风险与边界

1. AirDrop 使用 Apple 私有生态（如 AWDL）；跨平台无法 1:1 复刻底层链路。
2. Windows/Linux 平台通知动作与系统分享入口能力碎片化，需要分平台实现。
3. 后台常驻服务引入更高运维复杂度（升级、崩溃恢复、权限管理）。
4. 带外配对需兼顾易用与安全，避免把流程做重。

---

## 13. 非目标（本设计不覆盖）

1. 跨公网中继/NAT 穿透。
2. 云账号体系与远程同步。
3. 移动端完整生态（可后续扩展）。

---

## 14. Definition of Done（AirDrop-like 门槛）

满足以下条件才可对外宣称“AirDrop-like”：

1. 系统入口可直接发送（不先开主应用）。
2. 系统通知可直接接收（不切主界面）。
3. 已配对设备低摩擦直达，未知设备安全确认。
4. 后台常驻 + 网络抖动自动恢复。
5. 诊断可直接定位发现/连接/协议/权限问题，不靠猜测。

