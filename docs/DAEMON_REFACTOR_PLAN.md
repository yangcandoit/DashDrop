# DashDrop Daemon Refactor Plan

## Why This Refactor Exists

`v0.2.0` stabilized the current single-process Tauri app, but the remaining roadmap items now depend on a different process model:

- background transfers after the UI closes
- stronger single-instance behavior
- system share/open handoff
- better platform integrations
- future BLE / P2P / fan-out work without duplicating UI lifecycle concerns

The current codebase still couples core runtime work to `tauri::AppHandle` in several places:

- database bootstrap and legacy state migration
- discovery event emission
- transport event emission
- app activation / foregrounding
- startup sequencing in `src-tauri/src/lib.rs`

That coupling is the main blocker. The next milestone is not "ship a daemon immediately"; it is "make the runtime separable without breaking the current app".

Current protocol baseline:

- the actual local IPC contract is now documented in [`docs/DAEMON_IPC_PROTOCOL.md`](/Users/young/Desktop/dashdrop/docs/DAEMON_IPC_PROTOCOL.md)
- that document is the implementation-derived source of truth for endpoint roles, command classes, envelopes, errors, retry behavior, and runtime event replay semantics

## Target Architecture

### Process Model

Phase target:

- `dashdrop-ui`
  - Tauri frontend shell
  - renders state
  - issues commands
  - shows dialogs / notifications
- `dashdropd`
  - discovery
  - transfer runtime
  - trust/config persistence
  - external share intake
  - long-lived background state
- local IPC
  - Unix domain socket on macOS/Linux
  - named pipe on Windows
  - shared wire contract for UI and secondary entry points

### Runtime Boundaries

The runtime must be split into three layers:

1. `runtime/bootstrap`
   - config-dir resolution
   - DB initialization
   - legacy-state migration
   - `AppState` construction

2. `runtime/host`
   - event emission
   - window activation/focus
   - future notification hooks
   - Tauri-backed today, daemon/headless-backed later

3. `runtime/subsystems`
   - transport listener
   - discovery register/browser/beacon
   - cleanup loops
   - notification loops

The UI process should depend on all three today. The future daemon process should depend on `bootstrap` and `subsystems`, and provide its own `runtime/host` implementation.

## Design Principles

- Keep the current single-process app working while extracting seams.
- Move from direct `AppHandle` usage to capability interfaces.
- Extract initialization before extracting execution.
- Preserve current local IPC contracts where possible.
- Do not move BLE / P2P / fan-out into the codebase until the daemon boundary is real.

## Current Blockers

### Blocker 1: Startup Is Hard-Wired Into `lib.rs`

Today `src-tauri/src/lib.rs` resolves config paths, loads identity, initializes DB, migrates legacy state, creates `AppState`, starts IPC, starts discovery, starts transport, and starts background loops.

This prevents:

- headless daemon entrypoints
- deterministic bootstrap tests
- reuse from non-Tauri binaries

### Blocker 2: Core Services Still Need a Concrete Tauri Host

`AppCoreService` uses the host for:

- `device_discovered` / `device_updated`
- `external_share_received`
- `system_error`
- foregrounding the main window
- some transfer terminal/error emissions

This means local IPC and app logic are conceptually reusable, but still operationally tied to Tauri.

### Blocker 3: Subsystems Emit Directly Into Tauri

Discovery and transport modules still emit events through `AppHandle`.

This prevents:

- daemon-side state publication
- alternate IPC-backed event fan-out
- testing without a Tauri runtime

## Migration Phases

### Phase 1: Extract Runtime Bootstrap

Goal:

- move state/bootstrap logic out of `lib.rs`
- make DB and legacy migration path-based instead of `AppHandle`-based

Deliverables:

- `runtime/bootstrap.rs`
- `db::init_db_at(&Path)`
- `persistence::load_state_at(&Path)`
- `lib.rs` consuming bootstrap helpers

Exit criteria:

- no startup regression
- `cargo test`, `cargo clippy`, `npm run build`, `npm run test:e2e`, `npm run test:tauri:smoke` remain green
- daemon-backed startup path is also covered by `npm run test:tauri:daemon-smoke`

### Phase 2: Introduce Host Abstraction

Goal:

- replace direct `AppHandle` storage in service-layer code with a runtime host interface

Deliverables:

- `runtime/host.rs`
- Tauri-backed host implementation
- `AppCoreService` consuming `RuntimeHost`

Exit criteria:

- app activation/share events work through host abstraction
- device update and system-error emissions no longer require storing raw `AppHandle`

Status on 2026-03-11:

- complete for `AppCoreService`
- current discovery/transport modules still use concrete Tauri handles internally

### Phase 3: Extract Runtime Supervisor

Goal:

- move subsystem startup into a reusable runtime supervisor

Deliverables:

- `runtime/supervisor.rs`
- transport/discovery/background-loop startup grouped behind one API

Exit criteria:

- Tauri entrypoint starts runtime through supervisor
- new headless binary can start the same supervisor

Status on 2026-03-11:

- partially complete
- `runtime/supervisor.rs` now owns local IPC startup, shared network runtime startup, plus maintenance loops
- Tauri startup uses the supervisor
- `dashdropd` headless binary also uses the supervisor
- discovery/transport now run against `RuntimeHost` instead of directly requiring `AppHandle`
- headless mode can start the shared network runtime, but full UI/event-subscription decoupling is still unfinished

### Phase 4: Add `dashdropd`

Goal:

- introduce a headless daemon binary using the same bootstrap + supervisor

Deliverables:

- new binary target
- daemon lifecycle docs
- local IPC handshake between UI and daemon

Exit criteria:

- daemon starts without Tauri
- UI can connect and fetch runtime status

### Phase 5: Move UI to Daemon Client Mode

Goal:

- UI no longer owns discovery/transfer execution

Deliverables:

- UI commands proxied via local IPC
- daemon-owned event fan-out back to UI
- stronger single-instance and share routing

Exit criteria:

- closing the UI does not kill active transfers
- second-instance handoff targets the daemon or active UI session

Status on 2026-03-12:

- partially complete
- selected read-side UI commands can already proxy via local IPC when the runtime's actual `control_plane_mode` is `daemon`
- current coverage now includes devices, trusted peers, pending incoming requests, app config, local identity, runtime status, discovery diagnostics, transfer metrics, active transfers, single transfer lookup, transfer history, and security events
- daemon runtime events are now journaled into an in-memory `runtime_event_feed` and exposed over local IPC for polling/replay
- runtime event delivery is still replay polling, not a push subscription stream; the current feed is in-memory and lossy across truncation/restart
- write-side proxying is also in place: core transfer actions plus trust/config mutations now route through local IPC in daemon mode
- daemon-mode UI close semantics are now shell-only: the main window hides instead of exiting, which preserves the daemon-owned runtime while the shell goes invisible
- local IPC is now split into two endpoint roles:
  - `service`: daemon control plane
  - `ui activation`: reveal/focus/share handoff for the visible shell
- endpoint ownership is now enforced by the local IPC server:
  - `app/activate` and `app/queue_external_share` belong to `ui activation`
  - read-side, write-side, and `app/get_event_feed` belong to `service`
- Tauri startup now supports `auto`, `daemon`, and `in_process` control-plane selection; in daemon-backed mode the UI shell no longer starts the in-process runtime supervisor
- packaged builds now prepare and bundle `dashdropd` as a Tauri `externalBin` sidecar, and runtime daemon lookup searches both executable and resource directories
- `get_control_plane_mode`, frontend event-subscription selection, and command proxy gating now treat `AppState.control_plane_mode` / `RuntimeStatus.control_plane_mode` as the source of truth; `requested_control_plane_mode` remains diagnostics-only
- the app store, History, Settings runtime cards, Security Events refresh path, and core mutation path now consume daemon-backed reads/writes/events
- daemon mode now explicitly keeps a narrow local shell-event path for `system_error`, `external_share_received`, and `app_window_revealed`, so second-instance handoff, external share intake, hide-on-close, and macOS reopen do not regress while runtime ownership stays with the daemon
- remaining gaps are now mostly bounded replay semantics plus future system-share/native integration work, not runtime ownership of discovery/transfer/trust/config or page-level subscription cleanup

## Event Model

Short term:

- keep existing frontend event names stable
- emit through `RuntimeHost`

Mid term:

- daemon owns canonical event stream
- UI subscribes over local IPC
- Tauri host becomes one consumer, not the producer of truth

Event classes:

- discovery events
- transfer progress / terminal events
- security events
- system/runtime error events
- app activation / share intake events

## Local IPC Implications

The existing local IPC contract is already the right direction. It should grow into:

- command request/response
- runtime event replay polling today, with room for future live subscription/fan-out
- activation/share handoff
- runtime status and diagnostics

This means future daemon work should extend the current wire contract, not replace it.

## What This Refactor Does Not Claim Yet

This plan does not mean the following are "nearly done":

- BLE assist
- P2P / SoftAP fallback
- 1:N fan-out
- full system share integrations on every desktop platform

Those features come after the daemon boundary exists. Building them earlier would create expensive rework.

## Immediate Implementation Plan

The next code changes should proceed in this order:

1. extract bootstrap and path-based persistence helpers
2. introduce `RuntimeHost` and migrate `AppCoreService`
3. extract subsystem supervisor from `lib.rs`
4. add headless daemon binary skeleton
5. convert UI to daemon-client mode incrementally

## Success Criteria For The Refactor Line

The refactor line is successful when:

- the current Tauri app still behaves like `v0.2.0+`
- runtime startup is reusable from a non-Tauri binary
- service-layer logic depends on host capabilities, not concrete Tauri ownership
- the codebase has a credible path to background runtime execution
