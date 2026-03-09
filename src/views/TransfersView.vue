<script setup lang="ts">
import { computed } from 'vue';
import { activeTransfers, incomingQueue } from '../store';
import ProgressBar from '../components/ProgressBar.vue';
import { acceptTransfer, acceptAndPairTransfer, cancelAllTransfers, cancelTransfer, connectByAddress, openTransferFolder, pairDevice, rejectTransfer, retryTransfer } from '../ipc';

const emit = defineEmits(['openSettings']);

const hasTransfers = computed(() => Object.keys(activeTransfers.value).length > 0);
const hasActiveRunning = computed(() =>
  Object.values(activeTransfers.value).some((t) => t.status === 'PendingAccept' || t.status === 'Transferring'),
);

const handleAccept = async (id: string) => {
  await acceptTransfer(id);
};

const handleAcceptAndPair = async (id: string, senderFp: string) => {
  await acceptAndPairTransfer(id, senderFp);
};

const handleReject = async (id: string) => {
  await rejectTransfer(id);
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

const handleConnectByAddress = async () => {
  try {
    const address = window.prompt('Enter peer address (host:port)');
    if (!address) return;
    const result = await connectByAddress(address.trim());
    const ok = window.confirm(
      `Connected to ${result.address}\nFingerprint: ${result.fingerprint}\n\nConfirm this identity before sending files.`,
    );
    if (!ok) return;
    if (!result.trusted) {
      const remember = window.confirm(
        `Remember and pair this device?\n${result.name}\n${result.fingerprint}`,
      );
      if (remember) {
        await pairDevice(result.fingerprint);
      }
    }
  } catch (e) {
    console.error('Connect by address failed', e);
  }
};

const canRetry = (status: string, direction: string) =>
  direction === 'Send' &&
  (status === 'Failed' ||
    status === 'CancelledBySender' ||
    status === 'CancelledByReceiver' ||
    status === 'Rejected' ||
    status === 'PartialCompleted');

const retryLabel = (status: string) =>
  status === 'PartialCompleted' ? 'Retry Failed Files' : 'Retry';

const handleRetry = async (transferId: string) => {
  try {
    await retryTransfer(transferId);
  } catch (e) {
    console.error('Retry failed', e);
  }
};

const handleCancelAll = async () => {
  try {
    await cancelAllTransfers();
  } catch (e) {
    console.error('Cancel all failed', e);
  }
};
</script>

<template>
  <div class="view-container animate-fade-in">
    <header class="view-header">
      <h2>Transfers</h2>
      <button class="btn btn-secondary" style="padding: 6px 12px;" @click="emit('openSettings')">⚙️</button>
    </header>
    <main class="content">
      <div class="top-actions">
        <button class="btn btn-secondary" :disabled="!hasActiveRunning" @click="handleCancelAll">
          Cancel All Active
        </button>
        <button class="btn btn-secondary" @click="handleConnectByAddress">
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
                Untrusted device • verify fingerprint {{ request.sender_fp.slice(-8) }}
              </div>
            </div>
            <div class="incoming-actions">
              <button class="btn btn-secondary" @click="handleReject(request.transfer_id)">Reject</button>
              <button class="btn btn-secondary" v-if="!request.trusted" @click="handleAcceptAndPair(request.transfer_id, request.sender_fp)">Accept & Pair</button>
              <button class="btn btn-primary" @click="handleAccept(request.transfer_id)">Accept</button>
            </div>
          </article>
        </div>
      </section>

      <div class="transfers-list" v-if="hasTransfers">
        <div v-for="t in activeTransfers" :key="t.id" class="transfer-card">
          <div class="transfer-meta" style="display: flex; justify-content: space-between; align-items: center; margin-bottom: 8px;">
            <span class="peer-name">{{ t.direction === 'Send' ? 'To ' : 'From ' }}{{ t.peer_name }}</span>
            <div class="transfer-actions" style="display: flex; gap: 8px;">
              <button 
                v-if="t.status === 'Transferring' || t.status === 'PendingAccept'" 
                @click="cancelTransfer(t.id)" 
                style="font-size: 0.8rem; padding: 4px 8px; border-radius: 4px; border: 1px solid rgba(255,255,255,0.2); background: transparent; color: inherit; cursor: pointer;">
                Cancel
              </button>
              <button 
                v-if="t.status === 'Completed' && t.direction === 'Receive'" 
                @click="openTransferFolder(t.id)" 
                style="font-size: 0.8rem; padding: 4px 8px; border-radius: 4px; border: 1px solid rgba(255,255,255,0.2); background: transparent; color: inherit; cursor: pointer;">
                Open Folder
              </button>
              <button
                v-if="canRetry(t.status, t.direction)"
                @click="handleRetry(t.id)"
                style="font-size: 0.8rem; padding: 4px 8px; border-radius: 4px; border: 1px solid rgba(255,255,255,0.2); background: transparent; color: inherit; cursor: pointer;">
                {{ retryLabel(t.status) }}
              </button>
            </div>
          </div>
          <ProgressBar 
            :progress="t.total_bytes > 0 ? (t.bytes_transferred / t.total_bytes) : 0"
            :status="t.status"
            :bytesReceived="t.bytes_transferred"
            :totalBytes="t.total_bytes"
          />
        </div>
      </div>

      <div class="empty-state" v-else>
        <p class="text-muted">No active transfers.</p>
      </div>
    </main>
  </div>
</template>

<style scoped>
.view-container {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
}

.view-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 24px 32px;
}

.content {
  flex: 1;
  padding: 0 32px 32px;
  display: flex;
  flex-direction: column;
  gap: 24px;
}

.top-actions {
  display: flex;
  justify-content: flex-end;
}

.incoming-section {
  display: flex;
  flex-direction: column;
  gap: 10px;
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
  padding: 14px;
  border-radius: var(--radius-md);
  border: 1px solid var(--border-light);
  background: rgba(245, 158, 11, 0.08);
}

.incoming-main .peer {
  font-weight: 600;
}

.incoming-main .meta,
.incoming-main .risk {
  font-size: 0.85rem;
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
  background: rgba(255,255,255,0.02);
  border: 1px dashed var(--border-light);
  border-radius: var(--radius-xl);
}

.transfers-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
  max-height: calc(100vh - 180px);
  overflow-y: auto;
}

.transfer-card {
  padding: 16px;
  background: var(--bg-surface);
  border-radius: var(--radius-md);
  border: 1px solid var(--border-light);
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.transfer-meta {
  display: flex;
  justify-content: space-between;
  font-weight: 500;
}
</style>
