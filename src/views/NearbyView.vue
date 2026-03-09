<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed } from 'vue';
import { open, message } from '@tauri-apps/plugin-dialog';
import DeviceCard from '../components/DeviceCard.vue';
import TransferModal from '../components/TransferModal.vue';
import ProgressBar from '../components/ProgressBar.vue';
import { getDevices, sendFiles, getTransfers,
  onDeviceDiscovered, onDeviceUpdated, onDeviceLost, onTransferStarted,
  onTransferIncoming, onTransferProgress, onTransferComplete, 
  onTransferPartial, onTransferFailed, onTransferError, getLocalIdentity,
  cancelTransfer, openTransferFolder, onSystemError, onIdentityMismatch
} from '../ipc';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { isPermissionGranted, requestPermission, sendNotification } from '@tauri-apps/plugin-notification';
import type { DeviceInfo, TransferTask, LocalIdentity, TransferIncomingPayload } from '../types';

async function notifyUser(title: string, body: string) {
  let permissionGranted = await isPermissionGranted();
  if (!permissionGranted) {
    const permission = await requestPermission();
    permissionGranted = permission === 'granted';
  }
  if (permissionGranted) {
    sendNotification({ title, body });
  }
}

const emit = defineEmits(['openSettings']);

const myIdentity = ref<LocalIdentity | null>(null);
const devices = ref<DeviceInfo[]>([]);
const activeTransfers = ref<Record<string, TransferTask>>({});
const incomingQueue = ref<TransferIncomingPayload[]>([]);
const incomingTransfer = computed(() => incomingQueue.value.length > 0 ? incomingQueue.value[0] : null);
const systemError = ref<string | null>(null);

const pendingTauriDrop = ref<string[] | null>(null);
const pendingDomTarget = ref<DeviceInfo | null>(null);

let unlistens: Array<() => void> = [];

onMounted(async () => {
  // Load initial state
  myIdentity.value = await getLocalIdentity();
  devices.value = await getDevices();
  
  const transfers = await getTransfers();
  for (const t of transfers) {
    activeTransfers.value[t.id] = t;
  }

  // Listen for mDNS device events
  unlistens.push(await onDeviceDiscovered((d) => {
    // Check if already in list
    const idx = devices.value.findIndex(existing => existing.fingerprint === d.fingerprint);
    if (idx === -1) {
      devices.value.push(d as DeviceInfo);
    }
  }));

  unlistens.push(await onDeviceUpdated((d) => {
    const idx = devices.value.findIndex(existing => existing.fingerprint === d.fingerprint);
    if (idx !== -1) {
      devices.value[idx] = { ...devices.value[idx], ...d };
    }
  }));

  unlistens.push(await onDeviceLost((fp) => {
    devices.value = devices.value.filter(d => d.fingerprint !== fp);
  }));

  // Listen for Tauri drag/drop globally
  unlistens.push(await getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === 'drop') {
      const paths = event.payload.paths;
      if (pendingDomTarget.value) {
        sendGivenPathsToDevice(paths, pendingDomTarget.value);
        pendingDomTarget.value = null;
        pendingTauriDrop.value = null;
      } else {
        pendingTauriDrop.value = paths;
        setTimeout(() => { pendingTauriDrop.value = null; }, 500);
      }
    }
  }));

  // Listen for transfer events
  unlistens.push(await onTransferStarted((payload) => {
    activeTransfers.value[payload.transfer_id] = {
      id: payload.transfer_id,
      direction: 'Send',
      peer_fingerprint: payload.peer_fp,
      peer_name: payload.peer_name,
      items: [],
      status: 'AwaitingAccept',
      bytes_transferred: 0,
      total_bytes: payload.total_size,
    };
  }));
  unlistens.push(await onTransferIncoming((payload) => {
    incomingQueue.value.push(payload);
  }));

  unlistens.push(await onTransferProgress((payload) => {
    const t = activeTransfers.value[payload.transfer_id];
    if (t) {
      t.bytes_transferred = payload.bytes_transferred;
      t.status = 'Transferring';
    }
  }));

  const markComplete = (id: string, partial: boolean) => {
    const t = activeTransfers.value[id];
    if (t) {
      t.status = partial ? 'PartialSuccess' : 'Complete';
      t.bytes_transferred = t.total_bytes;
      
      notifyUser(
        t.direction === 'Send' ? 'Upload Complete' : 'Download Complete',
        `Successfully transferred files ${t.direction === 'Send' ? 'to' : 'from'} ${t.peer_name}`
      );
    }
  };

  unlistens.push(await onTransferComplete((payload) => {
    markComplete(payload.transfer_id, false);
  }));

  unlistens.push(await onTransferPartial((payload) => {
    markComplete(payload.transfer_id, true);
  }));

  unlistens.push(await onTransferFailed((payload) => {
    const t = activeTransfers.value[payload.transfer_id];
    if (t) {
      t.status = 'Failed';
      notifyUser(
        t.direction === 'Send' ? 'Upload Failed' : 'Download Failed',
        `Transfer failed with ${t.peer_name}: ${payload.reason}`
      );
    }
  }));

  unlistens.push(await onTransferError((err) => {
    console.error("Transfer error:", err);
    if (err.transfer_id && activeTransfers.value[err.transfer_id]) {
      activeTransfers.value[err.transfer_id].status = 'Failed';
      return;
    }
    systemError.value = `Transfer error (${err.phase}): ${err.reason}`;
    setTimeout(() => { systemError.value = null; }, 10_000);
  }));

  unlistens.push(await onSystemError((payload: { subsystem: string; message: string }) => {
    systemError.value = payload.message;
    setTimeout(() => { systemError.value = null; }, 10_000);
  }));

  unlistens.push(await onIdentityMismatch((payload) => {
    const expected = payload.expected_fp || payload.mdns_fp || 'unknown';
    const actual = payload.actual_fp || payload.cert_fp || 'unknown';
    systemError.value = `Security warning: peer identity mismatch (expected ${expected}, got ${actual}).`;
  }));
});

onUnmounted(() => {
  unlistens.forEach(fn => fn());
});

const handleDeviceClick = async (device: DeviceInfo) => {
  // Open Tauri file dialog to pick files
  const selected = await open({
    multiple: true,
    title: `Send to ${device.name}`
  });

  if (selected && selected.length > 0) {
    const paths = Array.isArray(selected) ? selected : [selected];
    await sendGivenPathsToDevice(paths, device);
  }
};

const handleDomDrop = (device: DeviceInfo) => {
  if (pendingTauriDrop.value) {
    sendGivenPathsToDevice(pendingTauriDrop.value, device);
    pendingTauriDrop.value = null;
    pendingDomTarget.value = null;
  } else {
    pendingDomTarget.value = device;
    setTimeout(() => { pendingDomTarget.value = null; }, 500);
  }
};

const sendingTo = ref<string | null>(null);

const sendGivenPathsToDevice = async (paths: string[], device: DeviceInfo) => {
  if (sendingTo.value) return; // Prevent double-clicks
  
  try {
    sendingTo.value = device.fingerprint;
    await sendFiles(device.fingerprint, paths);
  } catch (e: unknown) {
    console.error("Failed to send files:", e);
    await message(`Failed to send files to ${device.name}:\n${String(e)}`, { title: 'Transfer Failed', kind: 'error' });
  } finally {
    sendingTo.value = null;
  }
};

const hasActiveTransfers = computed(() => Object.keys(activeTransfers.value).length > 0);

</script>

<template>
  <div class="nearby-view">
    <!-- System Error Banner -->
    <div v-if="systemError" class="system-error-banner" @click="systemError = null">
      ⚠️ {{ systemError }} <span style="opacity:0.7;font-size:0.8em">(click to dismiss)</span>
    </div>
    <header class="app-header">
      <div class="logo">
        <svg viewBox="0 0 24 24" fill="none" class="icon-logo" stroke="url(#gradient)" stroke-width="2.5">
          <defs>
            <linearGradient id="gradient" x1="0%" y1="0%" x2="100%" y2="100%">
              <stop offset="0%" stop-color="#3b82f6" />
              <stop offset="100%" stop-color="#8b5cf6" />
            </linearGradient>
          </defs>
          <path stroke-linecap="round" stroke-linejoin="round" d="M13 10V3L4 14h7v7l9-11h-7z" />
        </svg>
        <h1>DashDrop</h1>
      </div>
      <div class="my-identity" v-if="myIdentity">
        <div class="status-dot"></div>
        <span class="text-muted">{{ myIdentity.device_name }}</span>
        <button @click="emit('openSettings')" class="btn-icon" style="margin-left:8px; cursor: pointer; background: transparent; border: none;" title="Settings">⚙️</button>
      </div>
    </header>

    <main class="content">
      <div class="devices-section">
        <h2>Nearby Devices</h2>
        <p class="text-muted subtitle">Click a device to send files</p>
        
        <div class="devices-grid" v-if="devices.length > 0">
          <DeviceCard 
            v-for="device in devices" 
            :key="device.fingerprint"
            :device="device"
            :isSending="sendingTo === device.fingerprint"
            @click="handleDeviceClick(device)"
            @drop="handleDomDrop(device)"
          />
        </div>
        
        <div class="empty-state" v-else>
          <div class="radar-ping"></div>
          <p class="text-muted">Looking for nearby devices...</p>
        </div>
      </div>

      <div class="transfers-section glass-panel" v-if="hasActiveTransfers">
        <h3>Transfers</h3>
        <div class="transfers-list">
          <div v-for="t in activeTransfers" :key="t.id" class="transfer-card">
            <div class="transfer-meta" style="display: flex; justify-content: space-between; align-items: center; margin-bottom: 8px;">
              <span class="peer-name">{{ t.direction === 'Send' ? 'To ' : 'From ' }}{{ t.peer_name }}</span>
              <div class="transfer-actions" style="display: flex; gap: 8px;">
                <button 
                  v-if="t.status === 'Transferring' || t.status === 'AwaitingAccept'" 
                  @click="cancelTransfer(t.id)" 
                  style="font-size: 0.8rem; padding: 4px 8px; border-radius: 4px; border: 1px solid rgba(255,255,255,0.2); background: transparent; color: inherit; cursor: pointer;">
                  Cancel
                </button>
                <button 
                  v-if="t.status === 'Complete' && t.direction === 'Receive'" 
                  @click="openTransferFolder(t.id)" 
                  style="font-size: 0.8rem; padding: 4px 8px; border-radius: 4px; border: 1px solid rgba(255,255,255,0.2); background: transparent; color: inherit; cursor: pointer;">
                  Open Folder
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
      </div>
    </main>

    <TransferModal 
      v-if="incomingTransfer"
      :transferId="incomingTransfer.transfer_id"
      :senderName="incomingTransfer.sender_name"
      :senderFp="incomingTransfer.sender_fp"
      :trusted="incomingTransfer.trusted"
      :items="incomingTransfer.items"
      :totalSize="incomingTransfer.total_size"
      @close="incomingQueue.shift()"
    />
  </div>
</template>

<style scoped>
.nearby-view {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
}

.app-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 24px 32px;
  background: linear-gradient(to bottom, rgba(15, 17, 21, 0.8), transparent);
}

.logo {
  display: flex;
  align-items: center;
  gap: 12px;
}

.icon-logo {
  width: 32px;
  height: 32px;
}

.my-identity {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 16px;
  background: var(--bg-surface-elevated);
  border-radius: var(--radius-full);
  border: 1px solid var(--border-light);
}

.status-dot {
  width: 8px;
  height: 8px;
  background: var(--success);
  border-radius: 50%;
  box-shadow: 0 0 8px rgba(16, 185, 129, 0.6);
}

.content {
  flex: 1;
  padding: 0 32px 32px;
  display: flex;
  flex-direction: column;
  gap: 32px;
  overflow-y: auto;
}

.devices-section {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.subtitle {
  margin-top: -8px;
}

.devices-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 16px;
}

.empty-state {
  height: 200px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 24px;
  background: rgba(255, 255, 255, 0.02);
  border: 1px dashed var(--border-light);
  border-radius: var(--radius-xl);
}

.radar-ping {
  width: 48px;
  height: 48px;
  border-radius: 50%;
  background: rgba(59, 130, 246, 0.2);
  animation: pulse-glow 2s infinite cubic-bezier(0.4, 0, 0.6, 1);
}

.transfers-section {
  padding: 24px;
  border-radius: var(--radius-xl);
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.transfers-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
  max-height: 300px;
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

.system-error-banner {
  background: rgba(239, 68, 68, 0.15);
  border-bottom: 1px solid rgba(239, 68, 68, 0.3);
  color: #fca5a5;
  padding: 10px 32px;
  font-size: 0.9rem;
  cursor: pointer;
  transition: background 0.2s ease;
}

.system-error-banner:hover {
  background: rgba(239, 68, 68, 0.25);
}
</style>
