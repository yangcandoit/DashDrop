# DashDrop Phase A Interface Freeze

Updated: 2026-03-11  
Branch baseline: `codex/arch-baseline`

## Purpose

This document freezes the first daemon-boundary contract so parallel agents can work without renaming commands, payload envelopes, terminal events, or cross-layer field names.

## State Split

### Shipped in current code

1. Transfer terminal event names are already stable and emitted as:
   - `transfer_complete`
   - `transfer_partial`
   - `transfer_rejected`
   - `transfer_cancelled_by_sender`
   - `transfer_cancelled_by_receiver`
   - `transfer_failed`
2. `reason_code` is already emitted in protocol-style `E_*` form, including `E_REQUEST_EXPIRED`.
3. Optional `batch_id` is already present on transfer state and transfer event payloads.
4. Incoming request notification lifecycle already uses `notification_id` and preserves `E_REQUEST_EXPIRED` on stale actions.
5. Control-plane logic for discovery/trust/config/runtime/security can already be dispatched through `AppCoreService`.

### In progress on this baseline

1. Local IPC now has a frozen wire envelope shape:
   - request: `proto_version`, `request_id`, `command`, `payload`, `auth_context`
   - response: `proto_version`, `request_id`, `ok`, `payload|error`
2. Implemented control-plane command names are frozen even though the app still runs single-process.
3. Phase A reserved command names are frozen but intentionally return not-implemented in the current single-process baseline:
   - `discover/diagnostics`
   - `transfer/send`
   - `transfer/cancel`
   - `transfer/retry`

### Target-state only

1. Unix socket / Named Pipe daemon deployment.
2. Access-token issuance, TTL refresh, and revocation enforcement.
3. System share entry, notification action callback bridge, BLE assist, P2P/SoftAP scheduling.
4. Daemon-side 1:N fan-out scheduler.

## Doc / Code Mismatches Found

1. `docs/AIRDROP_SEAMLESS_EXPERIENCE_DESIGN.md` Phase A requires `command + payload` in the local IPC envelope; previous code encoded request commands as a tagged enum with snake_case variant names. This branch freezes the documented envelope shape.
2. The design doc lists `discover/diagnostics` and `transfer/*` as Phase A commands, but current code does not implement them behind the core service yet. Their names are now reserved to prevent drift before implementation.
3. `ARCHITECTURE.md` and `STATUS.md` correctly describe the shipped app as single-process, but the branch now introduces a stricter daemon-ready control-plane contract than the previously loose in-process abstraction.

## Frozen Interface Checklist

### Local IPC command names

Implemented and frozen:

1. `discover/list`
2. `trust/list`
3. `trust/pair`
4. `trust/unpair`
5. `trust/set_alias`
6. `config/get`
7. `config/set`
8. `app/get_local_identity`
9. `app/get_runtime_status`
10. `security/get_posture`

Reserved and frozen:

1. `discover/diagnostics`
2. `transfer/send`
3. `transfer/cancel`
4. `transfer/retry`

### Local IPC response shape

Success responses must stay:

```json
{
  "proto_version": 1,
  "request_id": "req-123",
  "ok": true,
  "payload": { }
}
```

Error responses must stay:

```json
{
  "proto_version": 1,
  "request_id": "req-123",
  "ok": false,
  "error": {
    "code": "invalid_request",
    "message": "..."
  }
}
```

Response payload keys that are frozen:

1. `devices`
2. `trusted_peers`
3. `config`
4. `identity`
5. `runtime_status`
6. `posture`

### Transfer terminal events

These names must not be renamed, merged, or overloaded:

1. `transfer_complete`
2. `transfer_partial`
3. `transfer_rejected`
4. `transfer_cancelled_by_sender`
5. `transfer_cancelled_by_receiver`
6. `transfer_failed`
7. `transfer_error`

### `reason_code` semantics

The following meanings are frozen:

1. `E_REJECTED_BY_PEER`: peer explicitly rejected the offer.
2. `E_CANCELLED_BY_SENDER`: sender initiated cancellation.
3. `E_CANCELLED_BY_RECEIVER`: receiver initiated cancellation.
4. `E_REQUEST_EXPIRED`: user acted on an expired notification or stale pending request.
5. `E_TIMEOUT`: offer-stage or transport-stage timeout.
6. `E_PROTOCOL`: sequencing / control-stream / framing failure, not a user decision.
7. `E_IDENTITY_MISMATCH`: selected/discovered fingerprint does not match peer certificate.

### No-drift field names

These exact field names are frozen across Rust DTOs, event payloads, and future IPC handlers:

1. `batch_id`
2. `notification_id`
3. `access_token`

Do not rename them to `batchId`, `notificationId`, `token`, or `authToken`.

## Multi-Agent File Boundaries

### Agent A: IPC / daemon boundary

Can edit:

1. `src-tauri/src/local_ipc.rs`
2. `src-tauri/src/core_service.rs`
3. `docs/PHASE_A_INTERFACE_FREEZE.md`
4. Small contract tests in `src-tauri/src/state.rs`

Must not edit:

1. `src-tauri/src/transport/events.rs`
2. `src-tauri/src/transport/protocol.rs`
3. Frontend event projection files

### Agent B: discovery / diagnostics

Can edit:

1. `src-tauri/src/discovery/*`
2. `src-tauri/src/commands.rs` only for diagnostics plumbing
3. `src-tauri/src/state.rs` only for discovery diagnostics fields

Must not edit:

1. `src-tauri/src/local_ipc.rs`
2. `src-tauri/src/core_service.rs`
3. Terminal transfer event names

### Agent C: transfer control plane

Can edit:

1. `src-tauri/src/commands.rs`
2. `src-tauri/src/transport/*`
3. `src-tauri/src/state.rs` for transfer metadata only

Must not edit:

1. Frozen local IPC command names
2. Frozen `reason_code` meanings
3. Frozen field names `batch_id` / `notification_id` / `access_token`

### Agent D: frontend / UX

Can edit:

1. `src/*`
2. Frontend tests
3. UI copy and visual states

Must not edit:

1. Rust terminal event names
2. Rust `reason_code` semantics
3. DTO field spelling for frozen names

## Recommended Merge Order

1. Merge `codex/arch-baseline` first so later branches inherit the frozen contract.
2. Merge discovery/diagnostics next because it adds a reserved command and backend diagnostics payload without disturbing transfer semantics.
3. Merge transfer control-plane work after that so `transfer/send|cancel|retry` can bind to the already frozen IPC names.
4. Merge frontend adaptations last so UI only targets one settled backend contract.

## Current Risks

1. The app is still single-process, so daemon-only guarantees in the design doc are not yet enforceable at runtime.
2. `access_token` is name-frozen but behavior is not implemented yet; agents must not assume TTL/refresh already exists.
3. `discover/diagnostics` and `transfer/*` names are frozen before implementation; agents must add behavior under these exact names instead of introducing alternates.
4. `commands.rs` remains a shared hotspot. Non-contract edits there should stay narrowly scoped to avoid merge churn.
