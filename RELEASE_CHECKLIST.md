# DashDrop 发布前清单（Release Checklist）

本文档面向“可发布软件”标准，按优先级拆分为 `P0 / P1 / P2`，可直接作为后续迭代与验收基线。

## P0（发布阻断项，必须完成）

### 1. 可靠性与回归防线
- [~] 补齐后端关键集成测试（**进行中：单元/契约测试已补强，真实双端集成未完成**）：
  - [ ] 设备发现上下线（含 `device_lost` 事件）
  - [x] 发起侧证书指纹强绑定（错误证书必须拒绝）*（契约/单测覆盖）*
  - [~] 多文件并发传输（100 次以上压力回归）*（已补 120 轮语义压力回归；真实双端吞吐压测未完成）*
  - [x] Cancel 语义（已确认保留、未确认删除）*（契约/单测覆盖）*
- [~] 补齐前端 E2E（Playwright）最小闭环（**已完成核心流程，当前为 mock IPC 的真实 UI 自动化**）：
  - [x] incoming -> accept -> history 可见
  - [x] incoming -> reject -> history 可见
  - [x] connect by address 输入/确认流程
  - [x] 终态触发 History 自动刷新
  - [x] `identity_mismatch` 告警展示

### 2. 安全底线
- [~] 验证三平台密钥存储行为（**部分实现**）：
  - [x] macOS Keychain（**已实现**）
  - [x] Windows Credential/DPAPI 路径（**已实现**）
  - [x] Linux Secret Service（不可用时降级 + UI 风险提示已实现，仍需多发行版实测）
- [x] 明确并实现“安全存储不可用”策略（本轮为“降级可用 + 明确告警”）。
- [x] 建立安全事件审计最小集（`identity_mismatch` / `handshake_failed` 已落地 SQLite）。
- [x] `fingerprint_changed` 告警链路与审计落地（事件 emit + UI 消费 + SQLite 记录）。
- [x] 未信任设备发送前强制指纹确认（Nearby 首次发送确认弹窗，可选立即配对）。

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

### 1. 产品功能完整度
- [x] 传输任务管理增强：重试、批量取消、失败项重传（**已实现批量取消、整任务重试与 partial 失败项按文件级重传**）。
- [x] 传输历史页：按设备/时间筛选、失败原因可追踪（**已实现关键词/方向/状态/时间窗口筛选**）。
- [x] 配对管理增强：备注、撤销确认、最近使用时间（**已实现 alias 编辑、unpair 二次确认、last_used_at 展示**）。

### 2. 传输能力优化
- [~] 限速与资源策略（前台/后台、按网络条件）（**已实现并发流资源上限配置 `max_parallel_streams`；网络条件自适应限速未实现**）。
- [x] 文件冲突策略可配置（覆盖/重命名/跳过）（**已实现后端策略生效 + 设置页配置入口**）。
- [ ] 大文件与海量小文件性能优化（队列/缓冲参数调优）（**未实现**）。

### 3. 可观测性
- [~] 结构化日志字段标准化（transfer_id、peer_fp、phase、error_code）（**关键 sender/receiver/handshake 路径已补齐，仍需全量统一**）。
- [x] 关键指标采集：成功率、平均耗时、失败分布、取消率（**已实现 SQLite 聚合统计：终态计数、收发字节、平均耗时、失败原因分布**）。

## P2（中优先级，体验与可维护性优化）

### 1. 用户体验
- [x] 首次使用引导（权限、配对、安全说明）（**已实现首启 Onboarding，可本地持久化关闭**）。
- [x] 设置页增强（网络状态、服务状态、重注册状态反馈）（**已显示端口、mDNS 注册状态、发现/信任设备数量与运行指标**）。
- [x] 错误提示可操作化（给出下一步建议）（**已统一为 “问题 + Next steps” 模板并覆盖传输/安全/系统关键告警路径**）。

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
