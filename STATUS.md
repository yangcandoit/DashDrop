# DashDrop Status

Last updated: 2026-03-09

## Overall

Project is stable on the Tauri-embedded architecture, with Phase A/B/C core contract work now implemented and validated locally.

Estimated completion:
- Core product hardening (state contract + reliability): 95%+
- Release readiness (current gate scope): complete
- Release readiness (strict protocol compliance + long-run behavior): mostly complete

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
- Sender now applies `Accept/Reject` wait timeout and emits `E_TIMEOUT` on offer-stage timeout.
- Transfer `reason_code` now uses protocol-style `E_*` mapping instead of Rust `Debug` names.
- Sender directory items now emit directory `Complete` and await receiver `Ack` (no silent directory bypass).
- Incoming offer rate limiting now includes fingerprint-level policy (trusted=10/min, untrusted=3/min).
- Probe close code now uses `0xD0` to match architecture/protocol docs.
- `set_app_config` mDNS rename re-register failure now emits user-visible `system_error` and rolls back config.
- `set_app_config` rollback error now carries structured `code/context` payload for precise frontend rendering.
- Runtime persistence now uses SQLite as the single store (`app_config_store`, `trusted_peers_store`, `transfers_history`, `security_events`).
- Legacy `state.json` is retained as read-only one-time migration input.
- Sender spawn error fallback now marks all non-terminal states (including `Draft`) as `Failed`.
- Pair/unpair now syncs `DeviceInfo.trusted` in memory and emits `device_updated`.
- Incoming handshake now emits `fingerprint_changed` (and audits it) when a trusted previous fp is replaced by cert fp on the same observed session context.
- `fingerprint_changed` now has dedupe/anti-noise behavior (session+fingerprint tuple cooldown) to avoid repeated warning floods.
- Sender/receiver/handshake key paths now include structured tracing fields (`transfer_id`, `peer_fp`, `phase`, `reason`).

### Frontend (Vue/TS)
- Store event projection refactored for strict incoming/active-transfer handoff.
- `transfer_accepted` processing now:
  1. removes from `incomingQueue`
  2. moves/updates active transfer to `Transferring`
  3. performs targeted snapshot fallback when needed
- Drag-and-drop changed to single-channel model (removed 500ms race hack).
- `sendingTo` behavior moved from local component ref to global transfer-lifecycle-derived state.
- Transfers page supports connect-by-address entry (with fingerprint confirmation prompt).
- Connect-by-address now supports confirm -> optional remember/pair flow (`pair_device`).
- Transfers page supports active-task batch cancel and outgoing transfer retry.
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
- Added first-use onboarding modal (local persisted dismissal, test mode auto-skip).

### CI and tests
- CI workflow exists and runs:
  - `cargo check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test`
  - `npm run build`
  - `npm run test:e2e`
- Added GitHub security workflows:
  - `.github/workflows/security-audit.yml` (`cargo audit` + `npm audit`)
  - `.github/workflows/codeql.yml` (JavaScript/TypeScript CodeQL scan)
- Added dependency automation:
  - `.github/dependabot.yml` (GitHub Actions + npm + cargo weekly updates)
- Build installer workflow hardened:
  - normalized release asset names
  - per-platform smoke checks
  - optional macOS notarization and Windows code-signing hooks
  - tag release uploads with `SHA256SUMS.txt`
- Added release/upgrade documentation templates:
  - `docs/RELEASE_NOTES_TEMPLATE.md`
  - `docs/UPGRADE_MIGRATION_TEMPLATE.md`
- Rust unit/contract tests currently passing.
- Frontend script-level E2E contract test exists and passing.
- Frontend Playwright UI E2E exists and passing (`tests/playwright/transfer-ui.spec.ts`).
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

## In progress / not fully complete

### Remaining release tails
- Playwright E2E currently uses injected mock IPC backend (real UI + mocked backend contract), not a full Tauri runtime orchestration test.
- Backend新增 `src-tauri/tests/phase_d_contract.rs` 为跨模块合同集成测试；真实多进程/多主机 QUIC 编排压测仍可继续扩展。
- Code-signing / notarization currently remains optional hooks; production certificates and notarization credentials still need to be configured in repository secrets.

## Validation snapshot

Latest local validations:
- `cargo check` passed
- `cargo test` passed
- `npm run build` passed
- `npm run test:e2e` passed
- `npm run test:e2e:contract` passed

## Notes

- This file is the canonical status summary.
- Keep this file updated whenever milestone-level behavior changes.
