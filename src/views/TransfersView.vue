<script setup lang="ts">
import { computed, ref } from 'vue';
import { message } from '@tauri-apps/plugin-dialog';
import { activeTransfers, incomingQueue, myIdentity } from '../store';
import ProgressBar from '../components/ProgressBar.vue';
import {
  acceptTransfer,
  acceptAndPairTransfer,
  cancelAllTransfers,
  cancelTransfer,
  connectByAddress,
  openTransferFolder,
  pairDevice,
  rejectTransfer,
  retryTransfer,
} from '../ipc';
import type { ConnectByAddressResult } from '../types';
import { sharedVerificationCode, verificationCodeFromFingerprint } from '../security';

const emit = defineEmits(['openSettings']);

const hasTransfers = computed(() => Object.keys(activeTransfers.value).length > 0);
const hasActiveRunning = computed(() =>
  Object.values(activeTransfers.value).some((t) => t.status === 'PendingAccept' || t.status === 'Transferring'),
);

const showConnectDialog = ref(false);
const connectAddress = ref('');
const connectLoading = ref(false);
const connectError = ref<string | null>(null);
const connectResult = ref<ConnectByAddressResult | null>(null);
const rememberDevice = ref(true);
const connectVerified = ref(false);
const actionError = ref<string | null>(null);
const verifyIncomingRequest = ref<{
  transferId: string;
  notificationId: string;
  senderFp: string;
  senderName: string;
  mode: 'accept' | 'accept_and_pair';
} | null>(null);
const verifyIncomingBusy = ref(false);
const verifyIncomingConfirmed = ref(false);
const verificationCode = (fingerprint: string) =>
  myIdentity.value
    ? sharedVerificationCode(myIdentity.value.fingerprint, fingerprint)
    : verificationCodeFromFingerprint(fingerprint);

const setActionError = async (summary: string, error: unknown) => {
  const detail = errorToMessage(error);
  actionError.value = `${summary} ${detail}`;
  try {
    await message(actionError.value, { title: 'Transfer Action Failed', kind: 'error' });
  } catch (dialogError) {
    console.debug('Unable to show transfer action error dialog', dialogError);
  }
};

const handleAccept = async (id: string, notificationId: string) => {
  try {
    actionError.value = null;
    await acceptTransfer(id, notificationId);
  } catch (e) {
    console.error('Accept transfer failed', e);
    await setActionError('Unable to accept this incoming request.', e);
  }
};

const handleAcceptAndPair = async (id: string, notificationId: string, senderFp: string) => {
  try {
    actionError.value = null;
    await acceptAndPairTransfer(id, notificationId, senderFp);
  } catch (e) {
    console.error('Accept and pair failed', e);
    await setActionError('Unable to accept and pair with this sender.', e);
  }
};

const requestAcceptVerification = (
  transferId: string,
  notificationId: string,
  senderFp: string,
  senderName: string,
  mode: 'accept' | 'accept_and_pair',
) => {
  verifyIncomingRequest.value = {
    transferId,
    notificationId,
    senderFp,
    senderName,
    mode,
  };
  verifyIncomingConfirmed.value = false;
};

const closeIncomingVerification = () => {
  if (verifyIncomingBusy.value) return;
  verifyIncomingRequest.value = null;
  verifyIncomingConfirmed.value = false;
};

const confirmIncomingVerification = async () => {
  const request = verifyIncomingRequest.value;
  if (!request || !verifyIncomingConfirmed.value) return;

  verifyIncomingBusy.value = true;
  try {
    if (request.mode === 'accept_and_pair') {
      await handleAcceptAndPair(request.transferId, request.notificationId, request.senderFp);
    } else {
      await handleAccept(request.transferId, request.notificationId);
    }
    verifyIncomingRequest.value = null;
    verifyIncomingConfirmed.value = false;
  } finally {
    verifyIncomingBusy.value = false;
  }
};

const handleReject = async (id: string, notificationId: string) => {
  try {
    actionError.value = null;
    await rejectTransfer(id, notificationId);
  } catch (e) {
    console.error('Reject transfer failed', e);
    await setActionError('Unable to reject this incoming request.', e);
  }
};

const formatSize = (bytes: number) => {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  const gb = mb / 1024;
  return `${gb.toFixed(1)} GB`;
};

const formatRate = (bytesPerSecond: number) => `${formatSize(bytesPerSecond)}/s`;

const transferRate = (t: { status: string; started_at_unix?: number; bytes_transferred: number }) => {
  if (t.status !== 'Transferring' || !t.started_at_unix || t.bytes_transferred <= 0) {
    return null;
  }
  const elapsedSec = Math.max(1, Math.floor(Date.now() / 1000) - t.started_at_unix);
  return t.bytes_transferred / elapsedSec;
};

const transferItemsPreview = (items: Array<{ name: string }>) => {
  if (!items.length) return 'No items';
  const visible = items.slice(0, 3).map((i) => i.name).join(', ');
  if (items.length <= 3) return visible;
  return `${visible} +${items.length - 3}`;
};

const errorToMessage = (error: unknown): string => {
  const text = String(error || '').trim();
  if (!text) return 'Unable to connect to that address right now.';
  const lower = text.toLowerCase();
  if (lower.includes('e_request_expired')) {
    return 'This incoming request already expired. Ask the sender to share again.';
  }
  if (lower.includes('not found') || lower.includes('already handled')) {
    return 'The transfer state changed before this action completed. Refresh and try again.';
  }
  if (lower.includes('handshake')) {
    return 'Handshake failed. Verify both devices run compatible versions and are on the same LAN.';
  }
  if (lower.includes('identity')) {
    return 'Identity verification failed. Confirm fingerprint out-of-band before retrying.';
  }
  if (lower.includes('connection attempts failed')) {
    return 'No route to the peer from this network. Check address, firewall and local network.';
  }
  return text;
};

const openConnectDialog = () => {
  showConnectDialog.value = true;
  connectAddress.value = '';
  connectError.value = null;
  connectResult.value = null;
  rememberDevice.value = true;
  connectVerified.value = false;
  actionError.value = null;
};

const closeConnectDialog = () => {
  if (connectLoading.value) return;
  showConnectDialog.value = false;
};

const lookupPeerByAddress = async () => {
  const address = connectAddress.value.trim();
  if (!address) {
    connectError.value = 'Enter a host:port value.';
    return;
  }

  connectLoading.value = true;
  connectError.value = null;
  connectResult.value = null;

  try {
    const result = await connectByAddress(address);
    connectResult.value = result;
    rememberDevice.value = !result.trusted;
    connectVerified.value = result.trusted;
  } catch (e) {
    connectError.value = errorToMessage(e);
  } finally {
    connectLoading.value = false;
  }
};

const confirmConnectIdentity = async () => {
  if (!connectResult.value) return;
  if (!connectResult.value.trusted && !connectVerified.value) return;

  connectLoading.value = true;
  connectError.value = null;
  try {
    if (rememberDevice.value && !connectResult.value.trusted) {
      await pairDevice(connectResult.value.fingerprint);
    }
    showConnectDialog.value = false;
  } catch (e) {
    connectError.value = errorToMessage(e);
  } finally {
    connectLoading.value = false;
  }
};

const canRetry = (status: string, direction: string) =>
  direction === 'Send' &&
  (status === 'Failed' ||
    status === 'CancelledBySender' ||
    status === 'CancelledByReceiver' ||
    status === 'Rejected' ||
    status === 'PartialCompleted');

const retryLabel = (status: string) => (status === 'PartialCompleted' ? 'Retry Failed Files' : 'Retry');

const handleRetry = async (transferId: string) => {
  try {
    actionError.value = null;
    await retryTransfer(transferId);
  } catch (e) {
    console.error('Retry failed', e);
    await setActionError('Unable to retry this transfer.', e);
  }
};

const handleCancelAll = async () => {
  try {
    actionError.value = null;
    await cancelAllTransfers();
  } catch (e) {
    console.error('Cancel all failed', e);
    await setActionError('Unable to cancel all active transfers.', e);
  }
};

const handleCancel = async (transferId: string) => {
  try {
    actionError.value = null;
    await cancelTransfer(transferId);
  } catch (e) {
    console.error('Cancel transfer failed', e);
    await setActionError('Unable to cancel this transfer.', e);
  }
};

const handleOpenFolder = async (transferId: string) => {
  try {
    actionError.value = null;
    await openTransferFolder(transferId);
  } catch (e) {
    console.error('Open folder failed', e);
    await setActionError('Unable to open the saved transfer folder.', e);
  }
};
</script>

<template>
  <div class="view-container animate-fade-in">
    <header class="view-header">
      <div class="title-wrap">
        <h2>Transfers</h2>
        <p class="text-muted">Incoming requests, active jobs and retries</p>
      </div>
      <button class="btn btn-secondary" @click="emit('openSettings')">Settings</button>
    </header>

    <main class="content">
      <div v-if="actionError" class="error-banner">
        <span>{{ actionError }}</span>
        <button class="btn btn-secondary" @click="actionError = null">Dismiss</button>
      </div>
      <div class="top-actions">
        <button class="btn btn-secondary" :disabled="!hasActiveRunning" @click="handleCancelAll">
          Cancel All Active
        </button>
        <button class="btn btn-secondary" @click="openConnectDialog">
          Connect by Address
        </button>
      </div>

      <section class="incoming-section" v-if="incomingQueue.length > 0">
        <h3>Incoming Requests</h3>
        <div class="incoming-list">
          <article v-for="request in incomingQueue" :key="request.transfer_id" class="incoming-card">
            <div class="incoming-main">
              <div class="peer">{{ request.sender_name }}</div>
              <div class="meta text-muted">
                {{ request.items.length }} items • {{ formatSize(request.total_size) }}
              </div>
              <div v-if="!request.trusted" class="risk text-muted">
                Compare shared code {{ verificationCode(request.sender_fp) }}
              </div>
            </div>
            <div class="incoming-actions">
              <button class="btn btn-secondary" @click="handleReject(request.transfer_id, request.notification_id)">Reject</button>
              <button
                class="btn btn-secondary"
                v-if="!request.trusted"
                @click="requestAcceptVerification(request.transfer_id, request.notification_id, request.sender_fp, request.sender_name, 'accept_and_pair')"
              >
                Accept & Pair
              </button>
              <button
                class="btn btn-primary"
                @click="request.trusted ? handleAccept(request.transfer_id, request.notification_id) : requestAcceptVerification(request.transfer_id, request.notification_id, request.sender_fp, request.sender_name, 'accept')"
              >
                Accept
              </button>
            </div>
          </article>
        </div>
      </section>

      <div class="transfers-list" v-if="hasTransfers">
        <div v-for="t in activeTransfers" :key="t.id" class="transfer-card">
          <div class="transfer-meta">
            <div class="transfer-main">
              <span class="peer-name">{{ t.direction === 'Send' ? 'To ' : 'From ' }}{{ t.peer_name }}</span>
              <p class="transfer-files text-muted">{{ transferItemsPreview(t.items) }}</p>
            </div>
            <div class="transfer-actions">
              <button
                v-if="t.status === 'Transferring' || t.status === 'PendingAccept'"
                @click="handleCancel(t.id)"
                class="mini-btn"
              >
                Cancel
              </button>
              <button
                v-if="t.status === 'Completed' && t.direction === 'Receive'"
                @click="handleOpenFolder(t.id)"
                class="mini-btn"
              >
                Open Folder
              </button>
              <button
                v-if="canRetry(t.status, t.direction)"
                @click="handleRetry(t.id)"
                class="mini-btn"
              >
                {{ retryLabel(t.status) }}
              </button>
            </div>
          </div>
          <ProgressBar
            :progress="t.total_bytes > 0 ? t.bytes_transferred / t.total_bytes : 0"
            :status="t.status"
            :bytesReceived="t.bytes_transferred"
            :totalBytes="t.total_bytes"
          />
          <div class="transfer-foot">
            <span class="text-muted">{{ formatSize(t.bytes_transferred) }} / {{ formatSize(t.total_bytes) }}</span>
            <span v-if="transferRate(t)" class="rate-chip">{{ formatRate(transferRate(t) || 0) }}</span>
          </div>
        </div>
      </div>

      <div class="empty-state" v-else>
        <p class="text-muted">No active transfers.</p>
      </div>
    </main>

    <div v-if="showConnectDialog" class="dialog-backdrop" @click.self="closeConnectDialog">
      <section class="dialog-card">
        <div class="dialog-header">
          <h3>Connect by Address</h3>
          <button class="btn btn-secondary" :disabled="connectLoading" @click="closeConnectDialog">Close</button>
        </div>

        <div class="dialog-body">
          <label class="field-label" for="connect-address">Peer address</label>
          <input
            id="connect-address"
            v-model="connectAddress"
            class="field-input"
            placeholder="192.168.1.7:50306"
            :disabled="connectLoading"
            @keyup.enter="lookupPeerByAddress"
          />
          <p class="text-muted field-help">Use host:port from a trusted peer in the same LAN.</p>

          <div v-if="connectResult" class="peer-preview">
            <div class="preview-row">
              <span class="preview-key">Peer</span>
              <span class="preview-value">{{ connectResult.name }}</span>
            </div>
            <div class="preview-row">
              <span class="preview-key">Address</span>
              <span class="preview-value">{{ connectResult.address }}</span>
            </div>
            <div class="preview-row">
              <span class="preview-key">Fingerprint</span>
              <code class="fingerprint">{{ connectResult.fingerprint }}</code>
            </div>
            <div class="preview-row">
              <span class="preview-key">Shared Verification Code</span>
              <code class="fingerprint">{{ verificationCode(connectResult.fingerprint) }}</code>
            </div>
            <label v-if="!connectResult.trusted" class="remember-row">
              <input type="checkbox" v-model="connectVerified" :disabled="connectLoading" />
              <span>I compared this shared code on both devices</span>
            </label>
            <label v-if="!connectResult.trusted" class="remember-row">
              <input type="checkbox" v-model="rememberDevice" :disabled="connectLoading" />
              <span>Pair and remember this device after confirmation</span>
            </label>
          </div>

          <div v-if="connectError" class="connect-error">{{ connectError }}</div>
        </div>

        <div class="dialog-actions">
          <button class="btn btn-secondary" :disabled="connectLoading" @click="lookupPeerByAddress">
            {{ connectLoading && !connectResult ? 'Connecting...' : 'Connect' }}
          </button>
          <button
            class="btn btn-primary"
            :disabled="!connectResult || connectLoading || (!connectResult.trusted && !connectVerified)"
            @click="confirmConnectIdentity"
          >
            {{ connectLoading && connectResult ? 'Saving...' : 'Confirm Fingerprint' }}
          </button>
        </div>
      </section>
    </div>

    <div v-if="verifyIncomingRequest" class="dialog-backdrop" @click.self="closeIncomingVerification">
      <section class="dialog-card">
        <div class="dialog-header">
          <h3>Verify Sender Before Accepting</h3>
          <button class="btn btn-secondary" :disabled="verifyIncomingBusy" @click="closeIncomingVerification">Close</button>
        </div>
        <div class="dialog-body">
          <p class="text-muted">Confirm the sender identity for <strong>{{ verifyIncomingRequest.senderName }}</strong> before continuing.</p>
          <div class="peer-preview">
            <div class="preview-row">
              <span class="preview-key">Fingerprint</span>
              <code class="fingerprint">{{ verifyIncomingRequest.senderFp }}</code>
            </div>
            <div class="preview-row">
              <span class="preview-key">Shared Verification Code</span>
              <code class="fingerprint">{{ verificationCode(verifyIncomingRequest.senderFp) }}</code>
            </div>
          </div>
          <label class="remember-row">
            <input type="checkbox" v-model="verifyIncomingConfirmed" :disabled="verifyIncomingBusy" />
            <span>I compared this shared code on both devices</span>
          </label>
        </div>
        <div class="dialog-actions">
          <button class="btn btn-secondary" :disabled="verifyIncomingBusy" @click="closeIncomingVerification">Cancel</button>
          <button class="btn btn-primary" :disabled="verifyIncomingBusy || !verifyIncomingConfirmed" @click="confirmIncomingVerification">
            {{ verifyIncomingBusy ? 'Confirming...' : 'Continue' }}
          </button>
        </div>
      </section>
    </div>
  </div>
</template>

<style scoped>
.view-container {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  background: var(--surface);
  position: relative;
}

.view-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 20px 22px 12px;
  border-bottom: 1px solid var(--border-subtle);
}

.title-wrap {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.content {
  flex: 1;
  padding: 14px 22px 22px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  overflow-y: auto;
}

.top-actions {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
}

.error-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 12px;
  border-radius: 12px;
  border: 1px solid rgba(198, 40, 40, 0.25);
  background: rgba(198, 40, 40, 0.06);
  color: #8f2d2a;
}

.incoming-section {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px;
  border-radius: 12px;
  border: 1px solid var(--border-subtle);
  background: var(--surface-muted);
}

.incoming-list {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.incoming-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px;
  border-radius: 10px;
  border: 1px solid var(--border-subtle);
  background: #fff;
}

.incoming-main .peer {
  font-weight: 600;
}

.incoming-main .meta,
.incoming-main .risk {
  font-size: 0.83rem;
  color: var(--text-muted);
}

.incoming-actions {
  display: flex;
  gap: 8px;
}

.empty-state {
  height: 200px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--surface-muted);
  border: 1px solid var(--border-subtle);
  border-radius: 12px;
}

.transfers-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.transfer-card {
  padding: 12px;
  background: #fff;
  border-radius: 12px;
  border: 1px solid var(--border-subtle);
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.transfer-meta {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
  margin-bottom: 8px;
  font-weight: 500;
}

.transfer-main {
  min-width: 0;
}

.transfer-files {
  margin-top: 4px;
  font-size: 0.8rem;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 460px;
}

.transfer-actions {
  display: flex;
  gap: 8px;
}

.mini-btn {
  min-height: 26px;
  padding: 4px 8px;
  border-radius: 8px;
  border: 1px solid var(--border-subtle);
  background: #fff;
  color: var(--text-secondary);
  font-size: 0.75rem;
  font-weight: 600;
  cursor: pointer;
}

.transfer-foot {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  font-size: 0.78rem;
}

.rate-chip {
  display: inline-flex;
  align-items: center;
  border: 1px solid color-mix(in srgb, var(--accent) 35%, transparent);
  color: #005bb5;
  background: color-mix(in srgb, var(--accent) 8%, #fff);
  border-radius: 999px;
  padding: 2px 8px;
  font-weight: 600;
}

.dialog-backdrop {
  position: absolute;
  inset: 0;
  background: rgba(0, 0, 0, 0.2);
  display: flex;
  justify-content: center;
  align-items: center;
  padding: 18px;
  z-index: 40;
}

.dialog-card {
  width: min(620px, 100%);
  border-radius: 12px;
  border: 1px solid var(--border-subtle);
  background: #fff;
  box-shadow: var(--shadow-soft);
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 16px;
}

.dialog-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.dialog-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.field-label {
  font-size: 0.78rem;
  font-weight: 600;
  color: var(--text-subtle);
}

.field-input {
  width: 100%;
  min-height: 38px;
  border-radius: 10px;
  border: 1px solid var(--border-subtle);
  background: #fff;
  color: var(--text-secondary);
  padding: 9px 11px;
}

.field-help {
  font-size: 0.8rem;
}

.peer-preview {
  margin-top: 4px;
  border-radius: 10px;
  border: 1px solid var(--border-subtle);
  background: var(--surface-muted);
  padding: 10px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.preview-row {
  display: grid;
  grid-template-columns: 90px 1fr;
  gap: 8px;
  align-items: start;
}

.preview-key {
  font-size: 0.74rem;
  color: var(--text-subtle);
  font-weight: 600;
}

.preview-value {
  color: var(--text-secondary);
  font-size: 0.86rem;
}

.fingerprint {
  font-size: 0.74rem;
  line-height: 1.5;
  color: var(--text-secondary);
  word-break: break-all;
  padding: 4px 6px;
  border-radius: 8px;
  border: 1px solid var(--border-subtle);
  background: #fff;
}

.remember-row {
  margin-top: 4px;
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--text-secondary);
  font-size: 0.84rem;
}

.connect-error {
  border: 1px solid rgba(198, 40, 40, 0.28);
  background: rgba(198, 40, 40, 0.06);
  color: #9d1b1b;
  border-radius: 10px;
  padding: 8px 10px;
  font-size: 0.82rem;
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

@media (max-width: 820px) {
  .view-header {
    flex-direction: column;
    align-items: flex-start;
    gap: 10px;
  }

  .error-banner {
    flex-direction: column;
    align-items: flex-start;
  }

  .top-actions {
    justify-content: flex-start;
    flex-wrap: wrap;
  }

  .incoming-card {
    flex-direction: column;
    align-items: flex-start;
  }

  .incoming-actions {
    width: 100%;
    justify-content: flex-end;
    flex-wrap: wrap;
  }

  .transfer-meta {
    flex-direction: column;
  }

  .transfer-files {
    max-width: 100%;
  }

  .dialog-header {
    flex-direction: column;
    align-items: flex-start;
  }

  .preview-row {
    grid-template-columns: 1fr;
    gap: 4px;
  }

  .dialog-actions {
    width: 100%;
    justify-content: stretch;
  }

  .dialog-actions .btn {
    flex: 1;
  }
}
</style>
