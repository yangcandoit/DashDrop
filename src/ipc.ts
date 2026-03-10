import { invoke } from "@tauri-apps/api/core";
import { listen, type Event } from "@tauri-apps/api/event";
import type {
  DeviceView,
  TrustedPeer,
  TransferView,
  ConnectByAddressResult,
  LocalIdentity,
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
  SecurityEvent,
  AppConfig,
  RuntimeStatus,
  DiscoveryDiagnostics,
  TransferMetrics,
} from "./types";

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

async function invokeCommand<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const mock = getTestMock();
  if (mock?.invoke) {
    return (await mock.invoke(command, args)) as T;
  }
  return invoke<T>(command, args);
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
  return invokeCommand<DeviceView[]>("get_devices");
}

export async function getTrustedPeers(): Promise<TrustedPeer[]> {
  return invokeCommand<TrustedPeer[]>("get_trusted_peers");
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

export async function sendFiles(peerFp: string, paths: string[]): Promise<void> {
  return invokeCommand("send_files_cmd", { peerFp, paths });
}

export async function connectByAddress(address: string): Promise<ConnectByAddressResult> {
  return invokeCommand<ConnectByAddressResult>("connect_by_address", { address });
}

export async function acceptTransfer(transferId: string): Promise<void> {
  return invokeCommand("accept_transfer", { transferId });
}

export async function acceptAndPairTransfer(transferId: string, senderFp: string): Promise<void> {
  return invokeCommand("accept_and_pair_transfer", { transferId, senderFp });
}

export async function rejectTransfer(transferId: string): Promise<void> {
  return invokeCommand("reject_transfer", { transferId });
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
  };
}

export async function setAppConfig(config: AppConfig): Promise<void> {
  return invokeCommand("set_app_config", { config });
}

export async function getLocalIdentity(): Promise<LocalIdentity> {
  return invokeCommand<LocalIdentity>("get_local_identity");
}

export async function getTransfers(): Promise<TransferView[]> {
  return invokeCommand<TransferView[]>("get_transfers");
}

export async function getTransfer(transferId: string): Promise<TransferView | null> {
  return invokeCommand<TransferView | null>("get_transfer", { transferId });
}

export async function getTransferHistory(limit = 50, offset = 0): Promise<TransferView[]> {
  return invokeCommand<TransferView[]>("get_transfer_history", { limit, offset });
}

export async function getSecurityPosture(): Promise<{ secure_store_available: boolean }> {
  return invokeCommand("get_security_posture");
}

export async function getSecurityEvents(limit = 50, offset = 0): Promise<SecurityEvent[]> {
  return invokeCommand("get_security_events", { limit, offset });
}

export async function getRuntimeStatus(): Promise<RuntimeStatus> {
  return invokeCommand("get_runtime_status");
}

export async function copyTextToClipboard(text: string): Promise<void> {
  return invokeCommand("copy_to_clipboard", { text });
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

export function onIdentityMismatch(callback: (payload: IdentityMismatchPayload) => void) {
  return listenEvent("identity_mismatch", callback);
}

export function onFingerprintChanged(callback: (payload: FingerprintChangedPayload) => void) {
  return listenEvent("fingerprint_changed", callback);
}
