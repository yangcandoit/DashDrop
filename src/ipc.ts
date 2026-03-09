import { invoke } from "@tauri-apps/api/core";
import { listen, type Event } from "@tauri-apps/api/event";
import type { DeviceInfo, TrustedPeer, TransferTask, LocalIdentity, TransferStartedPayload, TransferIncomingPayload, TransferProgressPayload, TransferCompletePayload, TransferPartialPayload, TransferFailedPayload, TransferErrorPayload, IdentityMismatchPayload } from "./types";

// --- Commands ---

export async function getDevices(): Promise<DeviceInfo[]> {
    return invoke<DeviceInfo[]>("get_devices");
}

export async function getTrustedPeers(): Promise<TrustedPeer[]> {
    return invoke<TrustedPeer[]>("get_trusted_peers");
}

export async function pairDevice(fp: string): Promise<void> {
    return invoke("pair_device", { fp });
}

export async function unpairDevice(fp: string): Promise<void> {
    return invoke("unpair_device", { fp });
}

export async function sendFiles(peerFp: string, paths: string[]): Promise<void> {
    return invoke("send_files_cmd", { peerFp, paths });
}

export async function acceptTransfer(transferId: string): Promise<void> {
    return invoke("accept_transfer", { transferId });
}

export async function acceptAndPairTransfer(transferId: string, senderFp: string): Promise<void> {
    return invoke("accept_and_pair_transfer", { transferId, senderFp });
}

export async function rejectTransfer(transferId: string): Promise<void> {
    return invoke("reject_transfer", { transferId });
}

export async function cancelTransfer(transferId: string): Promise<void> {
    return invoke("cancel_transfer", { transferId });
}

export async function openTransferFolder(transferId: string): Promise<void> {
    return invoke("open_transfer_folder", { transferId });
}

export async function getAppConfig(): Promise<{ device_name: string; download_dir: string | null }> {
    return invoke("get_app_config");
}

export async function setAppConfig(config: { device_name: string; download_dir: string | null }): Promise<void> {
    return invoke("set_app_config", { config });
}

export async function getLocalIdentity(): Promise<LocalIdentity> {
    return invoke<LocalIdentity>("get_local_identity");
}

export async function getTransfers(): Promise<TransferTask[]> {
    return invoke<TransferTask[]>("get_transfers");
}

// --- Events ---

export function onDeviceDiscovered(callback: (device: DeviceInfo) => void) {
    return listen("device_discovered", (event: Event<DeviceInfo>) => callback(event.payload));
}

export function onDeviceUpdated(callback: (device: DeviceInfo) => void) {
    return listen("device_updated", (event: Event<DeviceInfo>) => callback(event.payload));
}

export function onDeviceLost(callback: (fp: string) => void) {
    return listen("device_lost", (event: Event<{ fingerprint: string }>) => callback(event.payload.fingerprint));
}

export function onTransferStarted(callback: (payload: TransferStartedPayload) => void) {
    return listen("transfer_started", (event: Event<TransferStartedPayload>) => callback(event.payload));
}

export function onTransferIncoming(callback: (payload: TransferIncomingPayload) => void) {
    return listen("transfer_incoming", (event: Event<TransferIncomingPayload>) => callback(event.payload));
}

export function onTransferProgress(callback: (payload: TransferProgressPayload) => void) {
    return listen("transfer_progress", (event: Event<TransferProgressPayload>) => callback(event.payload));
}

export function onTransferComplete(callback: (payload: TransferCompletePayload) => void) {
    return listen("transfer_complete", (event: Event<TransferCompletePayload>) => callback(event.payload));
}

export function onTransferPartial(callback: (payload: TransferPartialPayload) => void) {
    return listen("transfer_partial", (event: Event<TransferPartialPayload>) => callback(event.payload));
}

export function onTransferFailed(callback: (payload: TransferFailedPayload) => void) {
    return listen("transfer_failed", (event: Event<TransferFailedPayload>) => callback(event.payload));
}

export function onTransferError(callback: (payload: TransferErrorPayload) => void) {
    return listen("transfer_error", (event: Event<TransferErrorPayload>) => callback(event.payload));
}

export function onSystemError(callback: (payload: { subsystem: string; message: string }) => void) {
    return listen("system_error", (event: Event<{ subsystem: string; message: string }>) => callback(event.payload));
}

export function onIdentityMismatch(callback: (payload: IdentityMismatchPayload) => void) {
    return listen("identity_mismatch", (event: Event<IdentityMismatchPayload>) => callback(event.payload));
}
