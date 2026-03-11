import { computed, ref } from "vue";
import type { TransferView, DeviceView, TransferIncomingPayload, LocalIdentity } from "./types";
import {
  onDeviceDiscovered,
  onDeviceUpdated,
  onDeviceLost,
  onTransferStarted,
  onTransferIncoming,
  onTransferAccepted,
  onTransferProgress,
  onTransferComplete,
  onTransferPartial,
  onTransferRejected,
  onTransferCancelledBySender,
  onTransferCancelledByReceiver,
  onTransferFailed,
  onTransferError,
  onSystemError,
  onIdentityMismatch,
  onFingerprintChanged,
  getLocalIdentity,
  getDevices,
  getTransfers,
  getPendingIncomingRequests,
  getTransfer,
} from "./ipc";
import { sendNotification, isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";

export const myIdentity = ref<LocalIdentity | null>(null);
export const devices = ref<DeviceView[]>([]);
export const activeTransfers = ref<Record<string, TransferView>>({});
export const incomingQueue = ref<TransferIncomingPayload[]>([]);
export const systemError = ref<string | null>(null);
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

function setSystemError(message: string, timeoutMs = 10_000) {
  systemError.value = message;
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

async function notifyUser(title: string, body: string) {
  let permissionGranted = await isPermissionGranted();
  if (!permissionGranted) {
    const permission = await requestPermission();
    permissionGranted = permission === "granted";
  }
  if (permissionGranted) {
    sendNotification({ title, body });
  }
}

function removeIncoming(transferId: string) {
  incomingQueue.value = incomingQueue.value.filter((entry) => entry.transfer_id !== transferId);
}

function upsertIncoming(payload: TransferIncomingPayload) {
  removeIncoming(payload.transfer_id);
  incomingQueue.value.push(payload);
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
) {
  let task: TransferView | null | undefined = activeTransfers.value[transferId];
  if (!task) {
    task = await hydrateTransfer(transferId);
    if (!task) {
      await fetchSnapshot();
      task = activeTransfers.value[transferId];
      if (!task) {
        return;
      }
    }
  }

  const currentRevision = task.revision ?? 0;
  if (revision > currentRevision + 1) {
    await fetchSnapshot();
    task = activeTransfers.value[transferId];
    if (!task) {
      return;
    }
  }

  if (!applyRevision(task, revision)) {
    return;
  }

  applyBatchId(task, batchId);
  updater(task);
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
  void applyStateEvent(transferId, batchId, revision, (t) => {
    t.status = status;
    t.error = reason;
    t.bytes_transferred = Math.min(t.total_bytes, t.bytes_transferred);
  });
}

function expiredRequestErrorKey(err: { transfer_id: string | null; reason_code: string; revision: number }): string {
  return `${err.transfer_id ?? "global"}:${err.reason_code}:${err.revision}`;
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
  } catch (e) {
    console.error("Failed to fetch snapshot:", e);
  }
}

export async function initAppStore() {
  myIdentity.value = await getLocalIdentity();
  devices.value = await getDevices();
  await fetchSnapshot();

  unlistens.push(
    await onDeviceDiscovered((d) => {
      const idx = devices.value.findIndex((existing) => existing.fingerprint === d.fingerprint);
      if (idx === -1) {
        devices.value.push(d);
      }
    }),
  );

  unlistens.push(
    await onDeviceUpdated((d) => {
      const idx = devices.value.findIndex((existing) => existing.fingerprint === d.fingerprint);
      if (idx !== -1) {
        devices.value[idx] = { ...devices.value[idx], ...d };
      } else {
        devices.value.push(d);
      }
    }),
  );

  unlistens.push(
    await onDeviceLost((fp) => {
      devices.value = devices.value.filter((d) => d.fingerprint !== fp);
    }),
  );

  unlistens.push(
    await onTransferStarted((payload) => {
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
    }),
  );

  unlistens.push(
    await onTransferIncoming((payload) => {
      upsertIncoming(payload);
    }),
  );

  unlistens.push(
    await onTransferAccepted(async (payload) => {
      removeIncoming(payload.transfer_id);
      await applyStateEvent(payload.transfer_id, payload.batch_id, payload.revision, (t) => {
        t.status = "Transferring";
      });
    }),
  );

  unlistens.push(
    await onTransferProgress((payload) => {
      applyProgressEvent(payload.transfer_id, payload.batch_id, payload.revision, (t) => {
        t.bytes_transferred = payload.bytes_transferred;
        t.total_bytes = payload.total_bytes;
      });
    }),
  );

  unlistens.push(
    await onTransferComplete((payload) => {
      removeIncoming(payload.transfer_id);
      void applyStateEvent(payload.transfer_id, payload.batch_id, payload.revision, (t) => {
        t.status = "Completed";
        t.bytes_transferred = t.total_bytes;
        void notifyUser(
          t.direction === "Send" ? "Upload Complete" : "Download Complete",
          `Successfully transferred files ${t.direction === "Send" ? "to" : "from"} ${t.peer_name}`,
        );
      });
    }),
  );

  unlistens.push(
    await onTransferPartial((payload) => {
      removeIncoming(payload.transfer_id);
      void applyStateEvent(payload.transfer_id, payload.batch_id, payload.revision, (t) => {
        t.status = "PartialCompleted";
        t.bytes_transferred = t.total_bytes;
      });
    }),
  );

  unlistens.push(
    await onTransferRejected((payload) => {
      addOrUpdateTerminalError(
        payload.transfer_id,
        payload.batch_id,
        "Rejected",
        payload.reason_code,
        payload.revision,
      );
    }),
  );

  unlistens.push(
    await onTransferCancelledBySender((payload) => {
      addOrUpdateTerminalError(
        payload.transfer_id,
        payload.batch_id,
        "CancelledBySender",
        payload.reason_code,
        payload.revision,
      );
    }),
  );

  unlistens.push(
    await onTransferCancelledByReceiver((payload) => {
      addOrUpdateTerminalError(
        payload.transfer_id,
        payload.batch_id,
        "CancelledByReceiver",
        payload.reason_code,
        payload.revision,
      );
    }),
  );

  unlistens.push(
    await onTransferFailed((payload) => {
      addOrUpdateTerminalError(
        payload.transfer_id,
        payload.batch_id,
        "Failed",
        payload.reason_code,
        payload.revision,
      );
    }),
  );

  unlistens.push(
    await onTransferError((err) => {
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
    }),
  );

  unlistens.push(
    await onSystemError((payload) => {
      if (payload.code === "MDNS_REGISTER_FAILED" || payload.code === "MDNS_BROWSER_FAILED") {
        setSystemError(
          actionableMessage(
            payload.message,
            [
              "Allow Local Network access for DashDrop in macOS privacy settings.",
              "On Windows, ensure UDP port 5353 is allowed in Windows Defender Firewall.",
              "On Linux, check if avahi-daemon is conflicting on port 5353 or ufw is blocking it.",
              "Alternatively, use 'Connect by Address' if mDNS is unavailable.",
            ],
          ),
          0,
        );
        return;
      }
      if (payload.code === "QUIC_SERVER_START_FAILED") {
        setSystemError(
          actionableMessage(
            payload.message,
            ["Close other network tools using the same stack and relaunch DashDrop."],
          ),
          0,
        );
        return;
      }
      if (payload.code === "MDNS_REREGISTER_FAILED") {
        const rollbackName = payload.rollback_device_name || "previous value";
        const attemptedName = payload.attempted_device_name || "new value";
        setSystemError(
          actionableMessage(
            `Device name update failed and was rolled back (${attemptedName} -> ${rollbackName}). ${payload.message}`,
            ["Keep current name or retry with another device name in Settings."],
          ),
          0,
        );
        return;
      }
      setSystemError(
        actionableMessage(payload.message, ["Check network/service status in Settings."]),
      );
    }),
  );

  unlistens.push(
    await onIdentityMismatch((payload) => {
      const expected = payload.expected_fp || payload.mdns_fp || "unknown";
      const actual = payload.actual_fp || payload.cert_fp || "unknown";
      const phase = payload.phase ? ` (${payload.phase})` : "";
      setSystemError(
        actionableMessage(
          `Security warning${phase}: peer identity mismatch (expected ${expected}, got ${actual}).`,
          ["Do not continue transfer until fingerprint is verified out-of-band."],
        ),
        20_000,
      );
    }),
  );

  unlistens.push(
    await onFingerprintChanged((payload) => {
      setSystemError(
        actionableMessage(
          `Security warning: paired peer fingerprint changed on session ${payload.session_id} (previous ${payload.previous_fp}, current ${payload.current_fp}).`,
          ["Unpair and re-verify this device before sending sensitive files."],
        ),
        30_000,
      );
    }),
  );
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
}
