import { invoke } from "@tauri-apps/api/core";
import { listen, type Event } from "@tauri-apps/api/event";
import type {
  DeviceView,
  TrustedPeer,
  TransferView,
  ConnectByAddressResult,
  LocalIdentity,
  BleAssistCapsule,
  TransferStartedPayload,
  TransferIncomingPayload,
  TransferAcceptedPayload,
  TransferProgressPayload,
  TransferCompletePayload,
  TransferPartialPayload,
  TransferRejectedPayload,
  TransferCancelledPayload,
  TransferFailedPayload,
  TransferErrorPayload,
  IdentityMismatchPayload,
  FingerprintChangedPayload,
  SystemErrorPayload,
  TrustedPeerUpdatedPayload,
  AppConfigUpdatedPayload,
  ExternalSharePayload,
  PairingLinkPayload,
  SecurityEvent,
  AppConfig,
  RuntimeStatus,
  DiscoveryDiagnostics,
  TransferMetrics,
  ControlPlaneMode,
  RuntimeEventCheckpoint,
  RuntimeEventEnvelope,
  RuntimeEventFeedSnapshot,
} from "./types.ts";
import type { PairingQrPayload } from "./security.ts";
import { splitRuntimeEventSources, type RuntimeEventName } from "./runtimeEventRouting.ts";

type DaemonRuntimeEventPollOptions = {
  afterSeq?: number;
  limit?: number;
  pollMs?: number;
};

type DaemonRuntimeEventSubscriber = (event: RuntimeEventEnvelope) => void;

const DEFAULT_DAEMON_EVENT_LIMIT = 100;
const DEFAULT_DAEMON_EVENT_POLL_MS = 800;
const SHARED_DAEMON_EVENT_CHECKPOINT_HEARTBEAT_MS = 30_000;
const DAEMON_EVENT_CURSOR_STORAGE_KEY = "dashdrop_daemon_event_cursor_v1";
const SHARED_DAEMON_EVENT_CONSUMER_ID = "shared_ui_poller";
const SET_SHELL_ATTENTION_STATE_COMMAND = "set_shell_attention_state";
const EXTERNAL_SHARE_RECEIVED_EVENT = "external_share_received";
const PAIRING_LINK_RECEIVED_EVENT = "pairing_link_received";

let sharedDaemonEventSubscribers = new Set<DaemonRuntimeEventSubscriber>();
let sharedDaemonEventAfterSeq = 0;
let sharedDaemonEventLoopRunning = false;
let sharedDaemonEventLoopStopRequested = false;
let sharedDaemonEventFailureCount = 0;
let sharedDaemonEventUnavailableNotified = false;
let sharedDaemonEventGeneration: string | null = null;
let sharedDaemonEventCheckpointPersistedAfterSeq = -1;
let sharedDaemonEventCheckpointPersistedGeneration: string | null | undefined = undefined;
let sharedDaemonEventCheckpointPersistedAtUnixMs = 0;

type MockListen = (event: string, cb: (payload: unknown) => void) => Promise<() => void> | (() => void);
type MockInvoke = (command: string, args?: Record<string, unknown>) => unknown | Promise<unknown>;

type DashdropTestMock = {
  invoke: MockInvoke;
  listen: MockListen;
};

declare global {
  interface Window {
    __DASHDROP_TEST_MOCK__?: DashdropTestMock;
  }
}

function getTestMock(): DashdropTestMock | undefined {
  if (typeof window === "undefined") return undefined;
  return window.__DASHDROP_TEST_MOCK__;
}

type PersistedDaemonEventCursor = {
  afterSeq: number;
  generation: string | null;
};

function readPersistedDaemonEventCursor(): PersistedDaemonEventCursor {
  if (typeof window === "undefined" || !window.localStorage) {
    return { afterSeq: 0, generation: null };
  }

  try {
    const raw = window.localStorage.getItem(DAEMON_EVENT_CURSOR_STORAGE_KEY);
    if (!raw) {
      return { afterSeq: 0, generation: null };
    }
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const afterSeq =
      typeof parsed.afterSeq === "number" && Number.isFinite(parsed.afterSeq) && parsed.afterSeq >= 0
        ? parsed.afterSeq
        : 0;
    const generation =
      typeof parsed.generation === "string" && parsed.generation.length > 0
        ? parsed.generation
        : null;
    return { afterSeq, generation };
  } catch (error) {
    console.warn("Failed to read persisted daemon event cursor:", error);
    return { afterSeq: 0, generation: null };
  }
}

function persistDaemonEventCursor(afterSeq: number, generation: string | null) {
  if (typeof window === "undefined" || !window.localStorage) {
    return;
  }

  try {
    window.localStorage.setItem(
      DAEMON_EVENT_CURSOR_STORAGE_KEY,
      JSON.stringify({
        afterSeq: Math.max(0, Math.trunc(afterSeq)),
        generation: generation && generation.length > 0 ? generation : null,
      }),
    );
  } catch (error) {
    console.warn("Failed to persist daemon event cursor:", error);
  }
}

function asArray<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

async function invokeCommand<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const mock = getTestMock();
  if (mock?.invoke) {
    return (await mock.invoke(command, args)) as T;
  }
  return invoke<T>(command, args);
}

function asControlPlaneMode(value: unknown): ControlPlaneMode {
  return value === "daemon" ? "daemon" : "in_process";
}

async function listenEvent<T>(event: string, callback: (payload: T) => void): Promise<() => void> {
  const mock = getTestMock();
  if (mock?.listen) {
    const unlisten = await mock.listen(event, (payload) => callback(payload as T));
    return typeof unlisten === "function" ? unlisten : () => {};
  }
  return listen(event, (e: Event<T>) => callback(e.payload));
}

export async function getDevices(): Promise<DeviceView[]> {
  return asArray<DeviceView>(await invokeCommand<unknown>("get_devices"));
}

export async function getTrustedPeers(): Promise<TrustedPeer[]> {
  return asArray<TrustedPeer>(await invokeCommand<unknown>("get_trusted_peers"));
}

export async function pairDevice(fp: string): Promise<void> {
  return invokeCommand("pair_device", { fp });
}

export async function unpairDevice(fp: string): Promise<void> {
  return invokeCommand("unpair_device", { fp });
}

export async function setTrustedAlias(fp: string, alias: string | null): Promise<void> {
  return invokeCommand("set_trusted_alias", { fp, alias });
}

export async function confirmTrustedPeerVerification(
  fp: string,
  verificationMethod:
    | "manual_pairing"
    | "legacy_unsigned_link"
    | "signed_pairing_link"
    | "mutual_receipt",
  mutualConfirmation = false,
): Promise<void> {
  return invokeCommand("confirm_trusted_peer_verification", {
    fp,
    verificationMethod,
    mutualConfirmation,
  });
}

export async function sendFiles(peerFp: string, paths: string[]): Promise<void> {
  return invokeCommand("send_files_cmd", { peerFp, paths });
}

export async function connectByAddress(address: string): Promise<ConnectByAddressResult> {
  return invokeCommand<ConnectByAddressResult>("connect_by_address", { address });
}

export async function acceptTransfer(transferId: string, notificationId: string): Promise<void> {
  return invokeCommand("accept_transfer", { transferId, notificationId });
}

export async function acceptAndPairTransfer(
  transferId: string,
  notificationId: string,
  senderFp: string,
): Promise<void> {
  return invokeCommand("accept_and_pair_transfer", { transferId, notificationId, senderFp });
}

export async function rejectTransfer(transferId: string, notificationId: string): Promise<void> {
  return invokeCommand("reject_transfer", { transferId, notificationId });
}

export async function cancelTransfer(transferId: string): Promise<void> {
  return invokeCommand("cancel_transfer", { transferId });
}

export async function cancelAllTransfers(): Promise<number> {
  return invokeCommand<number>("cancel_all_transfers");
}

export async function retryTransfer(transferId: string): Promise<void> {
  return invokeCommand("retry_transfer", { transferId });
}

export async function openTransferFolder(transferId: string): Promise<void> {
  return invokeCommand("open_transfer_folder", { transferId });
}

export async function getAppConfig(): Promise<AppConfig> {
  const config = await invokeCommand<Partial<AppConfig>>("get_app_config");
  return {
    device_name: config.device_name ?? "",
    auto_accept_trusted_only: Boolean(config.auto_accept_trusted_only),
    download_dir: config.download_dir ?? null,
    file_conflict_strategy: config.file_conflict_strategy ?? "rename",
    max_parallel_streams: config.max_parallel_streams ?? 4,
    launch_at_login: Boolean(config.launch_at_login),
  };
}

export async function setAppConfig(config: AppConfig): Promise<void> {
  return invokeCommand("set_app_config", { config });
}

export async function getLocalIdentity(): Promise<LocalIdentity> {
  return invokeCommand<LocalIdentity>("get_local_identity");
}

export async function getLocalBleAssistCapsule(): Promise<BleAssistCapsule> {
  return invokeCommand<BleAssistCapsule>("get_local_ble_assist_capsule");
}

export async function ingestBleAssistCapsule(
  capsule: BleAssistCapsule,
  source?: string | null,
): Promise<void> {
  return invokeCommand("ingest_ble_assist_capsule", { capsule, source });
}

export async function getLocalPairingLink(): Promise<string> {
  return invokeCommand<string>("get_local_pairing_link");
}

export async function validatePairingInput(input: string): Promise<PairingQrPayload> {
  return invokeCommand<PairingQrPayload>("validate_pairing_input", { input });
}

export async function getTransfers(): Promise<TransferView[]> {
  return asArray<TransferView>(await invokeCommand<unknown>("get_transfers"));
}

export async function getPendingIncomingRequests(): Promise<TransferIncomingPayload[]> {
  return asArray<TransferIncomingPayload>(
    await invokeCommand<unknown>("get_pending_incoming_requests"),
  );
}

export async function getControlPlaneMode(): Promise<ControlPlaneMode> {
  // This command is expected to reflect the currently running control plane,
  // not the originally requested env mode.
  return asControlPlaneMode(await invokeCommand<unknown>("get_control_plane_mode"));
}

export async function getTransfer(transferId: string): Promise<TransferView | null> {
  return invokeCommand<TransferView | null>("get_transfer", { transferId });
}

export async function getTransferHistory(limit = 50, offset = 0): Promise<TransferView[]> {
  return asArray<TransferView>(
    await invokeCommand<unknown>("get_transfer_history", { limit, offset }),
  );
}

export async function getSecurityPosture(): Promise<{ secure_store_available: boolean }> {
  return invokeCommand("get_security_posture");
}

export async function getSecurityEvents(limit = 50, offset = 0): Promise<SecurityEvent[]> {
  return asArray<SecurityEvent>(
    await invokeCommand<unknown>("get_security_events", { limit, offset }),
  );
}

export async function getRuntimeStatus(): Promise<RuntimeStatus> {
  return invokeCommand("get_runtime_status");
}

export async function getRuntimeEvents(
  afterSeq = 0,
  limit = 100,
): Promise<RuntimeEventFeedSnapshot> {
  return normalizeRuntimeEventFeed(
    await invokeCommand<unknown>("get_runtime_events", { afterSeq, limit }),
  );
}

function normalizeRuntimeEventFeed(value: unknown): RuntimeEventFeedSnapshot {
  if (Array.isArray(value)) {
    const events = value as RuntimeEventEnvelope[];
    return {
      events,
      generation: "legacy",
      oldest_available_seq: events[0]?.seq ?? null,
      latest_available_seq: events[events.length - 1]?.seq ?? 0,
      resync_required: false,
    };
  }

  const record = value && typeof value === "object" ? (value as Record<string, unknown>) : {};
  const events = asArray<RuntimeEventEnvelope>(record.events);
  const oldestRaw = record.oldest_available_seq;
  const latestRaw = record.latest_available_seq;
  const latest =
    typeof latestRaw === "number" && Number.isFinite(latestRaw) ? latestRaw : events[events.length - 1]?.seq ?? 0;
  return {
    events,
    generation: typeof record.generation === "string" && record.generation.length > 0 ? record.generation : "legacy",
    oldest_available_seq:
      typeof oldestRaw === "number" && Number.isFinite(oldestRaw) ? oldestRaw : events[0]?.seq ?? null,
    latest_available_seq: latest,
    resync_required: Boolean(record.resync_required),
    replay_source:
      typeof record.replay_source === "string" && record.replay_source.length > 0
        ? (record.replay_source as RuntimeEventFeedSnapshot["replay_source"])
        : undefined,
    resync_reason:
      typeof record.resync_reason === "string" && record.resync_reason.length > 0
        ? (record.resync_reason as RuntimeEventFeedSnapshot["resync_reason"])
        : undefined,
  };
}

export async function getRuntimeEventCheckpoint(
  consumerId: string,
): Promise<RuntimeEventCheckpoint | null> {
  const value = await invokeCommand<unknown>("get_runtime_event_checkpoint", {
    consumerId,
  });
  if (!value || typeof value !== "object") {
    return null;
  }
  const record = value as Record<string, unknown>;
  const consumer_id =
    typeof record.consumer_id === "string" && record.consumer_id.length > 0
      ? record.consumer_id
      : consumerId;
  const generation =
    typeof record.generation === "string" && record.generation.length > 0
      ? record.generation
      : "legacy";
  const seq =
    typeof record.seq === "number" && Number.isFinite(record.seq) && record.seq >= 0
      ? record.seq
      : 0;
  const updated_at_unix_ms =
    typeof record.updated_at_unix_ms === "number" &&
    Number.isFinite(record.updated_at_unix_ms) &&
    record.updated_at_unix_ms >= 0
      ? record.updated_at_unix_ms
      : 0;
  return { consumer_id, generation, seq, updated_at_unix_ms };
}

export async function setRuntimeEventCheckpoint(
  consumerId: string,
  generation: string,
  seq: number,
): Promise<void> {
  await invokeCommand("set_runtime_event_checkpoint", {
    consumerId,
    generation,
    seq: Math.max(0, Math.trunc(seq)),
  });
}

function buildDaemonEventFeedResyncNotice(feed: RuntimeEventFeedSnapshot, reason: string): RuntimeEventEnvelope {
  return {
    seq: 0,
    event: "daemon_event_feed_resync_required",
    payload: {
      source: "daemon_event_feed",
      reason: feed.resync_reason ?? reason,
      generation: feed.generation,
      oldest_available_seq: feed.oldest_available_seq ?? null,
      latest_available_seq: feed.latest_available_seq,
      replay_source: feed.replay_source ?? "resync_required",
    },
    emitted_at_unix_ms: Date.now(),
  };
}

export async function copyTextToClipboard(text: string): Promise<void> {
  return invokeCommand("copy_to_clipboard", { text });
}

export async function setShellAttentionState(
  pendingIncomingCount: number,
  activeTransferCount: number,
  recentFailureCount: number,
  notificationsDegraded: boolean,
): Promise<void> {
  // Shared tray-shell contract: frontend computes these four aggregates and the
  // backend only mirrors them into the native tray/menu surface.
  return invokeCommand(SET_SHELL_ATTENTION_STATE_COMMAND, {
    pendingIncomingCount: Math.max(0, Math.trunc(pendingIncomingCount)),
    activeTransferCount: Math.max(0, Math.trunc(activeTransferCount)),
    recentFailureCount: Math.max(0, Math.trunc(recentFailureCount)),
    notificationsDegraded,
  });
}

export async function getDiscoveryDiagnostics(): Promise<DiscoveryDiagnostics> {
  return invokeCommand("get_discovery_diagnostics");
}

export async function getTransferMetrics(): Promise<TransferMetrics> {
  const metrics = await invokeCommand<Partial<TransferMetrics>>("get_transfer_metrics");
  return {
    completed: metrics.completed ?? 0,
    partial: metrics.partial ?? 0,
    failed: metrics.failed ?? 0,
    cancelled_by_sender: metrics.cancelled_by_sender ?? 0,
    cancelled_by_receiver: metrics.cancelled_by_receiver ?? 0,
    rejected: metrics.rejected ?? 0,
    bytes_sent: metrics.bytes_sent ?? 0,
    bytes_received: metrics.bytes_received ?? 0,
    average_duration_ms: metrics.average_duration_ms ?? 0,
    failure_distribution: metrics.failure_distribution ?? {},
  };
}

export function onDeviceDiscovered(callback: (device: DeviceView) => void) {
  return listenEvent("device_discovered", callback);
}

export function onDeviceUpdated(callback: (device: DeviceView) => void) {
  return listenEvent("device_updated", callback);
}

export function onDeviceLost(callback: (fp: string) => void) {
  return listenEvent<{ fingerprint: string }>("device_lost", (payload) => callback(payload.fingerprint));
}

export function onTransferStarted(callback: (payload: TransferStartedPayload) => void) {
  return listenEvent("transfer_started", callback);
}

export function onTransferIncoming(callback: (payload: TransferIncomingPayload) => void) {
  return listenEvent("transfer_incoming", callback);
}

export function onTransferAccepted(callback: (payload: TransferAcceptedPayload) => void) {
  return listenEvent("transfer_accepted", callback);
}

export function onTransferProgress(callback: (payload: TransferProgressPayload) => void) {
  return listenEvent("transfer_progress", callback);
}

export function onTransferComplete(callback: (payload: TransferCompletePayload) => void) {
  return listenEvent("transfer_complete", callback);
}

export function onTransferPartial(callback: (payload: TransferPartialPayload) => void) {
  return listenEvent("transfer_partial", callback);
}

export function onTransferRejected(callback: (payload: TransferRejectedPayload) => void) {
  return listenEvent("transfer_rejected", callback);
}

export function onTransferCancelledBySender(callback: (payload: TransferCancelledPayload) => void) {
  return listenEvent("transfer_cancelled_by_sender", callback);
}

export function onTransferCancelledByReceiver(callback: (payload: TransferCancelledPayload) => void) {
  return listenEvent("transfer_cancelled_by_receiver", callback);
}

export function onTransferFailed(callback: (payload: TransferFailedPayload) => void) {
  return listenEvent("transfer_failed", callback);
}

export function onTransferError(callback: (payload: TransferErrorPayload) => void) {
  return listenEvent("transfer_error", callback);
}

export function onSystemError(callback: (payload: SystemErrorPayload) => void) {
  return listenEvent("system_error", callback);
}

export function onTrustedPeerUpdated(callback: (payload: TrustedPeerUpdatedPayload) => void) {
  return listenEvent("trusted_peer_updated", callback);
}

export function onAppConfigUpdated(callback: (payload: AppConfigUpdatedPayload) => void) {
  return listenEvent("app_config_updated", callback);
}

export function onIdentityMismatch(callback: (payload: IdentityMismatchPayload) => void) {
  return listenEvent("identity_mismatch", callback);
}

export function onFingerprintChanged(callback: (payload: FingerprintChangedPayload) => void) {
  return listenEvent("fingerprint_changed", callback);
}

export function onExternalShareReceived(callback: (payload: ExternalSharePayload) => void) {
  return listenEvent(EXTERNAL_SHARE_RECEIVED_EVENT, callback);
}

export function onPairingLinkReceived(callback: (payload: PairingLinkPayload) => void) {
  return listenEvent(PAIRING_LINK_RECEIVED_EVENT, callback);
}

export async function subscribeDaemonRuntimeEvents(
  callback: (event: RuntimeEventEnvelope) => void,
  options?: DaemonRuntimeEventPollOptions,
): Promise<() => void> {
  const hasCustomOptions =
    options?.afterSeq !== undefined ||
    options?.limit !== undefined ||
    options?.pollMs !== undefined;

  if (!hasCustomOptions) {
    sharedDaemonEventSubscribers.add(callback);
    if (sharedDaemonEventLoopRunning) {
      // A new subscriber may appear while the previous unsubscribe already
      // requested loop shutdown. Clear that stop request so the shared poller
      // can keep serving the replacement subscriber instead of exiting after
      // the current poll cycle.
      sharedDaemonEventLoopStopRequested = false;
    }
    void ensureSharedDaemonRuntimeEventLoop();
    return () => {
      sharedDaemonEventSubscribers.delete(callback);
      if (sharedDaemonEventSubscribers.size === 0) {
        sharedDaemonEventLoopStopRequested = true;
      }
    };
  }

  let stopped = false;
  let afterSeq = Math.max(0, options?.afterSeq ?? 0);
  let generation: string | null = null;
  const limit = Math.max(1, options?.limit ?? 100);
  const pollMs = Math.max(250, options?.pollMs ?? 800);

  const loop = async () => {
    while (!stopped) {
      try {
        const feed = await getRuntimeEvents(afterSeq, limit);
        const generationChanged = generation !== null && feed.generation !== generation;
        generation = feed.generation;
        if (feed.resync_required || generationChanged) {
          afterSeq = Math.max(0, feed.latest_available_seq);
          callback(
            buildDaemonEventFeedResyncNotice(
              feed,
              generationChanged ? "generation_changed" : "cursor_invalid",
            ),
          );
        } else {
          for (const event of feed.events) {
            afterSeq = Math.max(afterSeq, event.seq);
            callback(event);
          }
        }
      } catch (error) {
        if (!stopped) {
          console.error("Failed to poll daemon runtime events:", error);
        }
      }
      if (stopped) {
        break;
      }
      await new Promise((resolve) => window.setTimeout(resolve, pollMs));
    }
  };

  void loop();
  return () => {
    stopped = true;
  };
}

async function loadSharedDaemonEventCursor(): Promise<void> {
  if (sharedDaemonEventAfterSeq > 0 || sharedDaemonEventGeneration !== null) {
    return;
  }

  const localCursor = readPersistedDaemonEventCursor();
  let resolvedAfterSeq = localCursor.afterSeq;
  let resolvedGeneration = localCursor.generation;

  try {
    const checkpoint = await getRuntimeEventCheckpoint(SHARED_DAEMON_EVENT_CONSUMER_ID);
    if (checkpoint) {
      if (resolvedGeneration === checkpoint.generation) {
        resolvedAfterSeq = Math.max(resolvedAfterSeq, checkpoint.seq);
        resolvedGeneration = checkpoint.generation;
      } else {
        resolvedAfterSeq = checkpoint.seq;
        resolvedGeneration = checkpoint.generation;
      }
    }
  } catch (error) {
    console.warn("Failed to load backend daemon event checkpoint:", error);
  }

  sharedDaemonEventAfterSeq = resolvedAfterSeq;
  sharedDaemonEventGeneration = resolvedGeneration;
  persistDaemonEventCursor(sharedDaemonEventAfterSeq, sharedDaemonEventGeneration);
  sharedDaemonEventCheckpointPersistedAfterSeq = sharedDaemonEventAfterSeq;
  sharedDaemonEventCheckpointPersistedGeneration = sharedDaemonEventGeneration;
  sharedDaemonEventCheckpointPersistedAtUnixMs = 0;
}

async function persistSharedDaemonEventCheckpoint(): Promise<void> {
  persistDaemonEventCursor(sharedDaemonEventAfterSeq, sharedDaemonEventGeneration);
  const now = Date.now();
  const heartbeatDue =
    sharedDaemonEventCheckpointPersistedAtUnixMs <= 0 ||
    now - sharedDaemonEventCheckpointPersistedAtUnixMs >= SHARED_DAEMON_EVENT_CHECKPOINT_HEARTBEAT_MS;

  if (
    sharedDaemonEventGeneration === null ||
    (sharedDaemonEventCheckpointPersistedAfterSeq === sharedDaemonEventAfterSeq &&
      sharedDaemonEventCheckpointPersistedGeneration === sharedDaemonEventGeneration &&
      !heartbeatDue)
  ) {
    return;
  }

  try {
    await setRuntimeEventCheckpoint(
      SHARED_DAEMON_EVENT_CONSUMER_ID,
      sharedDaemonEventGeneration,
      sharedDaemonEventAfterSeq,
    );
    sharedDaemonEventCheckpointPersistedAfterSeq = sharedDaemonEventAfterSeq;
    sharedDaemonEventCheckpointPersistedGeneration = sharedDaemonEventGeneration;
    sharedDaemonEventCheckpointPersistedAtUnixMs = now;
  } catch (error) {
    console.warn("Failed to persist backend daemon event checkpoint:", error);
  }
}

async function ensureSharedDaemonRuntimeEventLoop(): Promise<void> {
  if (sharedDaemonEventLoopRunning) {
    if (sharedDaemonEventSubscribers.size > 0) {
      sharedDaemonEventLoopStopRequested = false;
    }
    return;
  }

  await loadSharedDaemonEventCursor();
  sharedDaemonEventLoopRunning = true;
  sharedDaemonEventLoopStopRequested = false;

  try {
    while (!sharedDaemonEventLoopStopRequested) {
      try {
        const events = await getRuntimeEvents(sharedDaemonEventAfterSeq, DEFAULT_DAEMON_EVENT_LIMIT);
        const recovered = sharedDaemonEventUnavailableNotified;
        sharedDaemonEventFailureCount = 0;
        sharedDaemonEventUnavailableNotified = false;
        const generationChanged =
          sharedDaemonEventGeneration !== null && events.generation !== sharedDaemonEventGeneration;
        sharedDaemonEventGeneration = events.generation;

        if (recovered) {
          emitSharedDaemonRuntimeEvent({
            seq: 0,
            event: "daemon_control_plane_recovered",
            payload: {
              source: "daemon_event_feed",
            },
            emitted_at_unix_ms: Date.now(),
          });
        }

        if (events.resync_required || generationChanged) {
          sharedDaemonEventAfterSeq = Math.max(0, events.latest_available_seq);
          await persistSharedDaemonEventCheckpoint();
          emitSharedDaemonRuntimeEvent(
            buildDaemonEventFeedResyncNotice(
              events,
              generationChanged ? "generation_changed" : "cursor_invalid",
            ),
          );
        } else {
          for (const event of events.events) {
            sharedDaemonEventAfterSeq = Math.max(sharedDaemonEventAfterSeq, event.seq);
            emitSharedDaemonRuntimeEvent(event);
          }
          await persistSharedDaemonEventCheckpoint();
        }
      } catch (error) {
        if (!sharedDaemonEventLoopStopRequested) {
          console.error("Failed to poll shared daemon runtime events:", error);
          sharedDaemonEventFailureCount += 1;
          if (
            !sharedDaemonEventUnavailableNotified &&
            sharedDaemonEventFailureCount >= 3
          ) {
            sharedDaemonEventUnavailableNotified = true;
            emitSharedDaemonRuntimeEvent({
              seq: 0,
              event: "system_error",
              payload: {
                code: "DAEMON_EVENT_FEED_UNAVAILABLE",
                subsystem: "daemon_event_feed",
                message:
                  "DashDrop temporarily lost contact with the daemon event feed. The UI will retry automatically.",
              },
              emitted_at_unix_ms: Date.now(),
            });
          }
        }
      }

      if (sharedDaemonEventLoopStopRequested || sharedDaemonEventSubscribers.size === 0) {
        break;
      }

      await new Promise((resolve) =>
        window.setTimeout(resolve, DEFAULT_DAEMON_EVENT_POLL_MS),
      );
    }
  } finally {
    sharedDaemonEventLoopRunning = false;
    sharedDaemonEventLoopStopRequested = false;
    sharedDaemonEventFailureCount = 0;
    sharedDaemonEventUnavailableNotified = false;
    sharedDaemonEventCheckpointPersistedAfterSeq = -1;
    sharedDaemonEventCheckpointPersistedGeneration = undefined;
    sharedDaemonEventCheckpointPersistedAtUnixMs = 0;
  }
}

function emitSharedDaemonRuntimeEvent(event: RuntimeEventEnvelope) {
  for (const subscriber of [...sharedDaemonEventSubscribers]) {
    try {
      subscriber(event);
    } catch (error) {
      console.error("Daemon runtime event subscriber failed:", error);
    }
  }
}

export function __resetDaemonRuntimeEventLoopForTests() {
  sharedDaemonEventSubscribers.clear();
  sharedDaemonEventAfterSeq = 0;
  sharedDaemonEventLoopRunning = false;
  sharedDaemonEventLoopStopRequested = false;
  sharedDaemonEventFailureCount = 0;
  sharedDaemonEventUnavailableNotified = false;
  sharedDaemonEventGeneration = null;
  sharedDaemonEventCheckpointPersistedAfterSeq = -1;
  sharedDaemonEventCheckpointPersistedGeneration = undefined;
  sharedDaemonEventCheckpointPersistedAtUnixMs = 0;
}

async function subscribeLocalRuntimeEvents(
  events: Iterable<RuntimeEventName>,
  callback: (event: RuntimeEventEnvelope) => void,
): Promise<() => void> {
  const unlistens = await Promise.all(
    [...events].map((eventName) =>
      listenEvent<unknown>(eventName, (payload) =>
        callback({
          seq: 0,
          event: eventName,
          payload,
          emitted_at_unix_ms: Date.now(),
        }),
      ),
    ),
  );

  return () => {
    for (const unlisten of unlistens) {
      unlisten();
    }
  };
}

export async function subscribeRuntimeEvents(
  events: RuntimeEventName[],
  callback: (event: RuntimeEventEnvelope) => void,
  options?: DaemonRuntimeEventPollOptions,
): Promise<() => void> {
  const eventSet = new Set(events);
  if (eventSet.size === 0) {
    return () => {};
  }

  const runtimeMode = await getControlPlaneMode();
  if (runtimeMode === "daemon") {
    const { daemonFeedEvents, localShellEvents } = splitRuntimeEventSources(eventSet);
    const unlistens: Array<() => void> = [];

    if (daemonFeedEvents.length > 0) {
      const daemonEventSet = new Set(daemonFeedEvents);
      unlistens.push(
        await subscribeDaemonRuntimeEvents((entry) => {
          if (daemonEventSet.has(entry.event as RuntimeEventName)) {
            callback(entry);
          }
        }, options),
      );
    }

    if (localShellEvents.length > 0) {
      // In daemon mode the daemon owns runtime state, but shell ingress remains
      // local to the UI process: second-instance activation, deep links, tray
      // navigation, hide/reveal, and system-notice fallback all enter here.
      unlistens.push(await subscribeLocalRuntimeEvents(localShellEvents, callback));
    }

    return () => {
      for (const unlisten of unlistens) {
        unlisten();
      }
    };
  }

  return subscribeLocalRuntimeEvents(eventSet, callback);
}
