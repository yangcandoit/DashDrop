# 贡献指南 (Contributing Guide)

欢迎参与 DashDrop 的开发！以下是开始贡献前需要了解的事项。

---

## 环境准备

1. **Rust**（1.75+）：https://rustup.rs/
2. **Node.js**（20+）：https://nodejs.org/
3. **Tauri CLI 前置依赖**：https://tauri.app/start/prerequisites/

安装完成后在项目根目录执行：
```bash
npm install
```

---

## 开发启动

```bash
# 开发模式（热重载）
npm run tauri dev

# 仅运行前端（不启动 Tauri）
npm run dev

# 构建生产版本
npm run tauri build

# 回归校验（提交前建议）
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
npm run build
npm run test:e2e
npm run test:e2e:contract
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features
```

---

## 代码规范

### Rust 后端
- 目标：`cargo clippy` 无 warning（当前 warning 清单见 `STATUS.md`）
- 所有公共函数需有文档注释 `/// ...`
- 异步代码统一使用 `tokio`，禁止 `std::thread::sleep`
- 错误处理以 `anyhow` + 明确上下文为主；对外错误码与事件载荷需保持协议一致

### Vue 前端
- TypeScript strict 模式，禁止 `any`
- 组件文件统一 PascalCase 命名
- 状态管理以 `src/store.ts` 为当前单一入口（如后续迁移 Pinia，需同步更新文档）

---

## 分支策略

| 分支 | 用途 |
|------|------|
| `main` | 当前默认集成分支 |
| `feat/*` | 新功能开发分支（合并到 `main`） |
| `fix/*` | Bug 修复分支（合并到 `main`） |

提交 PR 前请确保：
- [ ] `cargo test` 通过
- [ ] `cargo check` 通过
- [ ] PR 描述中说明改动内容与测试方式

---

## 模块负责人

| 模块 | 优先级 | 说明 |
|------|--------|------|
| `discovery/mdns` | 🔴 P0 | MVP 核心 |
| `transport/server` | 🔴 P0 | MVP 核心 |
| `transport/client` | 🔴 P0 | MVP 核心 |
| `crypto/identity` | 🔴 P0 | MVP 核心 |
| UI: DeviceCard | 🟡 P1 | 主界面 |
| UI: TransferModal | 🟡 P1 | 接收交互 |
| 右键菜单集成 | 🟢 P2 | v1 功能 |
| BLE 发现 | 🔵 P3 | v2 功能 |
