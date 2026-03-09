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
```

---

## 代码规范

### Rust 后端
- 强制 `cargo clippy` 无 warning
- 所有公共函数需有文档注释 `/// ...`
- 异步代码统一使用 `tokio`，禁止 `std::thread::sleep`
- 错误处理使用 `thiserror` 定义自定义异常类型

### Vue 前端
- TypeScript strict 模式，禁止 `any`
- 组件文件统一 PascalCase 命名
- 状态管理使用 Pinia

---

## 分支策略

| 分支 | 用途 |
|------|------|
| `main` | 稳定版本，受保护 |
| `dev` | 日常开发集成 |
| `feat/*` | 新功能开发 |
| `fix/*` | Bug 修复 |

提交 PR 前请确保：
- [ ] `cargo test` 通过
- [ ] `cargo clippy` 无 warning
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
