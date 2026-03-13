import { computed, ref } from "vue";
import type {
  TransferView,
  DeviceView,
  TransferIncomingPayload,
  LocalIdentity,
  ExternalSharePayload,
  PairingLinkPayload,
  DaemonEventFeedResyncPayload,
  AppNavigationPayload,
  RuntimeEventEnvelope,
  TransferStartedPayload,
  TransferAcceptedPayload,
  TransferProgressPayload,
  TransferCompletePayload,
  TransferPartialPayload,
  TransferRejectedPayload,
  TransferCancelledPayload,
  TransferFailedPayload,
  TransferErrorPayload,
  SystemErrorPayload,
  IdentityMismatchPayload,
  FingerprintChangedPayload,
} from "./types";
import {
  getLocalIdentity,
  getDevices,
  getTransfers,
  getPendingIncomingRequests,
  getTransfer,
  subscribeRuntimeEvents,
  setShellAttentionState,
} from "./ipc";
import { sendNotification, isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";

export type SystemNoticeTarget =
  | "Nearby"
  | "Transfers"
  | "History"
  | "TrustedDevices"
  | "SecurityEvents"
  | "Settings";

export interface SystemNoticeState {
  message: string;
  tone?: "error" | "warning" | "info";
  code?: string;
  actionLabel?: string;
  actionTarget?: SystemNoticeTarget;
}

// Shared UI-shell ingress contract. These events remain local-shell owned in
// daemon mode and are the only ones that should drive top-level routing / share
// intake without going through runtime-state projection.
const APP_STORE_RUNTIME_EVENTS = [
  "device_discovered",
  "device_updated",
  "device_lost",
  "transfer_started",
  "transfer_incoming",
  "transfer_accepted",
  "transfer_progress",
  "transfer_complete",
  "transfer_partial",
  "transfer_rejected",
  "transfer_cancelled_by_sender",
  "transfer_cancelled_by_receiver",
  "transfer_failed",
  "transfer_error",
  "system_error",
  "identity_mismatch",
  "fingerprint_changed",
  "external_share_received",
  "pairing_link_received",
  "app_navigation_requested",
  "app_window_revealed",
  "daemon_control_plane_recovered",
  "daemon_event_feed_resync_required",
] as const;

export const myIdentity = ref<LocalIdentity | null>(null);
export const devices = ref<DeviceView[]>([]);
export const activeTransfers = ref<Record<string, TransferView>>({});
export const incomingQueue = ref<TransferIncomingPayload[]>([]);
export const systemError = ref<SystemNoticeState | null>(null);
// External-share intake is a queued selection for Nearby. The shell may replace
// the pending list with a newer handoff, but it never auto-sends files.
export const externalSharePaths = ref<string[]>([]);
export const externalShareSource = ref<string | null>(null);
// Pairing deep links stay pending until a view explicitly imports or dismisses
// them, which lets Settings / Nearby / Trusted Devices all reuse the same flow.
export const pendingPairingLink = ref<string | null>(null);
export const pendingPairingLinkSource = ref<string | null>(null);
export const pendingNavigationTarget = ref<SystemNoticeTarget | null>(null);
export const pendingNavigationSource = ref<string | null>(null);
const notificationsDegraded = ref(false);
const recentTransferFailureAttentionCount = ref(0);
export const sendingPeerFingerprints = computed(() => {
  const sending = new Set<string>();
  for (const task of Object.values(activeTransfers.value)) {
    if (
      task.direction === "Send" &&
      (task.status === "PendingAccept" || task.status === "Transferring")
    ) {
      sending.add(task.peer_fingerprint);
    }
  }
  return sending;
});

let unlistens: Array<() => void> = [];
let clearSystemErrorTimer: ReturnType<typeof setTimeout> | null = null;
const handledExpiredRequestErrors = new Set<string>();

function actionableMessage(summary: string, nextSteps: string[]): string {
  if (nextSteps.length === 0) return summary;
  return `${summary} Next: ${nextSteps.join(" ")}`;
}

function queueExternalShare(payload: ExternalSharePayload) {
  const paths = Array.isArray(payload.paths)
    ? payload.paths.filter((value): value is string => typeof value === "string" && value.length > 0)
    : [];
  if (paths.length === 0) {
    return;
  }
  externalSharePaths.value = paths;
  externalShareSource.value = payload.source ?? null;
  setSystemError(
    {
      message: actionableMessage(
        `Received ${paths.length} shared item${paths.length > 1 ? "s" : ""}.`,
        ["Open Nearby and choose a device to send them."],
      ),
      tone: "info",
      code: "EXTERNAL_SHARE_RECEIVED",
      actionLabel: "Open Nearby",
      actionTarget: "Nearby",
    },
    12_000,
  );
}

function queuePairingLink(payload: PairingLinkPayload) {
  const pairingLink =
    typeof payload.pairing_link === "string" ? payload.pairing_link.trim() : "";
  if (!pairingLink) {
    return;
  }
  pendingPairingLink.value = pairingLink;
  pendingPairingLinkSource.value = payload.source ?? null;
  setSystemError(
    {
      message: actionableMessage(
        "Received a DashDrop pairing link.",
        ["Open Settings to review the device and confirm trust."],
      ),
      tone: "info",
      code: "PAIRING_LINK_RECEIVED",
      actionLabel: "Open Settings",
      actionTarget: "Settings",
    },
    12_000,
  );
}

export function clearExternalShare() {
  externalSharePaths.value = [];
  externalShareSource.value = null;
}

export function clearPendingPairingLink() {
  pendingPairingLink.value = null;
  pendingPairingLinkSource.value = null;
  clearSystemNoticeByCode("PAIRING_LINK_RECEIVED");
}

function isSystemNoticeTarget(value: unknown): value is SystemNoticeTarget {
  return (
    value === "Nearby" ||
    value === "Transfers" ||
    value === "History" ||
    value === "TrustedDevices" ||
    value === "SecurityEvents" ||
    value === "Settings"
  );
}

function queueNavigationRequest(payload: AppNavigationPayload) {
  if (!isSystemNoticeTarget(payload.target)) {
    return;
  }
  pendingNavigationTarget.value = payload.target;
  pendingNavigationSource.value = payload.source ?? null;
}

export function clearPendingNavigationRequest() {
  pendingNavigationTarget.value = null;
  pendingNavigationSource.value = null;
}

function transferErrorNextSteps(reasonCode: string): string[] {
  switch (reasonCode) {
    case "E_REQUEST_EXPIRED":
      return ["Ask the sender to share the files again."];
    case "E_TIMEOUT":
      return ["Check both devices are online and retry."];
    case "E_DISK_FULL":
      return ["Free disk space on receiver, then retry failed files."];
    case "E_IDENTITY_MISMATCH":
      return ["Stop transfer and verify fingerprint before retrying."];
    case "E_PATH_CONFLICT":
      return ["Adjust conflict strategy in Settings or rename source files, then retry."];
    case "E_PROTOCOL":
      return [
        "Ensure both peers run the same DashDrop version.",
        "If needed, unpair and pair again, then retry.",
      ];
    default:
      return ["Open Security/History for details, then retry."];
  }
}

function normalizeReasonCode(reasonCode: string): string {
  return reasonCode.startsWith("E_") ? reasonCode : "E_PROTOCOL";
}

function summarizeTransferError(err: { reason_code: string; terminal_cause: string; detail?: string }): string {
  const code = normalizeReasonCode(err.reason_code);
  if (code === "E_REQUEST_EXPIRED") {
    return "This transfer request expired";
  }
  if (code === "E_TIMEOUT") {
    return "Timed out while waiting for peer response";
  }
  if (code === "E_DISK_FULL") {
    return "Receiver reported insufficient disk space";
  }
  if (code === "E_IDENTITY_MISMATCH") {
    return "Peer identity verification failed";
  }
  if (code === "E_PATH_CONFLICT") {
    return "Receiver encountered a file conflict";
  }
  if (code !== "E_PROTOCOL") {
    return code.replace(/^E_/, "").replace(/_/g, " ").toLowerCase();
  }

  const rawDetail = err.detail || (!err.reason_code.startsWith("E_") ? err.reason_code : "");
  const detail = rawDetail.toLowerCase();
  if (detail.includes("read len") || detail.includes("read body")) {
    return "Peer closed the control stream unexpectedly";
  }
  if (err.terminal_cause === "ProtocolSequenceError") {
    return "Peer rejected protocol sequence (possible version mismatch)";
  }
  if (err.terminal_cause === "OfferTooLarge") {
    return "Transfer offer is too large (too many entries in one batch)";
  }
  if (detail.includes("quic handshake")) {
    return "Secure channel handshake failed";
  }
  if (detail.includes("all connection attempts failed")) {
    return "Peer is unreachable on all advertised addresses";
  }
  if (detail.includes("connection attempts failed")) {
    return "Could not establish a reliable connection to peer";
  }
  if (err.terminal_cause && err.terminal_cause !== "SystemError") {
    return `Protocol failure during ${err.terminal_cause}`;
  }
  return "Protocol error during transfer";
}

function setSystemError(notice: string | SystemNoticeState, timeoutMs = 10_000) {
  systemError.value =
    typeof notice === "string"
      ? {
          message: notice,
          tone: "error",
        }
      : notice;
  if (clearSystemErrorTimer) {
    clearTimeout(clearSystemErrorTimer);
    clearSystemErrorTimer = null;
  }
  if (timeoutMs > 0) {
    clearSystemErrorTimer = setTimeout(() => {
      systemError.value = null;
      clearSystemErrorTimer = null;
    }, timeoutMs);
  }
}

function clearSystemNoticeByCode(code: string) {
  if (systemError.value?.code !== code) {
    return;
  }
  systemError.value = null;
  if (clearSystemErrorTimer) {
    clearTimeout(clearSystemErrorTimer);
    clearSystemErrorTimer = null;
  }
}

function activeTransferCount(): number {
  return Object.values(activeTransfers.value).filter(
    (task) => task.status === "PendingAccept" || task.status === "Transferring",
  ).length;
}

function syncShellAttentionState() {
  // Keep this aggregate aligned with the Tauri command in `commands.rs` and the
  // tray rendering in `lib.rs`; it is the only shell-attention payload shape.
  void setShellAttentionState(
    incomingQueue.value.length,
    activeTransferCount(),
    recentTransferFailureAttentionCount.value,
    notificationsDegraded.value,
  ).catch((error) => {
    console.warn("Failed to sync shell attention state:", error);
  });
}

function maybeClearNotificationFallbackNotice() {
  if (incomingQueue.value.length === 0) {
    clearSystemNoticeByCode("NOTIFICATION_DEGRADED");
  }
}

function showTransferAttentionNotice(summary: string) {
  setSystemError(
    {
      message: actionableMessage(
        summary,
        ["Open Transfers to review the task state or retry from History if needed."],
      ),
      tone: "warning",
      code: "TRANSFER_ATTENTION_REQUIRED",
      actionLabel: "Open Transfers",
      actionTarget: "Transfers",
    },
    0,
  );
}

async function notifyTransferAttention(title: string, body: string, fallbackSummary: string) {
  recentTransferFailureAttentionCount.value += 1;
  syncShellAttentionState();
  const delivered = await notifyUser(title, body);
  if (!delivered) {
    showTransferAttentionNotice(fallbackSummary);
  }
}

async function notifyUser(title: string, body: string): Promise<boolean> {
  let permissionGranted = await isPermissionGranted();
  if (!permissionGranted) {
    const permission = await requestPermission();
    permissionGranted = permission === "granted";
  }
  if (permissionGranted) {
    notificationsDegraded.value = false;
    maybeClearNotificationFallbackNotice();
    syncShellAttentionState();
    await sendNotification({ title, body });
    return true;
  }
  notificationsDegraded.value = true;
  syncShellAttentionState();
  return false;
}

function showNotificationFallbackNotice() {
  setSystemError(
    {
      message: actionableMessage(
        "System notifications are unavailable, so incoming requests stay in the Transfers queue and tray status instead.",
        ["Open Transfers to accept or reject them before they expire."],
      ),
      tone: "warning",
      code: "NOTIFICATION_DEGRADED",
      actionLabel: "Open Transfers",
      actionTarget: "Transfers",
    },
    0,
  );
}

function removeIncoming(transferId: string) {
  const queue = Array.isArray(incomingQueue.value) ? incomingQueue.value : [];
  incomingQueue.value = queue.filter((entry) => entry.transfer_id !== transferId);
  syncShellAttentionState();
  maybeClearNotificationFallbackNotice();
}

function upsertIncoming(payload: TransferIncomingPayload) {
  removeIncoming(payload.transfer_id);
  incomingQueue.value = [...incomingQueue.value, payload];
  syncShellAttentionState();
}

function applyBatchId(task: TransferView, batchId?: string | null) {
  if (batchId !== undefined) {
    task.batch_id = batchId;
  }
}

function applyRevision(task: TransferView, revision: number): boolean {
  const currentRevision = task.revision ?? 0;
  if (revision < currentRevision) {
    return false;
  }
  task.revision = revision;
  return true;
}

async function hydrateTransfer(transferId: string): Promise<TransferView | null> {
  try {
    const transfer = await getTransfer(transferId);
    if (transfer) {
      activeTransfers.value[transferId] = transfer;
    }
    return transfer;
  } catch (e) {
    console.error(`Failed to fetch transfer ${transferId}:`, e);
    return null;
  }
}

async function applyStateEvent(
  transferId: string,
  batchId: string | null | undefined,
  revision: number,
  updater: (task: TransferView) => void,
): Promise<boolean> {
  let task: TransferView | null | undefined = activeTransfers.value[transferId];
  if (!task) {
    task = await hydrateTransfer(transferId);
    if (!task) {
      await fetchSnapshot();
      task = activeTransfers.value[transferId];
      if (!task) {
        return false;
      }
    }
  }

  const currentRevision = task.revision ?? 0;
  if (revision > currentRevision + 1) {
    await fetchSnapshot();
    task = activeTransfers.value[transferId];
    if (!task) {
      return false;
    }
  }

  if (!applyRevision(task, revision)) {
    return false;
  }

  applyBatchId(task, batchId);
  updater(task);
  syncShellAttentionState();
  return true;
}

function applyProgressEvent(
  transferId: string,
  batchId: string | null | undefined,
  revision: number,
  updater: (task: TransferView) => void,
) {
  const task = activeTransfers.value[transferId];
  if (!task) {
    return;
  }

  const currentRevision = task.revision ?? 0;
  if (revision < currentRevision) {
    return;
  }
  task.revision = revision;
  applyBatchId(task, batchId);
  updater(task);
}

function addOrUpdateTerminalError(
  transferId: string,
  batchId: string | null | undefined,
  status: TransferView["status"],
  reason: string,
  revision: number,
) {
  removeIncoming(transferId);
  void (async () => {
    let peerName = "peer";
    const updated = await applyStateEvent(transferId, batchId, revision, (t) => {
      peerName = t.peer_name;
      t.status = status;
      t.error = reason;
      t.bytes_transferred = Math.min(t.total_bytes, t.bytes_transferred);
    });
    if (!updated) {
      return;
    }

    if (status === "Failed") {
      await notifyTransferAttention(
        "Transfer Failed",
        `A transfer with ${peerName} failed.`,
        `A transfer with ${peerName} failed.`,
      );
      return;
    }
    if (status === "PartialCompleted") {
      await notifyTransferAttention(
        "Transfer Finished With Errors",
        `Some files from ${peerName} need attention.`,
        `A transfer with ${peerName} completed partially and needs review.`,
      );
      return;
    }
    if (status === "Rejected") {
      await notifyTransferAttention(
        "Transfer Rejected",
        `${peerName} rejected a transfer request.`,
        `${peerName} rejected a transfer request.`,
      );
    }
  })();
}

function expiredRequestErrorKey(err: { transfer_id: string | null; reason_code: string; revision: number }): string {
  return `${err.transfer_id ?? "global"}:${err.reason_code}:${err.revision}`;
}

function handleDeviceDiscovered(d: DeviceView) {
  const idx = devices.value.findIndex((existing) => existing.fingerprint === d.fingerprint);
  if (idx === -1) {
    devices.value.push(d);
  }
}

function handleDeviceUpdated(d: DeviceView) {
  const idx = devices.value.findIndex((existing) => existing.fingerprint === d.fingerprint);
  if (idx !== -1) {
    devices.value[idx] = { ...devices.value[idx], ...d };
  } else {
    devices.value.push(d);
  }
}

function handleDeviceLost(fp: string) {
  devices.value = devices.value.filter((d) => d.fingerprint !== fp);
}

function handleTransferStarted(payload: TransferStartedPayload) {
  activeTransfers.value[payload.transfer_id] = {
    id: payload.transfer_id,
    batch_id: payload.batch_id ?? null,
    direction: "Send",
    peer_fingerprint: payload.peer_fp,
    peer_name: payload.peer_name,
    items: payload.items,
    status: "PendingAccept",
    bytes_transferred: 0,
    total_bytes: payload.total_size,
    revision: payload.revision,
  };
  syncShellAttentionState();
}

function handleTransferIncoming(payload: TransferIncomingPayload) {
  upsertIncoming(payload);
  void notifyUser(
    "Incoming Transfer Request",
    `${payload.sender_name} wants to send ${payload.items.length} item${payload.items.length === 1 ? "" : "s"}`,
  ).then((delivered) => {
    if (!delivered) {
      showNotificationFallbackNotice();
    }
  });
}

async function handleTransferAccepted(payload: TransferAcceptedPayload) {
  removeIncoming(payload.transfer_id);
  await applyStateEvent(payload.transfer_id, payload.batch_id, payload.revision, (t) => {
    t.status = "Transferring";
  });
}

function handleTransferProgress(payload: TransferProgressPayload) {
  applyProgressEvent(payload.transfer_id, payload.batch_id, payload.revision, (t) => {
    t.bytes_transferred = payload.bytes_transferred;
    t.total_bytes = payload.total_bytes;
  });
}

function handleTransferComplete(payload: TransferCompletePayload) {
  removeIncoming(payload.transfer_id);
  void (async () => {
    let title = "Transfer Complete";
    let body = "Successfully transferred files.";
    const updated = await applyStateEvent(payload.transfer_id, payload.batch_id, payload.revision, (t) => {
      t.status = "Completed";
      t.bytes_transferred = t.total_bytes;
      title = t.direction === "Send" ? "Upload Complete" : "Download Complete";
      body = `Successfully transferred files ${t.direction === "Send" ? "to" : "from"} ${t.peer_name}`;
    });
    if (updated) {
      void notifyUser(title, body);
    }
  })();
}

function handleTransferPartial(payload: TransferPartialPayload) {
  removeIncoming(payload.transfer_id);
  void applyStateEvent(payload.transfer_id, payload.batch_id, payload.revision, (t) => {
    t.status = "PartialCompleted";
    t.bytes_transferred = t.total_bytes;
  });
}

function handleTransferRejected(payload: TransferRejectedPayload) {
  addOrUpdateTerminalError(
    payload.transfer_id,
    payload.batch_id,
    "Rejected",
    payload.reason_code,
    payload.revision,
  );
}

function handleTransferCancelledBySender(payload: TransferCancelledPayload) {
  addOrUpdateTerminalError(
    payload.transfer_id,
    payload.batch_id,
    "CancelledBySender",
    payload.reason_code,
    payload.revision,
  );
}

function handleTransferCancelledByReceiver(payload: TransferCancelledPayload) {
  addOrUpdateTerminalError(
    payload.transfer_id,
    payload.batch_id,
    "CancelledByReceiver",
    payload.reason_code,
    payload.revision,
  );
}

function handleTransferFailed(payload: TransferFailedPayload) {
  addOrUpdateTerminalError(
    payload.transfer_id,
    payload.batch_id,
    "Failed",
    payload.reason_code,
    payload.revision,
  );
}

function handleTransferError(err: TransferErrorPayload) {
  const normalizedReasonCode = normalizeReasonCode(err.reason_code);
  if (normalizedReasonCode === "E_REQUEST_EXPIRED") {
    if (err.transfer_id) {
      removeIncoming(err.transfer_id);
    }
    const key = expiredRequestErrorKey({
      transfer_id: err.transfer_id,
      reason_code: normalizedReasonCode,
      revision: err.revision,
    });
    if (handledExpiredRequestErrors.has(key)) {
      return;
    }
    handledExpiredRequestErrors.add(key);
    setSystemError(
      actionableMessage(
        summarizeTransferError(err),
        transferErrorNextSteps(normalizedReasonCode),
      ),
    );
    return;
  }
  if (err.transfer_id) {
    addOrUpdateTerminalError(
      err.transfer_id,
      err.batch_id,
      "Failed",
      normalizedReasonCode,
      err.revision,
    );
  }
  setSystemError(
    actionableMessage(
      `Transfer failed during ${err.phase}: ${summarizeTransferError(err)}`,
      transferErrorNextSteps(normalizedReasonCode),
    ),
  );
}

function handleSystemError(payload: SystemErrorPayload) {
  if (payload.code === "MDNS_REGISTER_FAILED" || payload.code === "MDNS_BROWSER_FAILED") {
    setSystemError(
      {
        message: actionableMessage(
          payload.message,
          [
            "Allow Local Network access for DashDrop in macOS privacy settings.",
            "On Windows, ensure UDP port 5353 is allowed in Windows Defender Firewall.",
            "On Linux, check if avahi-daemon is conflicting on port 5353 or ufw is blocking it.",
            "Alternatively, use 'Connect by Address' if mDNS is unavailable.",
          ],
        ),
        tone: "error",
        code: payload.code,
        actionLabel: "Open Settings",
        actionTarget: "Settings",
      },
      0,
    );
    return;
  }
  if (payload.code === "QUIC_SERVER_START_FAILED") {
    setSystemError(
      {
        message: actionableMessage(
          payload.message,
          ["Close other network tools using the same stack and relaunch DashDrop."],
        ),
        tone: "error",
        code: payload.code,
        actionLabel: "Open Settings",
        actionTarget: "Settings",
      },
      0,
    );
    return;
  }
  if (payload.code === "MDNS_REREGISTER_FAILED") {
    const rollbackName = payload.rollback_device_name || "previous value";
    const attemptedName = payload.attempted_device_name || "new value";
    setSystemError(
      {
        message: actionableMessage(
          `Device name update failed and was rolled back (${attemptedName} -> ${rollbackName}). ${payload.message}`,
          ["Keep current name or retry with another device name in Settings."],
        ),
        tone: "warning",
        code: payload.code,
        actionLabel: "Open Settings",
        actionTarget: "Settings",
      },
      0,
    );
    return;
  }
  if (payload.code === "DAEMON_CONTROL_PLANE_FALLBACK") {
    setSystemError(
      {
        message: actionableMessage(
          `${payload.message}${payload.daemon_status ? ` Current daemon status: ${payload.daemon_status}.` : ''}${payload.daemon_connect_attempts ? ` Attach attempts: ${payload.daemon_connect_attempts}.` : ''}${payload.daemon_connect_strategy ? ` Strategy: ${payload.daemon_connect_strategy}.` : ''}`,
          [
            "Open Settings and confirm Control Plane / Daemon Status.",
            "If this is a packaged build, rebuild or reinstall the app bundle.",
            "If this is a dev session, prepare the sidecar with npm run tauri:prepare-sidecar.",
          ],
        ),
        tone: "warning",
        code: payload.code,
        actionLabel: "Open Settings",
        actionTarget: "Settings",
      },
      0,
    );
    return;
  }
  if (payload.code === "DAEMON_BINARY_MISSING") {
    setSystemError(
      {
        message: actionableMessage(
          payload.message,
          [
            "Rebuild or reinstall the app so the dashdropd sidecar is bundled.",
            "Use Settings runtime status to confirm daemon binary path after relaunch.",
          ],
        ),
        tone: "error",
        code: payload.code,
        actionLabel: "Open Settings",
        actionTarget: "Settings",
      },
      0,
    );
    return;
  }
  if (payload.code === "DAEMON_UI_HIDDEN") {
    setSystemError(
      {
        message: actionableMessage(
          payload.message,
          ["Use the dock/taskbar icon or reopen the app when you want the window back."],
        ),
        tone: "info",
        code: payload.code,
        actionLabel: "Open Transfers",
        actionTarget: "Transfers",
      },
      12_000,
    );
    return;
  }
  if (payload.code === "DAEMON_EVENT_FEED_UNAVAILABLE") {
    setSystemError(
      {
        message: actionableMessage(
          payload.message,
          ["Keep the app open; DashDrop will retry automatically and refresh once the control plane responds again."],
        ),
        tone: "warning",
        code: payload.code,
        actionLabel: "Open Settings",
        actionTarget: "Settings",
      },
      0,
    );
    return;
  }
  setSystemError(
    {
      message: actionableMessage(payload.message, ["Check network/service status in Settings."]),
      tone: "error",
      code: payload.code,
      actionLabel: "Open Settings",
      actionTarget: "Settings",
    },
  );
}

function handleIdentityMismatch(payload: IdentityMismatchPayload) {
  const expected = payload.expected_fp || payload.mdns_fp || "unknown";
  const actual = payload.actual_fp || payload.cert_fp || "unknown";
  const phase = payload.phase ? ` (${payload.phase})` : "";
  setSystemError(
    {
      message: actionableMessage(
        `Security warning${phase}: peer identity mismatch (expected ${expected}, got ${actual}).`,
        ["Do not continue transfer until fingerprint is verified out-of-band."],
      ),
      tone: "warning",
      code: "IDENTITY_MISMATCH",
      actionLabel: "Open Security Events",
      actionTarget: "SecurityEvents",
    },
    20_000,
  );
}

function handleFingerprintChanged(payload: FingerprintChangedPayload) {
  const severe = payload.previous_trust_level === "mutual_confirmed";
  setSystemError(
    {
      message: actionableMessage(
        `${severe ? "Security alert" : "Security warning"}: paired peer fingerprint changed on session ${payload.session_id} (previous ${payload.previous_fp}, current ${payload.current_fp}).`,
        severe
          ? [
              "Treat this as a broken mutual-trust relationship.",
              "Unpair the device and repeat the full signed-link verification flow before sending sensitive files.",
            ]
          : ["Unpair and re-verify this device before sending sensitive files."],
      ),
      tone: severe ? "error" : "warning",
      code: "FINGERPRINT_CHANGED",
      actionLabel: "Open Security Events",
      actionTarget: "SecurityEvents",
    },
    30_000,
  );
}

function handleExternalShareReceived(payload: ExternalSharePayload) {
  queueExternalShare(payload);
}

function handleAppWindowRevealed() {
  clearSystemNoticeByCode("DAEMON_UI_HIDDEN");
  clearSystemNoticeByCode("TRANSFER_ATTENTION_REQUIRED");
  if (recentTransferFailureAttentionCount.value !== 0) {
    recentTransferFailureAttentionCount.value = 0;
    syncShellAttentionState();
  }
}

async function handleDaemonControlPlaneRecovered() {
  clearSystemNoticeByCode("DAEMON_EVENT_FEED_UNAVAILABLE");
  clearSystemNoticeByCode("DAEMON_EVENT_FEED_RESYNCED");
  try {
    myIdentity.value = await getLocalIdentity();
    devices.value = await getDevices();
    await fetchSnapshot();
  } catch (error) {
    console.error("Failed to refresh state after daemon control-plane recovery:", error);
  }
}

async function handleDaemonEventFeedResyncRequired(
  payload?: DaemonEventFeedResyncPayload,
) {
  clearSystemNoticeByCode("DAEMON_EVENT_FEED_UNAVAILABLE");
  clearSystemNoticeByCode("DAEMON_EVENT_FEED_RESYNCED");
  try {
    myIdentity.value = await getLocalIdentity();
    devices.value = await getDevices();
    await fetchSnapshot();
    const reason = payload?.reason;
    if (!reason || reason === "generation_changed") {
      return;
    }

    let summary = "DashDrop refreshed daemon state after replay had to resync.";
    let nextSteps = ["Open Settings diagnostics if this keeps happening."];

    if (reason === "cursor_before_oldest_available") {
      summary =
        "DashDrop refreshed daemon state because the UI cursor fell behind the retained replay window.";
      nextSteps = [
        "Keep the app open during long transfer sessions if you need live incremental history.",
        "Open Settings diagnostics to compare replay retention and checkpoint state.",
      ];
    } else if (reason === "cursor_after_latest_available") {
      summary =
        "DashDrop refreshed daemon state because the UI cursor was ahead of the daemon's latest retained event.";
      nextSteps = [
        "If this repeats, relaunch the app and inspect daemon replay diagnostics in Settings.",
      ];
    } else if (reason === "persisted_catch_up_empty") {
      summary =
        "DashDrop refreshed daemon state because SQLite catch-up could not supply the expected replay gap.";
      nextSteps = [
        "Open Settings diagnostics to inspect replay journal health and compaction state.",
      ];
    } else if (reason === "persisted_journal_unavailable") {
      summary =
        "DashDrop refreshed daemon state because the persisted replay journal was temporarily unavailable.";
      nextSteps = [
        "Open Settings diagnostics to inspect replay journal health.",
        "Relaunch the app if daemon storage continues to look stale.",
      ];
    } else if (reason === "cursor_invalid") {
      summary =
        "DashDrop refreshed daemon state because the previous replay cursor was no longer valid.";
    }

    setSystemError(
      {
        message: actionableMessage(summary, nextSteps),
        tone: "warning",
        code: "DAEMON_EVENT_FEED_RESYNCED",
        actionLabel: "Open Settings",
        actionTarget: "Settings",
      },
      12_000,
    );
  } catch (error) {
    console.error("Failed to resync state after daemon event-feed reset:", error);
    const reason = payload?.reason;
    const summary =
      reason === "persisted_journal_unavailable"
        ? "DashDrop could not reload daemon state after the persisted replay journal became unavailable."
        : "DashDrop had to resync daemon state after an event-feed reset.";
    setSystemError(
      actionableMessage(summary, [
        "Open Settings diagnostics or relaunch the app if data still looks stale.",
      ]),
      15_000,
    );
  }
}

async function dispatchRuntimeEvent(event: string, payload: unknown) {
  switch (event) {
    case "device_discovered":
      handleDeviceDiscovered(payload as DeviceView);
      break;
    case "device_updated":
      handleDeviceUpdated(payload as DeviceView);
      break;
    case "device_lost":
      handleDeviceLost((payload as { fingerprint: string }).fingerprint);
      break;
    case "transfer_started":
      handleTransferStarted(payload as TransferStartedPayload);
      break;
    case "transfer_incoming":
      handleTransferIncoming(payload as TransferIncomingPayload);
      break;
    case "transfer_accepted":
      await handleTransferAccepted(payload as TransferAcceptedPayload);
      break;
    case "transfer_progress":
      handleTransferProgress(payload as TransferProgressPayload);
      break;
    case "transfer_complete":
      handleTransferComplete(payload as TransferCompletePayload);
      break;
    case "transfer_partial":
      handleTransferPartial(payload as TransferPartialPayload);
      break;
    case "transfer_rejected":
      handleTransferRejected(payload as TransferRejectedPayload);
      break;
    case "transfer_cancelled_by_sender":
      handleTransferCancelledBySender(payload as TransferCancelledPayload);
      break;
    case "transfer_cancelled_by_receiver":
      handleTransferCancelledByReceiver(payload as TransferCancelledPayload);
      break;
    case "transfer_failed":
      handleTransferFailed(payload as TransferFailedPayload);
      break;
    case "transfer_error":
      handleTransferError(payload as TransferErrorPayload);
      break;
    case "system_error":
      handleSystemError(payload as SystemErrorPayload);
      break;
    case "identity_mismatch":
      handleIdentityMismatch(payload as IdentityMismatchPayload);
      break;
    case "fingerprint_changed":
      handleFingerprintChanged(payload as FingerprintChangedPayload);
      break;
    case "external_share_received":
      handleExternalShareReceived(payload as ExternalSharePayload);
      break;
    case "pairing_link_received":
      queuePairingLink(payload as PairingLinkPayload);
      break;
    case "app_navigation_requested":
      queueNavigationRequest(payload as AppNavigationPayload);
      break;
    case "app_window_revealed":
      handleAppWindowRevealed();
      break;
    case "daemon_control_plane_recovered":
      await handleDaemonControlPlaneRecovered();
      break;
    case "daemon_event_feed_resync_required":
      await handleDaemonEventFeedResyncRequired(payload as DaemonEventFeedResyncPayload);
      break;
    default:
      break;
  }
}

export async function fetchSnapshot() {
  try {
    const transfers = await getTransfers();
    const next: Record<string, TransferView> = {};
    for (const t of transfers) {
      next[t.id] = t;
    }
    activeTransfers.value = next;
    incomingQueue.value = await getPendingIncomingRequests();
    syncShellAttentionState();
  } catch (e) {
    console.error("Failed to fetch snapshot:", e);
    setSystemError(
      actionableMessage(
        "Failed to refresh transfer state.",
        ["Relaunch the app or open Settings diagnostics if the issue persists."],
      ),
    );
  }
}

async function initRuntimeSubscriptions() {
  unlistens.push(
    await subscribeRuntimeEvents(
      [...APP_STORE_RUNTIME_EVENTS],
      (entry: RuntimeEventEnvelope) => {
        void dispatchRuntimeEvent(entry.event, entry.payload);
      },
    ),
  );
}

export async function initAppStore() {
  myIdentity.value = await getLocalIdentity();
  devices.value = await getDevices();
  await fetchSnapshot();
  await initRuntimeSubscriptions();
  syncShellAttentionState();
}

export function destroyAppStore() {
  for (const unlisten of unlistens) {
    unlisten();
  }
  unlistens = [];
  if (clearSystemErrorTimer) {
    clearTimeout(clearSystemErrorTimer);
    clearSystemErrorTimer = null;
  }
  handledExpiredRequestErrors.clear();
  myIdentity.value = null;
  devices.value = [];
  activeTransfers.value = {};
  incomingQueue.value = [];
  systemError.value = null;
  externalSharePaths.value = [];
  externalShareSource.value = null;
  pendingPairingLink.value = null;
  pendingPairingLinkSource.value = null;
  pendingNavigationTarget.value = null;
  pendingNavigationSource.value = null;
  notificationsDegraded.value = false;
  recentTransferFailureAttentionCount.value = 0;
  syncShellAttentionState();
}
