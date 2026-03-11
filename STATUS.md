# DashDrop Status

Last updated: 2026-03-11

## Overall

Project is stable on the current single-process Tauri architecture, with discovery/transfer/security contracts implemented and the A-E feature sequence integrated on `main`.

Estimated completion:
- Core product hardening (state contract + reliability): 95%+
- Release readiness (current gate scope): near-complete
- Release readiness (strict protocol compliance + long-run behavior): mostly complete
- AirDrop-like target architecture (daemon/system share/P2P-SoftAP): not started as a shipped feature line

## Completed

### Architecture and state contract
- Unified transfer event contract with explicit in-progress vs terminal events.
- Added `transfer_accepted` and wired sender/receiver/frontend flow.
- `revision` now increments only on state transitions (not on progress chunks).
- In-progress source of truth is backend memory (`AppState.transfers`).
- `TransferOutcome::Cancelled` split and aligned to `CancelledBySender` / `CancelledByReceiver`.

### Backend (Rust)
- DTO boundary introduced and adopted:
  - `DeviceView`
  - `SessionView`
  - `TransferView`
- Frontend no longer depends on unstable backend internals like `SocketAddr`/`Instant`.
- Terminal event emission unified via transport event helpers.
- `unpair_device` now returns explicit error when target does not exist.
- DB history parsing now rejects unknown `direction` values with explicit error (no silent fallback).
- Receiver directory-create failure now explicitly signals sender-side failure path.
- Sender ack-dispatch path has fail-fast + waiter cleanup behavior.
- Added `connect_by_address` command (handshake + fingerprint summary + state upsert).
- Added trusted-only auto accept (`auto_accept_trusted_only`, with legacy `auto_accept` read compatibility).
- Added security audit persistence (`security_events`) and read command (`get_security_events`).
- Added secure-store posture command (`get_security_posture`) and startup degraded-storage warning path.
- Added probe subsystem (`dashdrop-probe/1`) with discovery-driven probe update and server-side probe short-circuit.
- Reachability state now includes `Offline`, with 15s grace from `OfflineCandidate`.
- Discovery now includes UDP beacon fallback (`53318/udp`) alongside mDNS for constrained multicast networks.
- Sender now applies `Accept/Reject` wait timeout and emits `E_TIMEOUT` on offer-stage timeout.
- Transfer `reason_code` now uses protocol-style `E_*` mapping instead of Rust `Debug` names.
- Sender directory items now emit directory `Complete` and await receiver `Ack` (no silent directory bypass).
- Incoming offer rate limiting now includes fingerprint-level policy (trusted=60/min, untrusted=20/min).
- Incoming connection rate limiting now includes fingerprint-level windows in addition to IP windows.
- Probe close code now uses `0xD0` to match architecture/protocol docs.
- `set_app_config` mDNS rename re-register failure now emits user-visible `system_error` and rolls back config.
- `set_app_config` rollback error now carries structured `code/context` payload for precise frontend rendering.
- Trust/config write commands are now funneled through `AppCoreService`, reducing duplicate mutation paths between Tauri commands and local IPC dispatch.
- `device_name` is now hard-rejected when blank at the backend boundary (not only in frontend form validation).
- Local IPC Unix socket startup now binds through `std::os::unix::net::UnixListener` before handing off to Tokio, so Tauri `setup` no longer panics when no Tokio reactor is already running.
- Windows local IPC now has named pipe server/client baseline implementation, using the same framed wire protocol as Unix local IPC.
- Local IPC now also supports queueing external share paths into the running app (`app/queue_external_share`), which the frontend consumes as a Nearby send queue.
- Startup now attempts single-instance handoff via local IPC (`app/activate`): a second launch forwards activation/share paths to the running instance instead of standing up a parallel app session.
- Discovery updates no longer use production `unwrap()` on system time conversion.
- Startup config directory resolution is now strict and fails fast if path resolution is unavailable.
- macOS bundle now declares Local Network / Bonjour usage (`Info.plist`) for reliable discovery permission prompts.
- Runtime persistence now uses SQLite as the single store (`app_config_store`, `trusted_peers_store`, `transfers_history`, `security_events`).
- Legacy `state.json` is retained as read-only one-time migration input.
- Sender spawn error fallback now marks all non-terminal states (including `Draft`) as `Failed`.
- Pair/unpair now syncs `DeviceInfo.trusted` in memory and emits `device_updated`.
- Incoming handshake now emits `fingerprint_changed` (and audits it) when a trusted previous fp is replaced by cert fp on the same observed session context.
- `fingerprint_changed` now has dedupe/anti-noise behavior (session+fingerprint tuple cooldown) to avoid repeated warning floods.
- Sender/receiver/handshake key paths now include structured tracing fields (`transfer_id`, `peer_fp`, `phase`, `reason`).
- Discovery beacon cadence now adapts to power profile and exports diagnostics fields for current power mode and interval selection.
- Resume now validates persisted `source_snapshot(size/mtime/head_hash)` before reuse and logs `resume_source_changed` when it must restart from scratch.
- Incoming request notifications now expire cleanly, retract stale actions, and return `E_REQUEST_EXPIRED` on late accept/reject clicks.
- QUIC listener now prefers fixed `53319/udp`, falls back to a random port only when necessary, and exposes `listener_port_mode` plus `firewall_rule_state`.
- Transfer history/event payloads now support optional `batch_id` for grouped transfers without renaming any terminal events.

### Frontend (Vue/TS)
- Store event projection refactored for strict incoming/active-transfer handoff.
- IPC read-side array responses now use defensive fallback normalization to avoid null/undefined payload crashes.
- `transfer_accepted` processing now:
  1. removes from `incomingQueue`
  2. moves/updates active transfer to `Transferring`
  3. performs targeted snapshot fallback when needed
- Drag-and-drop changed to single-channel model (removed 500ms race hack).
- `sendingTo` behavior moved from local component ref to global transfer-lifecycle-derived state.
- Transfers page supports connect-by-address entry (with fingerprint confirmation prompt).
- Connect-by-address now supports confirm -> optional remember/pair flow (`pair_device`).
- Nearby first-send to untrusted device now requires explicit fingerprint confirmation before transfer.
- Nearby first-send confirmation no longer dismisses on send failure; users keep context and can retry after the error.
- Settings / Nearby / Transfers / incoming receive prompt now expose short verification codes, and untrusted send/receive/connect flows require explicit confirmation against a shared code visible on both devices.
- Nearby now consumes queued external share/file-open paths and lets the user pick a target device directly instead of re-selecting files manually.
- Transfers page supports active-task batch cancel and outgoing transfer retry.
- Transfers, History, and Security Events now surface user-visible action/load failures instead of relying on console logs only.
- PartialCompleted transfer retry now performs failed-file-only retry (uses stored `failed_file_ids` + source map).
- Incoming request total size is now human-formatted.
- History now auto-refreshes on transfer terminal events.
- History now supports local filters (peer keyword, direction, status, time window).
- Trusted Devices now shows `paired_at`, `last_used_at`, alias edit, and unpair confirmation.
- Settings includes trusted-only auto-accept toggle, file conflict strategy, stream-concurrency config, degraded secure-storage warning banner, and runtime status/metrics cards.
- Settings runtime metrics now include average duration and failure distribution (SQLite aggregated).
- `system_error` now supports structured handling (`code`) so rename rollback warnings are persistent until user dismisses.
- Error banner messaging now uses a unified actionable template (`summary + Next steps`).
- `identity_mismatch` warning now auto-clears; `fingerprint_changed` warning is now consumed and shown.
- mDNS startup failures (`MDNS_REGISTER_FAILED` / `MDNS_BROWSER_FAILED`) are now surfaced as persistent actionable errors.
- Added first-use onboarding modal (local persisted dismissal, test mode auto-skip).
- App shell now adapts better to narrow windows with top-nav and compact-grid navigation breakpoints instead of a permanently narrow side rail.

### CI and tests
- CI workflow exists and runs:
  - `cargo check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test`
  - `npm run build`
  - `npm run test:e2e`
- Added GitHub security workflows:
  - `.github/workflows/security-audit.yml` (`cargo audit` + `npm audit`)
  - GitHub Code Scanning default setup (避免与 advanced CodeQL workflow 冲突)
- Added dependency automation:
  - `.github/dependabot.yml` (GitHub Actions + npm + cargo weekly updates)
- Build installer workflow hardened:
  - normalized release asset names
  - per-platform smoke checks
  - optional macOS notarization and Windows code-signing hooks
  - unsigned macOS artifacts now get CI ad-hoc signing before DMG packaging
  - release artifacts now include `INSTALL_NOTES.txt` for unsigned macOS/Windows diagnostics
  - tag release uploads with `SHA256SUMS.txt`
- Added release/upgrade documentation templates:
  - `docs/RELEASE_NOTES_TEMPLATE.md`
  - `docs/UPGRADE_MIGRATION_TEMPLATE.md`
- Added platform network troubleshooting guide:
  - `docs/NETWORK_TROUBLESHOOTING.md`
- Rust unit/contract tests currently passing.
- Frontend script-level E2E contract test exists and passing.
- Frontend Playwright UI E2E exists and passing (`tests/playwright/transfer-ui.spec.ts`).
- Added Rust regression test covering local IPC server startup outside a Tokio runtime.
- Added Playwright case for `identity_mismatch` warning visibility.
- Added Playwright case for History filter behavior.
- Additional unit tests added for:
  - `db.direction` strict parse
  - trusted-only auto-accept branch
  - offline grace transition helper
  - error-code reason mapping (`E_*`) contract
  - 100+ multi-file stress regression with cancel/failure recovery (`sender::tests::stress_regression_multi_file_over_100_rounds`)
- Additional Playwright cases now include:
  - history peer/status filter behavior
  - transfer batch cancel action
  - transfer retry action
- Persistence has been unified to SQLite (`app_config_store` + `trusted_peers_store`); legacy `state.json` is read only for one-time migration.

### Build quality
- `cargo check` / `cargo test` / `npm run build` / `npm run test:e2e` pass.
- `cargo clippy --all-targets --all-features` passes with no warnings.

## AirDrop-like target status

- The design in `docs/AIRDROP_SEAMLESS_EXPERIENCE_DESIGN.md` is a target-state plan.
- The target-state doc has been consolidated with explicit constraints for:
  - fixed QUIC port preference + firewall bootstrap (`53319/udp` with fallback random),
  - notification expiry lifecycle (`E_REQUEST_EXPIRED`),
  - source snapshot validation before resume (`size/mtime/head_hash`),
  - SQLite progress write coalescing (batch flush + WAL single writer),
  - power-aware beacon policy and BLE rolling identifiers.
- `main` now already implements the first four items above inside the current single-process architecture; BLE assist / rolling identifiers remain target-state only.
- Daemon split, local IPC auth model, system share entry, BLE assist, and Wi-Fi direct link manager are not part of the current shipped architecture.
- Current production baseline remains: single-process app + mDNS/beacon + QUIC transfer + diagnostics.

## In progress / not fully complete

### Remaining release tails
- Full native-window Tauri runtime E2E orchestration is still pending; current non-mock coverage now includes a local IPC startup regression test plus a verified `npm run test:tauri:smoke` startup smoke on 2026-03-11.
- Backend新增 `src-tauri/tests/phase_d_contract.rs` 为跨模块合同集成测试；真实多进程/多主机 QUIC 编排压测仍可继续扩展。
- Code-signing / notarization currently remains optional hooks; production certificates and notarization credentials still need to be configured in repository secrets.
- First-contact trust model remains TOFU; UI now exposes both full fingerprint and a shared short verification code for out-of-band comparison, but QR / richer out-of-band cryptographic verification is still planned for v0.2.
- Linux GUI stack still includes GTK3/WebKit2GTK transitively via Tauri runtime; long-term mitigation is upgrading runtime dependency chain.
- AirDrop-like implementation phases are only partially started: local IPC groundwork, single-instance handoff, and external share intake now exist in the single-process baseline, but daemon split, system-native share integrations, BLE assist, P2P/SoftAP scheduler, and 1:N fan-out reader/writer split remain incomplete.

## Validation snapshot

Latest local validations (2026-03-11):
- `cargo check` passed
- `cargo test` passed
- `cargo clippy --all-targets --all-features -- -D warnings` passed
- `npm run build` passed
- `npm run test:e2e` passed
- `npm run test:e2e:contract` passed
- `npm run test:tauri:smoke` passed

## Notes

- This file is the canonical status summary.
- Keep this file updated whenever milestone-level behavior changes.
