# DashDrop Daemon IPC Protocol

Updated: 2026-03-13

This document is the current local IPC contract for the daemon control plane. It is derived from the shipped implementation in:

- `src-tauri/src/local_ipc.rs`
- `src-tauri/src/daemon/client.rs`
- `src-tauri/src/daemon/server.rs`
- `src-tauri/src/core_service.rs`
- `src-tauri/src/commands.rs`

It describes the contract that exists today. It does not redesign the protocol.

## Scope

This document covers:

- local IPC transport and framing
- endpoint roles
- command classes and payload shapes
- request/response envelopes
- error behavior
- retry and timeout behavior
- runtime event replay semantics
- compatibility expectations

This document does not cover the QUIC file-transfer wire protocol. That remains in [`PROTOCOL.md`](/Users/young/Desktop/dashdrop/PROTOCOL.md).

## Transport and Framing

### Endpoint transport

The local IPC transport is:

- Unix domain sockets on macOS/Linux
- Windows named pipes on Windows

The endpoint name is derived from the resolved config directory. The implementation hashes the config-dir path and uses a 12-hex suffix, so separate config roots get separate IPC endpoints.

Current endpoint naming:

- service endpoint
  - Unix: `dashdrop-service-<hash>.sock`
  - Windows: `\\\\.\\pipe\\dashdrop-service-v1-<hash>`
- UI activation endpoint
  - Unix: `dashdrop-ui-<hash>.sock`
  - Windows: `\\\\.\\pipe\\dashdrop-ui-v1-<hash>`

### Framing

Each request and response is encoded as:

1. 4-byte big-endian frame length
2. CBOR message body

Current frame limit:

- `LOCAL_IPC_MAX_FRAME_LEN = 1 MiB`

Frames above this limit fail at codec level and do not become application requests.

### Protocol version

Current local IPC protocol version:

- `proto_version = 1`

Requests with any other version are rejected with `error.code = "proto_mismatch"`.

## Endpoint Roles

There are two endpoint kinds.

### 1. Service endpoint

Role:

- daemon control plane
- state reads
- state mutations
- runtime event replay polling

Commands accepted on this endpoint:

- `auth/issue`
- all read-side commands
- all write-side commands
- `app/get_event_feed`

This is the default endpoint returned by `resolve_local_ipc_endpoint(...)`.

### 2. UI activation endpoint

Role:

- reveal/focus the visible UI shell
- deliver activation/share handoff payloads into that UI shell

Commands accepted on this endpoint:

- `app/activate`
- `app/queue_external_share`

Current behavior:

- the daemon/service endpoint now rejects activation/share commands
- the UI activation endpoint rejects service/read/write/replay commands
- wrong-endpoint requests return `invalid_request`

Operational note:

- `dashdropd` starts the service endpoint
- the Tauri UI shell starts the UI activation endpoint when it is attached to daemon-backed control-plane mode
- if only the daemon is running, a new UI instance attaches to the daemon instead of using activation handoff

## Envelope Shape

### Request envelope

```json
{
  "proto_version": 1,
  "request_id": "req-123",
  "command": "config/get",
  "payload": {},
  "auth_context": {
    "access_token": "optional"
  }
}
```

Fields:

- `proto_version: u16`
- `request_id: string`
- `command: string`
- `payload?: object`
- `auth_context?: { access_token?: string }`

Current auth status:

- `auth_context.access_token` is now enforced on the service endpoint for:
  - read-side commands
  - write-side commands
  - `app/get_event_feed`
- `auth_context.access_token` is not required for:
  - `auth/issue`
  - `auth/revoke` (but it still consumes `auth_context.access_token` and returns `unauthorized` if the token is missing / expired / unknown)
  - UI activation endpoint commands (`app/activate`, `app/queue_external_share`)
- Unix service-endpoint requests also check peer credentials and reject callers whose uid does not match the current user
- Windows service-endpoint creation now applies an owner-only DACL (plus SYSTEM access) at pipe-creation time
- Windows service-endpoint requests now impersonate the connected named-pipe client and reject callers whose SID does not match the current user
- token grants are daemon-issued and memory-only; the Tauri UI keeps its cached grant in memory only
- when `auth/issue` is called with the current cached token in `auth_context.access_token`, the daemon issues a replacement grant and revokes the previous token
- `auth/revoke` explicitly invalidates the currently presented token; the Tauri UI calls it on real application exit as a best-effort shutdown cleanup step

Current grant shape:

```json
{
  "auth": {
    "access_token": "base64url-token",
    "expires_at_unix_ms": 1741700300000,
    "refresh_after_unix_ms": 1741700120000
  }
}
```

### Success response envelope

```json
{
  "proto_version": 1,
  "request_id": "req-123",
  "ok": true,
  "payload": {
    "config": {}
  }
}
```

Notes:

- `ok` is always `true`
- `payload` is omitted for pure ack responses

### Error response envelope

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

Fields:

- `error.code: string`
- `error.message: string`

### Legacy response compatibility

The client still accepts the older internal response envelope:

- `LocalEnvelope<LocalIpcResult>`

That compatibility exists on the client decode path only. New implementations should emit the frozen wire response shape above.

## Command Classes

### Auth bootstrap

These commands manage the short-lived daemon access grant lifecycle.

| Command | Request payload | Success payload |
|---|---|---|
| `auth/issue` | none | `{ "auth": { "access_token": string, "expires_at_unix_ms": u64, "refresh_after_unix_ms": u64 } }` |
| `auth/revoke` | none | ack |

### Read-side

These commands read daemon-owned state and do not intentionally mutate it.

| Command | Request payload | Success payload |
|---|---|---|
| `discover/list` | none | `{ "devices": DeviceView[] }` |
| `trust/list` | none | `{ "trusted_peers": TrustedPeerView[] }` |
| `config/get` | none | `{ "config": AppConfig }` |
| `transfer/list` | none | `{ "transfers": TransferView[] }` |
| `transfer/get` | `{ "transfer_id": string }` | `{ "transfer": TransferView \| null }` |
| `transfer/history` | `{ "limit": u32, "offset": u32 }` | `{ "history": TransferView[] }` |
| `transfer/pending_incoming` | none | `{ "requests": PendingIncomingRequestPayload[] }` |
| `app/get_local_identity` | none | `{ "identity": LocalIdentityView }` |
| `app/get_runtime_status` | none | `{ "runtime_status": RuntimeStatus }` |
| `app/get_discovery_diagnostics` | none | `{ "diagnostics": object }` |
| `app/get_event_checkpoint` | `{ "consumer_id": string }` | `{ "checkpoint": RuntimeEventCheckpoint \| null }` |
| `security/get_events` | `{ "limit": u32, "offset": u32 }` | `{ "events": SecurityEvent[] }` |
| `transfer/get_metrics` | none | `{ "metrics": TransferMetrics }` |
| `security/get_posture` | none | `{ "posture": object }` |

### Write-side

These commands may mutate daemon-owned state or trigger runtime actions.

| Command | Request payload | Success payload |
|---|---|---|
| `discover/connect_by_address` | `{ "address": string }` | `{ "result": ConnectByAddressResult }` |
| `trust/pair` | `{ "fingerprint": string }` | ack |
| `trust/unpair` | `{ "fingerprint": string }` | ack |
| `trust/set_alias` | `{ "fingerprint": string, "alias": string \| null }` | ack |
| `config/set` | `{ "config": AppConfig }` | ack |
| `app/set_event_checkpoint` | `{ "consumer_id": string, "generation": string, "seq": u64 }` | ack |
| `transfer/send` | `{ "peer_fingerprint": string, "paths": string[] }` | ack |
| `transfer/accept` | `{ "transfer_id": string, "notification_id": string }` | ack |
| `transfer/reject` | `{ "transfer_id": string, "notification_id": string }` | ack |
| `transfer/cancel` | `{ "transfer_id": string }` | ack |
| `transfer/cancel_all` | none | `{ "count": u32 }` |
| `transfer/retry` | `{ "transfer_id": string }` | ack |

Notes:

- `discover/connect_by_address` is classified as write-side because it performs a real connect/probe flow and upserts peer state
- `transfer/cancel_all` returns data, but it is still write-side

### Activation/share handoff

These commands belong to the UI activation endpoint.

| Command | Request payload | Success payload |
|---|---|---|
| `app/activate` | `{ "paths": string[], "pairing_links": string[] }` | ack |
| `app/queue_external_share` | `{ "paths": string[] }` | ack |

Current semantics:

- `app/activate`
  - reveals the main window
  - if `paths` is non-empty, also emits `external_share_received` with `source = "app_activate"`
  - if `pairing_links` is non-empty, also emits `pairing_link_received` with `source = "app_activate"`
  - complements OS-level `RunEvent::Opened` handling for direct `dashdrop://pair?...` launches into an already-running UI shell
  - desktop scheme registration itself is now provided by `tauri-plugin-deep-link` (`dashdrop` scheme in Tauri config), while this IPC path remains the single-instance handoff bridge
  - runtime deep-link delivery now also flows through `tauri-plugin-deep-link` and is re-emitted into the same `pairing_link_received` shell event path
- `app/queue_external_share`
  - requires at least one path
  - emits `external_share_received` with `source = "local_ipc"`
  - reveals the main window

### Runtime event replay

| Command | Request payload | Success payload |
|---|---|---|
| `app/get_event_feed` | `{ "after_seq": u64, "limit": u32 }` | `{ "events": RuntimeEventEnvelope[], "generation": string, "oldest_available_seq": u64 \| null, "latest_available_seq": u64, "resync_required": bool, "replay_source"?: string, "resync_reason"?: string }` |

This is replay polling, not a push subscription.

### Reserved but not implemented

Current reserved command names:

- `discover/diagnostics`

It is intentionally rejected with `invalid_request` to keep the name frozen without claiming implementation.

## Error Model

There are three layers of failure.

### 1. Transport / codec failures

These are local client/server failures, not application-level error envelopes:

- Unix socket / named pipe connect failures
- I/O errors while writing or reading a frame
- CBOR encode/decode failures
- frame too large
- client-side response validation failure
  - response `proto_version` mismatch
  - response `request_id` mismatch

These failures surface as local client errors, not as `LocalIpcError`.

### 2. Request-parse failures

If the server can read a frame but cannot parse a valid request envelope, it returns:

```json
{
  "proto_version": 1,
  "request_id": "decode-error",
  "ok": false,
  "error": {
    "code": "invalid_request",
    "message": "..."
  }
}
```

This happens before the server has a trustworthy caller-provided `request_id`.

### 3. Application errors

Current `LocalIpcError.code` values:

- `invalid_request`
  - unknown command
  - malformed or missing payload field
  - wrong endpoint kind
  - reserved-but-unimplemented command
- `proto_mismatch`
  - request `proto_version != 1`
- `unauthorized`
  - missing access token
  - unknown or expired access token
  - Unix peer uid does not match current user
- `dispatch_failed`
  - command parsed correctly, but the service operation failed

## Retry and Timeout Strategy

### Protocol-level behavior

The wire protocol has:

- no timeout field
- no idempotency token
- no server-generated retry hint

### Current client behavior

`LocalIpcClient` itself does not apply a general request timeout.

Windows-specific note:

- opening the named pipe already retries internally up to 80 attempts with 50 ms delay when the pipe is busy or not yet present
- worst-case pipe-open wait is about 4 seconds before the request write starts

### Current UI proxy behavior

The Tauri command proxy in `src-tauri/src/commands.rs` uses two policies:

- read-side and event replay:
  - retries on local IPC failure with delays `[0, 100, 250, 500]` ms
- write-side and activation/share handoff:
  - single attempt only

Reason for the split:

- read-side requests are safe to retry
- write-side requests are not guaranteed idempotent if the daemon executed the command but the response was lost

Auth-specific behavior:

- the UI caches a daemon access grant in memory only
- if the cached grant reaches `refresh_after_unix_ms`, the UI attempts to issue a fresh grant before the next auth-required request
- successful refresh revokes the previously presented token on the daemon side
- on real application exit, the Tauri shell best-effort calls `auth/revoke` for the currently cached token before process teardown
- if refresh fails but the cached grant is still before `expires_at_unix_ms`, the UI reuses the cached grant
- if an auth-required request returns `unauthorized`, the UI clears its cached grant, issues a fresh one once, and retries the same request once
- this unauthorized recovery happens before any mutation is dispatched successfully, so it does not change the write-side duplicate-execution guidance above

Guidance for future callers:

- safe to auto-retry: read-side, `app/get_event_feed`
- retry only with care: write-side, `app/activate`, `app/queue_external_share`

## Runtime Event Feed Semantics

### Shape

`app/get_event_feed` returns a feed snapshot:

```json
{
  "events": [
    {
      "seq": 42,
      "event": "trusted_peer_updated",
      "payload": {},
      "emitted_at_unix_ms": 1741700000000
    }
  ],
  "generation": "feed-epoch-id",
  "oldest_available_seq": 1,
  "latest_available_seq": 42,
  "resync_required": false
}
```

Each replayed event inside `events` is:

```json
{
  "seq": 42,
  "event": "trusted_peer_updated",
  "payload": {},
  "emitted_at_unix_ms": 1741700000000
}
```

Fields:

- `generation: string`
- `oldest_available_seq: u64 | null`
- `latest_available_seq: u64`
- `resync_required: bool`
- `seq: u64`
- `event: string`
- `payload: object | scalar | array | null`
- `emitted_at_unix_ms: u64`

### Ordering and replay

Current behavior:

- `seq` starts at `1` for a fresh process
- `seq` increments by 1 for each recorded runtime event
- `app/get_event_feed { after_seq, limit }` returns events where `seq > after_seq`
- results are returned in ascending `seq` order
- `limit = 0` returns an empty `events` array
- `generation` is stable for the current persisted replay journal
- `latest_available_seq` is the newest emitted seq in the current replay window
- `oldest_available_seq` is the earliest event still retained for cursor-based replay
- `resync_required = true` when `after_seq` is outside the retained/reasonable replay window
- `replay_source` is an optional explanatory hint (`memory_hot_window`, `persisted_catch_up`, `empty`, `resync_required`)
- `resync_reason` is an optional explanatory hint when `resync_required = true` (for example `cursor_before_oldest_available`, `cursor_after_latest_available`, `persisted_catch_up_empty`, `persisted_journal_unavailable`)

### Storage model

The event feed is a bounded replay journal:

- in-memory ring buffer for hot-path replay
- SQLite-backed persistence for cursor-based catch-up beyond the hot window
- SQLite compaction now happens on segment boundaries rather than by deleting arbitrary individual rows
- SQLite also tracks segment metadata (`runtime_event_segments`) plus compaction watermark fields in `runtime_event_meta`, so diagnostics can report both active and already-compacted segment ranges
- daemon-side replay checkpoints (`runtime_event_checkpoints`) for logical consumers such as the shared Tauri UI poller
- checkpoint retention and lifecycle queries are indexed by generation / updated time / seq so durable replay compaction does not degrade as checkpoint rows accumulate

Current retention:

- latest 1024 events in memory
- latest 10,000 events in SQLite as the baseline durable catch-up window
- recent checkpoints can pin older events and extend SQLite retention beyond the baseline window
- SQLite retention is still bounded; the daemon currently caps checkpoint-pinned retention at 100,000 events
- the persisted journal is currently segmented in fixed 1,000-event chunks, and compaction drops whole old segments once they are older than the retention watermark
- the daemon records the last compacted `seq` / `segment_id` watermark and last compaction timestamp from those segment records rather than inferring them from the requested retention cursor
- `after_seq = 0` remains a hot-window replay request; SQLite catch-up is used when a caller presents an existing cursor that has fallen behind the in-memory ring buffer but is still inside the retained persisted window
- the Tauri frontend shared poller now also persists its last seen `after_seq` plus `generation` into daemon-side SQLite via `app/set_event_checkpoint`, and keeps a local-storage copy as fallback, so UI restarts can resume cursor-based replay instead of always restarting from `0`

### Checkpoint lifecycle

Replay checkpoints are also bounded/managed state rather than permanent log records.

Current behavior:

- checkpoints are keyed by logical `consumer_id`
- `app/get_event_checkpoint` returns `null` when no checkpoint exists or when an expired checkpoint was pruned on read
- writing the same `generation + seq` again is treated as a checkpoint heartbeat / lease renewal, not only as cursor advancement
- `RuntimeEventCheckpoint` keeps the legacy required fields (`consumer_id`, `generation`, `seq`, `updated_at_unix_ms`) and may now also include backward-compatible optional metadata fields:
  - `created_at_unix_ms`
  - `last_read_at_unix_ms`
  - `lease_expires_at_unix_ms`
  - `revision`
  - `last_transition` (`created`, `advanced`, `heartbeat`, `rewound`, `generation_reset`)
  - `recovery_hint` (`up_to_date`, `hot_window`, `persisted_catch_up`, `resync_required`)
  - `current_oldest_available_seq`
  - `current_latest_available_seq`
  - `current_compaction_watermark_seq`
  - `current_compaction_watermark_segment_id`
- the daemon refreshes the optional recovery/window metadata when checkpoints are saved and when they are read back through `app/get_event_checkpoint`
- diagnostics/listing paths that enumerate checkpoints also refresh the optional recovery/window metadata before returning rows
- the shared Tauri UI poller currently renews its checkpoint roughly every 30 seconds even when no new events arrive, so an idle but healthy UI consumer does not age into `stale` simply because the event stream is quiet
- checkpoint rows older than 7 days are pruned automatically during checkpoint reads/saves and diagnostics snapshots
- diagnostics classify each checkpoint by:
  - `lifecycle_state`: `active`, `idle`, or `stale`
  - `recovery_state`: `up_to_date`, `hot_window`, `persisted_catch_up`, or `resync_required`
  - `lag_events` and `age_ms`
- retention pinning now only considers checkpoints that are still inside the current max-retained floor (`seq >= max_retained_from_seq - 1`) and also ignores checkpoints already classified as `resync_required`, so an unrecoverable old consumer does not mask a newer still-recoverable consumer

Operational guidance:

- `hot_window` means the consumer can still recover entirely from the in-memory ring buffer
- `persisted_catch_up` means the consumer has fallen out of RAM replay but can still recover from SQLite
- `resync_required` means the consumer is no longer inside the retained replay window or is pinned to an older `generation`
- `stale` means the checkpoint has not been refreshed recently even if it is not yet old enough to prune

### Loss semantics

The replay feed is lossy.

Loss can happen when:

- more than the currently retained SQLite replay window was emitted since the client last caught up
- a runtime action emitted a Tauri event but did not also record it into `runtime_event_feed`

Current protocol behavior on loss:

- there is no per-gap marker event
- `generation` acts as the replay-journal epoch id
- `resync_required` explicitly tells the client that incremental replay is no longer trustworthy
- the daemon can replay the latest persisted window after restart, but not events older than the retained window

Current client guidance:

- if `resync_required = true`, refresh authoritative snapshots before trusting incremental replay again
- if `generation` changes, treat it as daemon/feed restart and refresh authoritative snapshots
- after resync, restart polling from `after_seq = 0`

### Diagnostics and counters

`app/get_discovery_diagnostics` now includes a `runtime_event_replay` section with:

- replay window bounds (`memory_*`, `persisted_*`, `latest_seq`)
- persisted segment stats (`persisted_segment_size`, `persisted_segment_count`, oldest/latest segment ids)
- replay retention mode (`baseline`, `checkpoint_pinned`, `checkpoint_pinned_max_capped`)
- retention pin summary (`retention_pinned_checkpoint_count`, `oldest_retention_pinned_checkpoint_seq`)
- checkpoint policy thresholds
- aggregate checkpoint counts (`active`, `idle`, `stale`, `resync_required`)
- replay counters (`total_feed_requests`, `memory_feed_requests`, `persisted_catch_up_requests`, `persisted_catch_up_events_served`, `resync_required_requests`)
- latest replay-path hints (`last_replay_source`, `last_replay_source_at_unix_ms`, `last_resync_reason`)
- checkpoint lifecycle counters (`checkpoint_loads`, `checkpoint_saves`, `expired_checkpoint_misses`, `pruned_checkpoint_count`)
- checkpoint heartbeat counters (`checkpoint_heartbeats`, `last_checkpoint_heartbeat_at_unix_ms`)
- checkpoint transition counters (`checkpoint_creates`, `checkpoint_advances`, `checkpoint_rewinds`, `checkpoint_generation_resets`, `last_checkpoint_transition_at_unix_ms`)
- checkpoint metadata refresh counters (`checkpoint_metadata_refreshes`, `checkpoint_resync_transitions`, `last_checkpoint_metadata_refresh_at_unix_ms`)
- last-seen timestamps for persisted catch-up, resync-required responses, and checkpoint pruning

### Relationship to frontend runtime events

`app/get_event_feed` replays the daemon-owned runtime journal. It is meant to let the UI catch up when it cannot rely on direct in-process Tauri listeners.

It is not yet a bidirectional subscription stream.

## Compatibility Strategy

### Stable parts

The following are intended to be stable:

- endpoint split: service vs UI activation
- request/response envelope fields
- command names
- response top-level `payload` field names
- `request_id` echo semantics
- `RuntimeEventEnvelope` field names

### Additive changes

Safe additive changes:

- adding optional fields inside payload objects
- adding new commands on the service endpoint
- adding new event names to the runtime feed

Unsafe changes:

- renaming existing commands
- renaming existing payload keys
- changing endpoint ownership of existing commands
- changing `seq` ordering semantics
- removing legacy wire-response decode support before all callers migrate

### Current backward-compatibility hooks

- client accepts both the frozen wire response and the older legacy envelope response
- extra payload fields are ignored by current request parsers unless they replace required fields

### Forward-looking reserved areas

These exist in shape but are not fully complete yet:

- durable event replay beyond the bounded SQLite-backed 10,000-event catch-up window
