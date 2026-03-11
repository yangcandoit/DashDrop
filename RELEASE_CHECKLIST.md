# DashDrop 发布前清单（Release Checklist）

本文档面向“可发布软件”标准，按优先级拆分为 `P0 / P1 / P2`，可直接作为后续迭代与验收基线。

## P0（发布阻断项，必须完成）

### 1. 可靠性与回归防线
- [~] 补齐后端关键集成测试（**进行中：单元/契约测试已补强，真实双端集成未完成**）：
  - [ ] 设备发现上下线（含 `device_lost` 事件）
  - [ ] **Windows/Linux 实机发现链路测试（mDNS + beacon fallback）**（含虚拟网卡、组播受限与广播受限场景）
  - [x] 发起侧证书指纹强绑定（错误证书必须拒绝）*（契约/单测覆盖）*
  - [~] 多文件并发传输（100 次以上压力回归）*（已补 120 轮语义压力回归；真实双端吞吐压测未完成）*
  - [ ] **跨平台大文件双向互传** (Mac <-> Win, Linux <-> Win 等混合平台，含 5GB 以上文件)
  - [x] Cancel 语义（已确认保留、未确认删除）*（契约/单测覆盖）*
  - [x] 本地 IPC 服务在无预先 Tokio runtime 的 GUI 启动阶段可正常拉起 *（回归测试 + 2026-03-11 `npm run test:tauri:smoke`）*
- [~] 补齐前端 E2E（Playwright）最小闭环（**已完成核心流程，当前为 mock IPC 的真实 UI 自动化**）：
  - [x] incoming -> accept -> history 可见
  - [x] incoming -> reject -> history 可见
  - [x] expired incoming action -> `E_REQUEST_EXPIRED` 可见且不残留活动任务
  - [x] connect by address 输入/确认流程
  - [x] 终态触发 History 自动刷新
  - [x] `identity_mismatch` 告警展示
  - [x] 批量取消与发送任务重试入口

### 2. 安全底线
- [~] 验证三平台密钥存储行为（**部分实现**）：
  - [x] macOS Keychain（**已实现**）
  - [x] Windows Credential/DPAPI 路径（**已实现**）
  - [x] Linux Secret Service（不可用时降级 + UI 风险提示已实现，仍需多发行版实测）
- [x] 明确并实现“安全存储不可用”策略（本轮为“降级可用 + 明确告警”）。
- [x] 建立安全事件审计最小集（`identity_mismatch` / `handshake_failed` 已落地 SQLite）。
- [x] `fingerprint_changed` 告警链路与审计落地（事件 emit + UI 消费 + SQLite 记录）。
- [x] 未信任设备发送前强制指纹确认（Nearby 首次发送确认弹窗，可选立即配对）。
- [x] 首次信任 UI 提供双方一致的 shared verification code，并要求未信任发送/接收/按地址确认前显式核对。

### 3. 传输一致性
- [x] 校验所有失败事件 payload 统一为：`transfer_id + reason + phase`（已通过统一 emit 函数收口）。
- [x] 确保取消路径不会残留临时文件（`*.dashdrop.part` 清理完整）（**已实现：见 `receiver.rs` 的 abort 清理逻辑**）。
- [x] 发送端等待 `Accept/Reject` 超时（已补齐，offer 阶段超时 emit `E_TIMEOUT`）。
- [x] `USER_RESPONSE_TIMEOUT_SECS` 与协议目标对齐（已改为 60s）。
- [x] `reason_code` 对齐协议 `E_*` 编码（sender 路径已改为协议编码）。
- [x] 目录项生命周期对齐协议（sender/receiver 目录 `Complete/Ack` 语义已补齐）。
- [x] Probe close code 对齐（已改为 `0xD0`）。
- [x] fingerprint 级 Offer 限流（trusted/untrusted 指纹窗口限制已落地）。
- [x] 校验 mDNS 改名重注册失败时 UI 可见、且配置回滚一致（已补 `system_error` 可见性并保持回滚）。

### 4. 工程门禁
- [x] CI 增加强制门禁（`.github/workflows/ci.yml`）：
  - [x] `cargo check`
  - [x] `cargo test`
  - [x] `npm run build`
  - [x] E2E（`npm run test:e2e` Playwright UI 自动化）
  - [x] E2E contract（`npm run test:e2e:contract` 脚本级契约）
  - [x] `cargo clippy --all-targets --all-features`
- [x] 安全扫描门禁与定时任务（`.github/workflows/security-audit.yml` + GitHub Code Scanning default setup）。
- [x] 依赖自动更新（`.github/dependabot.yml` 覆盖 GitHub Actions / npm / cargo）。
- [x] 安装包流水线与发布资产上传（`.github/workflows/build-installers.yml`，含 `SHA256SUMS.txt`）。

## P1（高优先级，建议首个小版本完成）

### 0. 目标态前置规范收口（AirDrop-like 设计实施前）
- [ ] 固化 daemon 本地 IPC 规范（Unix socket/Named pipe、认证、权限边界、命令集合）。
- [ ] 固化 SoftAP 安全与交互策略（一次一密凭据、用户显式同意、单网卡风险提示）。
- [ ] 固化通知权限降级路径（托盘角标 + 前台 pending 队列），禁止“通知被禁用即静默超时”。
- [ ] 固化跨 VLAN/子网边界提示策略（默认不支持自动发现，UI 指引 connect-by-address）。
- [x] 固化 Windows 防火墙策略：QUIC 固定端口 `53319/udp` 优先 + 端口占用时 fallback 随机端口 + 诊断输出 `listener_port_mode/firewall_rule_state` 已落地；安装期/首次提权引导仍见 P2 文档项。
- [x] 固化通知生命周期：超时/取消/终态时强制撤回通知，过期点击统一返回 `E_REQUEST_EXPIRED`。
- [x] 固化断点恢复一致性：恢复前校验 `source_snapshot(size/mtime/head_hash)`，不一致必须整文件重传。
- [ ] 固化进度持久化写盘策略：SQLite 批量落盘（时间窗或字节窗）、WAL、单写线程。
- [~] 固化能耗与隐私策略（**已实现电源状态驱动的 beacon 降频与诊断暴露；休眠联动与 BLE rolling identifier 仍未完成**）。
- [ ] 明确 BLE 能力落地阶段（探测基线与凭据胶囊分发）及无 BLE 回退链路（二维码/短码）。

### 1. 产品功能完整度
- [x] 传输任务管理增强：重试、批量取消、失败项重传（**已实现批量取消、整任务重试与 partial 失败项按文件级重传**）。
- [x] 传输历史页：按设备/时间筛选、失败原因可追踪（**已实现关键词/方向/状态/时间窗口筛选**）。
- [x] 配对管理增强：备注、撤销确认、最近使用时间（**已实现 alias 编辑、unpair 二次确认、last_used_at 展示**）。

### 2. 传输能力优化
- [~] 限速与资源策略（前台/后台、按网络条件）（**已实现并发流资源上限配置 `max_parallel_streams`；网络条件自适应限速未实现**）。
- [x] 文件冲突策略可配置（覆盖/重命名/跳过）（**已实现后端策略生效 + 设置页配置入口**）。
- [ ] 大文件与海量小文件性能优化（队列/缓冲参数调优）（**未实现**）。
- [ ] 1:N 单读多发（fan-out）读写层拆分（shared reader + per-target writer + bounded buffer + 背压隔离）。

### 3. 可观测性
- [~] 结构化日志字段标准化（transfer_id、peer_fp、phase、error_code）（**关键 sender/receiver/handshake 路径已补齐，仍需全量统一**）。
- [x] 关键指标采集：成功率、平均耗时、失败分布、取消率（**已实现 SQLite 聚合统计：终态计数、收发字节、平均耗时、失败原因分布**）。

## P2（中优先级，体验与可维护性优化）

### 1. 用户体验
- [x] 首次使用引导（权限、配对、安全说明）（**已实现首启 Onboarding，可本地持久化关闭**）。
- [x] 设置页增强（网络状态、服务状态、重注册状态反馈）（**已显示端口、mDNS 注册状态、发现/信任设备数量与运行指标**）。
- [x] 错误提示可操作化（给出下一步建议）（**已统一为 “问题 + Next steps” 模板并覆盖传输/安全/系统关键告警路径**）。
- [x] Transfers / History / Security Events 关键失败路径用户可见（**已补错误横幅、弹窗或重试入口，避免仅 console log**）。
- [x] 完善关于 Windows 防火墙（UDP 5353、UDP 53318、固定 `53319/udp` + fallback 随机端口）和 Linux Avahi/ufw 冲突的官方文档引导（见 `docs/NETWORK_TROUBLESHOOTING.md`）；更细的弹窗引导可继续增强。
- [x] Windows 本地 IPC baseline（named pipe server/client）已实现；更细的认证/权限策略仍见 P1 规范项。
- [x] 外部文件分享/打开入口基础链路已实现（启动参数 + 本地 IPC -> Nearby 队列）；完整系统级 share integration 仍可继续增强。
- [x] second-instance handoff 基础已实现（`app/activate` 经本地 IPC 把激活/分享路径转交给已运行实例）；完整 daemon/system share 仍未完成。

### 2. 架构与长期维护
- [x] 持久化方案统一评估（JSON vs tauri store vs sqlite）并固化（**已收口为 SQLite 作为唯一运行时持久化，`state.json` 仅保留一次性迁移读取**）。
- [x] 协议演进策略文档（版本兼容、弃用策略、迁移策略）（**已补充于 `PROTOCOL.md` §9**）。
- [x] 发布说明模板与升级迁移说明模板（**已补 `docs/RELEASE_NOTES_TEMPLATE.md` 与 `docs/UPGRADE_MIGRATION_TEMPLATE.md`，并配置 `.github/release.yml` 分类模板**）。

## 建议执行顺序

1. P0 测试与 CI 门禁  
2. P0 安全与一致性收口  
3. P1 功能增强与性能优化  
4. P2 体验与维护性提升

## 发布验收标准（Definition of Done）

- [ ] 所有 P0 项打勾（当前仍有未完成项）  
- [x] CI 全绿且可重复  
- [ ] 至少一轮多设备实机冒烟通过（发现、传输、取消、异常）  
- [ ] 已产出发布说明（已知限制与风险透明）
