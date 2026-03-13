# DashDrop Status

Last updated: 2026-03-13

## Overall

Project is stable on the current Tauri product baseline, with discovery/transfer/security contracts implemented and the A-E feature sequence integrated on `main`. Dev sessions still default to `in_process`; packaged builds now prefer a daemon-backed control plane.

Estimated completion:
- Core product hardening (state contract + reliability): 95%+
- Release readiness (current gate scope): Code-complete, but pending physical field verification
- Release readiness (strict protocol compliance + long-run behavior): Mostly complete, pending high-capacity stress tests
- AirDrop-like target architecture (daemon/system share/P2P-SoftAP): Core logic implemented, SoftAP/P2P requires real-world driver compatibility testing

## Shared entry-layer contract

- Frozen runtime shell event names: `external_share_received`, `pairing_link_received`, `app_navigation_requested`, `app_window_revealed`.
- Frozen UI activation payload shape: `app/activate` forwards `paths[]` plus `pairing_links[]` only; transfer execution remains a frontend choice after handoff.
- Frozen shell-attention payload shape: `pendingIncomingCount`, `activeTransferCount`, `recentFailureCount`, `notificationsDegraded`.
- Implemented, but pending field-verification: deep-link activation, share-path intake, tray routing, and notification fallback.

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
- Local IPC now also supports queueing external share paths into the running UI shell (`app/queue_external_share`), which the frontend consumes as a Nearby send queue.
- Startup now attempts UI activation handoff via local IPC (`app/activate`): when a compatible UI shell is already running, a later launch forwards activation/share paths to that shell; if only `dashdropd` is running, the new UI shell attaches to the daemon instead of forwarding.
- `app/activate` now forwards pairing deep links as well as share paths, and the running app also handles OS-delivered open-url events for `dashdrop://pair?...`. Receiving a pairing deep link now opens Settings and preloads the import-confirm dialog instead of requiring manual paste.
- Desktop deep-link registration now goes through `tauri-plugin-deep-link` with the `dashdrop` scheme declared in Tauri config. Linux and Windows debug builds also best-effort call `register_all()` during setup so local development matches packaged behavior more closely.
- Runtime delivery of desktop deep links is now bridged through `tauri-plugin-deep-link` as well: the plugin's open-url callback is translated into the existing `pairing_link_received` shell event instead of relying on a separate ad-hoc `RunEvent::Opened` branch.
- Runtime bootstrap has been extracted into `src-tauri/src/runtime/bootstrap.rs`, `AppCoreService` now depends on a runtime-host abstraction instead of storing a raw `AppHandle` directly, and async subsystem startup has been grouped under `src-tauri/src/runtime/supervisor.rs`.
- Added `dashdropd` headless daemon binary. It now starts local IPC, maintenance loops, and the shared discovery/transport network runtime through `runtime/supervisor.rs`; daemon mode now treats discovery/transfer/trust/config as daemon-owned runtime state, while the Tauri process stays focused on shell/client behavior.
- Added the second UI daemon-client read path. When `DASHDROP_CONTROL_PLANE_MODE=daemon` is set, selected Tauri commands now proxy read-side queries through local IPC instead of reading in-process state directly (`get_devices`, `get_trusted_peers`, `get_pending_incoming_requests`, `get_app_config`, `get_local_identity`, `get_runtime_status`, `get_transfers`, `get_transfer`, `get_transfer_history`, `get_security_events`).
- Added daemon-mode runtime event replay. Runtime hosts now append emitted events to `runtime_event_feed`, local IPC exposes `app/get_event_feed`, and the frontend store/History view can poll and replay daemon-owned events without relying on in-process Tauri listeners. Replay now uses a 1024-event in-memory hot window plus a checkpoint-aware SQLite-backed journal: it keeps a 10,000-event baseline window, can extend retention for recently active consumers up to a 100,000-event cap, and now compacts that persisted journal on segment boundaries rather than deleting arbitrary individual rows. The shared frontend daemon poller persists its checkpoint in daemon-side SQLite as well as local fallback storage so UI restarts can resume from the same replay position. This is still a bounded journal rather than a durable push subscription.
- Replay journal metadata is now first-class as well: SQLite tracks `runtime_event_segments` plus compaction watermark fields in `runtime_event_meta`, and diagnostics expose active/compacted segment counts, watermark seq/segment, and the timestamp of the last real compaction boundary.
- `app/get_event_feed` now returns a replay snapshot (`generation`, `oldest_available_seq`, `latest_available_seq`, `resync_required`) instead of only a bare event array, so the UI can detect feed truncation / daemon restart and trigger authoritative resync instead of trusting a stale incremental cursor.
- Replay snapshots now also expose lightweight explanatory hints (`replay_source`, `resync_reason`) so the UI/client layer can tell whether a batch came from the hot in-memory window, SQLite catch-up, or a forced resync path, instead of treating every replay reset as the same generic condition.
- The frontend store now consumes those replay hints as well: daemon replay resets still trigger an authoritative refresh, but warning banners can now distinguish daemon restart from cursor drift or persisted-journal catch-up issues when resync succeeds.
- Settings diagnostics now surface the latest replay path and most recent resync cause directly from daemon replay metrics, so operators can tell at a glance whether the last incremental read came from the hot window, SQLite catch-up, or a forced resync and why that resync happened.
- Replay checkpoint lifecycle is now daemon-managed as well: the shared UI poller renews its checkpoint lease even during idle periods, expired checkpoints are pruned automatically, discovery diagnostics expose replay request / catch-up / resync / heartbeat counters, and each logical consumer now carries lag / age / recovery-state diagnostics so operators can tell whether a consumer is up to date, still inside the hot window, can catch up from SQLite, or already needs a full resync.
- Replay diagnostics now also expose retention semantics more explicitly: Settings/discovery diagnostics include `oldest_recoverable_seq`, `retention_cutoff_reason`, `persisted_journal_health`, and per-checkpoint recovery safety/mode fields, so operators can distinguish safe incremental catch-up from retention truncation or generation-mismatch refresh paths.
- Transfer progress persistence is now explicitly coalesced and observable: progress stays in memory first, flushes to SQLite on a `3s` or `32MB` threshold, terminal states force an immediate flush, SQLite remains in WAL mode, and discovery diagnostics now expose the current flush policy plus queue/flush counters.
- Cross-VLAN/subnet boundary messaging is now explicit as well: discovery diagnostics call out that automatic discovery is not expected to cross VLAN/subnet boundaries by default and recommend Connect by Address, while Nearby empty-state UI now points users to Transfers for the manual host:port path instead of leaving them waiting on discovery indefinitely.
- Trusted peer persistence now carries trust levels and verification metadata. Signed pairing-link import can record `signed_link_verified`, optional shared-pair-code confirmation upgrades trust to `mutual_confirmed`, Trusted Devices shows those badges directly, and fingerprint-change warnings escalate when they break a previously mutual-confirmed relationship.
- Trust levels now have real freeze/thaw behavior too: fingerprint changes against previously verified peers freeze the old trust record in SQLite, Trusted Devices shows the freeze timestamp/reason, outgoing sends to frozen peers are blocked with a re-verification requirement, and explicit signed-link verification can thaw the relationship back into an active trust state.
- BLE target-state work now has a baseline capability model in diagnostics: Settings exposes whether BLE baseline is enabled, whether the current platform/runtime reports BLE/P2P/SoftAP capability, and whether the product is currently falling back to QR/short-code pairing instead of any BLE assist path.
- BLE baseline now also has a concrete local assist-capsule skeleton: the backend can mint a short-lived rolling identifier plus integrity tag for future BLE-assist advertisement work, and Settings can preview that capsule without exposing long-term fingerprint material in the payload itself.
- BLE baseline now has an accept path too: validated assist capsules can be ingested into a short-lived runtime observation cache, and discovery diagnostics surface the recent observed capsule set so future BLE scanner/runtime work can plug into an already-stable state/inspection layer.
- BLE baseline runtime now includes a provider/scanner adapter seam inside the supervisor: a noop provider starts by default, publishes provider/scanner/advertiser state into diagnostics, and owns periodic observation-cache maintenance so future platform-specific BLE implementations can swap in without changing the surrounding state contract.
- macOS now has the first platform-specific provider scaffold wired into that seam: `macos_native` is the default provider on macOS, performs a lightweight `system_profiler` Bluetooth hardware probe, and maps the result into runtime diagnostics as `hardware_ready_scaffold` / `hardware_unavailable` / `hardware_present_powered_off` without yet claiming to run real CoreBluetooth scanning.
- The macOS BLE scaffold now also has a bridge-snapshot contract for future native adapters: when `DASHDROP_BLE_MACOS_BRIDGE_FILE` is set, the provider can ingest a JSON snapshot carrying permission state, scanner/advertiser state, and observed assist capsules, and Settings now surfaces the bridge mode, source path, and last snapshot timestamp directly in diagnostics.
- macOS BLE baseline now also includes a real native helper shell: `src-tauri/macos/BleAssistBridge.swift` uses CoreBluetooth to observe adapter state, request permission through the normal OS flow, scan for future DashDrop BLE advertisements, and continuously write bridge snapshots that the Rust provider can consume in dev today and from a bundled helper later.
- Tauri packaging now prepares and bundles that macOS BLE helper alongside `dashdropd`, and bundle smoke asserts both sidecars are physically present inside `dashdrop.app`, so BLE baseline scaffolding is no longer a dev-only source artifact.
- BLE baseline now has the first real advertisement path too: the Rust runtime writes a rolling local capsule request into the config dir, the macOS helper reads that request, advertises a compact manufacturer payload via `CBPeripheralManager`, and also decodes the same compact payload when scanning nearby broadcasters.
- Windows BLE baseline now has a matching helper scaffold as well: `windows_native` is the default provider on Windows, can spawn a `dashdrop-ble-bridge` helper (or PowerShell source scaffold in dev), consumes a Windows bridge snapshot file, mirrors the rolling advertisement request path, and surfaces helper/advertisement state through the same diagnostics contract even though real WinRT scanning/advertising is still not implemented yet.
- Daemon mode now keeps a narrow local shell-event bridge for `system_error`, `external_share_received`, and `app_window_revealed`, so second-instance handoff, external share intake, hide-on-close, and macOS reopen still reach the frontend without giving the UI process runtime ownership back.
- Settings-side diagnostics/metrics queries are now daemon-proxyable as well (`get_discovery_diagnostics`, `get_transfer_metrics`), so the Settings runtime cards and copied diagnostics can stay daemon-backed.
- Added daemon-mode write-side command proxying. In daemon control-plane mode, trust/config mutations and core transfer actions (`send_files_cmd`, `connect_by_address`, `accept/reject`, `cancel/retry`, `pair/unpair`, `set_trusted_alias`, `set_app_config`) now route through local IPC before falling back to in-process execution. Automatic retry is now limited to read-side/event-replay requests; write-side calls are single-attempt to avoid duplicate mutations after ambiguous transport failures.
- Local IPC service commands now require a daemon-issued short-lived `access_token`. The Tauri UI caches the grant in memory only, refreshes it proactively, revokes the previous token during successful refresh, and retries once with a fresh grant when the daemon returns `unauthorized`.
- Added explicit `auth/revoke` support. The Tauri shell now best-effort revokes its cached daemon grant during real app exit instead of only relying on TTL expiry or refresh-time replacement.
- Unix local IPC now also rejects service requests from a different uid via peer credentials before dispatch. Windows named pipes now create the service endpoint with an owner-only DACL and also impersonate the connected client to compare its SID with the current user before dispatch.
- Trust/config mutations now also emit explicit runtime events (`trusted_peer_updated`, `app_config_updated`), so Trusted Devices and Settings can auto-refresh in both in-process and daemon-backed modes instead of relying on manual reloads.
- In daemon mode, closing the main window now hides it instead of exiting the UI shell; macOS `RunEvent::Reopen` restores the main window so the daemon-backed runtime can keep working without a visible UI.
- Local IPC endpoints are now split by role: a daemon/service endpoint for the control plane and a separate UI activation endpoint for window reveal and share handoff, so `dashdropd` and the Tauri shell no longer fight over the same socket / named pipe. Endpoint ownership is now enforced by the server instead of being a documentation-only convention.
- Added [`docs/DAEMON_IPC_PROTOCOL.md`](/Users/young/Desktop/dashdrop/docs/DAEMON_IPC_PROTOCOL.md) as the implementation-derived local IPC spec for daemon control plane, envelopes, errors, retries, and event replay.
- Tauri startup now supports `auto` / `daemon` / `in_process` control-plane selection. In `auto`, the UI first tries to attach to an existing daemon service and then best-effort spawns `dashdropd`; when daemon-backed, the UI shell no longer starts the in-process network runtime supervisor.
- Packaged builds now prepare and bundle `dashdropd` as a Tauri `externalBin` sidecar, and runtime daemon lookup searches both executable and resource directories.
- Added `npm run test:tauri:bundle-smoke`, which builds an app bundle and asserts that `dashdropd` is physically present inside the packaged output.
- Added `npm run test:tauri:daemon-smoke`, which starts a real `dashdropd` sidecar plus `tauri dev` in daemon mode and waits for both processes to stabilize under a shared config dir.
- Added release-tail repository scaffolding for repeatable validation and signing posture: `docs/RELEASE_VALIDATION_MATRIX.md`, `docs/RELEASE_VALIDATION_REPORT_TEMPLATE.md`, `docs/RELEASE_SIGNING_SECRETS.md`, and installer artifacts now include `SIGNING_STATUS.txt`.
- Startup policy is now explicit by profile: packaged builds prefer daemon-backed control plane by default, dev sessions default to in-process unless `DASHDROP_CONTROL_PLANE_MODE=daemon` is set.
- `RuntimeStatus` now exposes `requested_control_plane_mode` and `runtime_profile`, and headless `dashdropd` reports itself as `daemon` / `headless` instead of inheriting single-process defaults.
- Headless `dashdropd` now also has an optional orphan/self-cleanup policy: when `DASHDROP_HEADLESS_IDLE_EXIT_SECS` is set, the daemon tracks active local IPC grants plus non-terminal transfers, exposes idle blockers/deadline in `RuntimeStatus`, and exits automatically after that idle timeout instead of running forever with no active client or transfer work.
- `npm run test:tauri:smoke` now allocates free Vite/HMR ports dynamically, avoiding false failures when local ports `1420/1421` are already occupied.
- Runtime status/diagnostics now expose `control_plane_mode`, `daemon_status`, and `daemon_binary_path`, so daemon-backed vs fallback in-process behavior is visible from the app itself.
- Startup now emits explicit system errors when daemon-backed mode was expected but the UI fell back to in-process execution, so packaged-sidecar regressions are visible without opening Settings first.
- Control-plane truth is now sourced from runtime state instead of raw env reads: backend proxy routing and frontend runtime-event subscription selection use the actual `control_plane_mode`, while `requested_control_plane_mode` remains the startup intent/diagnostic field.
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
- Settings now exports a local pairing QR plus `dashdrop://pair?...` pairing URI, and also supports importing that pairing link back into the app via paste, QR image import, or direct camera scan to complete trust + alias setup. New pairing links are now signed with the local long-term identity key, import flow validates freshness / verification code / signed-link metadata before trust can be confirmed, and the import UI now derives a shared pair code from both device fingerprints so operators can do a real two-sided out-of-band check instead of relying on a one-way remote code only. QR scanning now falls back to `jsQR` when `BarcodeDetector` is unavailable, and pairing links are short-lived with an expiry of about 10 minutes.
- Pairing import is no longer Settings-only. Nearby and Trusted Devices now expose the same import/scan entry so users can trust a device from the send flow or from the trust-management view directly.
- Nearby now briefly promotes and highlights a device card after that device is newly paired, making the post-pair send path easier to follow.
- Camera-based pairing scan now renders a visible guide frame, detection overlay, and short success state so the user can see when the QR code has actually been acquired.
- Tauri shell now creates a real system tray icon with left-click reveal plus menu shortcuts into Nearby / Transfers / History / Trusted Devices / Security Events / Settings, and tray quit now flows through the normal app shutdown path instead of bypassing daemon grant cleanup.
- Settings now persists `launch_at_login`, and saving that toggle performs real per-user startup registration locally: macOS writes a LaunchAgent plist, Windows updates the HKCU `Run` key, and Linux writes an XDG autostart desktop entry. The UI also re-syncs that registration on startup so the OS-side state stays aligned with the saved preference.
- Incoming transfer notifications now degrade cleanly when OS notification permission is unavailable: DashDrop keeps the request visible in Transfers, raises a persistent in-app warning with a direct jump target, and updates the tray tooltip/title with the pending request count instead of silently timing out in the background.
- Tray shell attention now also tracks active transfers and recent transfer issues, so the tooltip/title can distinguish between pending incoming requests, in-flight transfers, and fresh failures that still need review.
- The tray menu itself now mirrors that attention summary with live, non-clickable status rows above the normal navigation actions, so background state is visible without opening the main window.
- The tray menu now also exposes a dynamic "Review …" action that jumps straight into Transfers when there are pending incoming requests, active transfers, or fresh issues to resolve.
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
- Settings runtime cards now auto-refresh on relevant discovery/terminal runtime events, and Security Events auto-refreshes on security-related events in both in-process and daemon modes.
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
- The daemonization refactor plan is now tracked in `docs/DAEMON_REFACTOR_PLAN.md`, and phases 1-5 are partially implemented in the current codebase.
- The target-state doc has been consolidated with explicit constraints for:
  - fixed QUIC port preference + firewall bootstrap (`53319/udp` with fallback random),
  - notification expiry lifecycle (`E_REQUEST_EXPIRED`),
  - source snapshot validation before resume (`size/mtime/head_hash`),
  - SQLite progress write coalescing (batch flush + WAL single writer),
  - power-aware beacon policy and BLE rolling identifiers.
- `main` now already implements the first four items above inside the current single-process architecture; BLE assist / rolling identifiers remain target-state only.
- Daemon split, local IPC auth model, system share entry, BLE assist, and Wi-Fi direct link manager are not part of the current shipped architecture, even though bootstrap/host extraction has begun.
- Current runtime baseline is profile-dependent: dev defaults to the single-process-compatible path, while packaged builds prefer `dashdropd` as the runtime owner and keep Tauri as the UI shell/client.

## In progress / not fully complete

### Remaining release tails
- Full native-window Tauri runtime E2E orchestration is still pending; current non-mock coverage now includes a local IPC startup regression test plus a verified `npm run test:tauri:smoke` startup smoke on 2026-03-11.
- `npm run test:tauri:daemon-smoke` is now available and locally verified on 2026-03-12, covering the daemon-backed UI shell startup path and guarding against regressions where the first UI shell could incorrectly self-exit as a second-instance handoff when only `dashdropd` was already running.
- Headless daemon smoke has been manually verified on 2026-03-11 via `cargo run --bin dashdropd` outside the sandbox; the daemon now reaches real QUIC/discovery runtime activity, while restricted sandbox runs still cannot exercise Unix socket bind smoke reliably.
- Backend新增 `src-tauri/tests/phase_d_contract.rs` 为跨模块合同集成测试；真实多进程/多主机 QUIC 编排压测仍可继续扩展。
- Code-signing / notarization currently remains optional hooks; production certificates and notarization credentials still need to be configured in repository secrets.
- First-contact trust model still remains partially TOFU; UI now exposes both full fingerprint and a shared short verification code for out-of-band comparison, and pairing links are signed + validated on import, but the broader v0.2 trust target still needs stronger mutual/out-of-band verification beyond a one-way signed link.
- Linux GUI stack still includes GTK3/WebKit2GTK transitively via Tauri runtime; long-term mitigation is upgrading runtime dependency chain.
- AirDrop-like implementation phases are only partially complete: local IPC groundwork, daemon-backed command/event paths, single-instance handoff, and external share intake now exist, but system-native share integrations, BLE assist, P2P/SoftAP scheduler, and 1:N fan-out reader/writer split remain incomplete.
- Daemon-backed reads/writes/event subscriptions are now the default path across the current app store and shipped views; the main remaining daemon/client gaps are no longer page-level subscription cleanup, but bounded replay semantics and future native/system integrations. Replay now has a wider SQLite-backed catch-up window and persisted frontend cursor resume, but the feed is still bounded rather than being an unbounded durable stream.
- Dev-mode daemon attach now also searches repo-local `src-tauri/binaries` and `src-tauri/target/{debug,release}` before declaring the daemon binary missing. If the daemon binary is still absent, dev startup will best-effort run a local `cargo build --bin dashdropd` once before falling back.
- The daemon event-feed retry path now keeps the same daemon subscription strategy across temporary feed outages because the UI no longer re-derives routing from the original requested mode/env.

## Validation snapshot

Latest local validations (2026-03-13):
- `cargo check` passed
- `cargo test` passed
- `cargo clippy --all-targets --all-features -- -D warnings` passed
- `npm run build` passed
- `npm run test:e2e` passed
- `npm run test:e2e:contract` passed
- `npm run test:tauri:smoke` passed
- `npm run test:tauri:daemon-smoke` passed
- `npm run test:tauri:bundle-smoke` passed

## Notes

- This file is the canonical status summary.
- Keep this file updated whenever milestone-level behavior changes.
