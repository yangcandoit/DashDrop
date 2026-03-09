<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed } from 'vue';
import { open, message } from '@tauri-apps/plugin-dialog';
import DeviceCard from '../components/DeviceCard.vue';
import { sendFiles } from '../ipc';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import type { DeviceView } from '../types';
import { myIdentity, devices, incomingQueue, sendingPeerFingerprints } from '../store';

const emit = defineEmits(['openSettings']);

const incomingCount = computed(() => incomingQueue.value.length);
const activeDropTargetFp = ref<string | null>(null);

let dragDropUnlisten: (() => void) | null = null;

onMounted(async () => {
  dragDropUnlisten = await getCurrentWebview().onDragDropEvent(async (event) => {
    if (event.payload.type !== 'drop') {
      return;
    }

    const paths = event.payload.paths;
    const targetFp = activeDropTargetFp.value;
    activeDropTargetFp.value = null;

    if (!targetFp) {
      await message('Drop files directly on a device card.', {
        title: 'No target device',
        kind: 'warning',
      });
      return;
    }

    const target = devices.value.find((d) => d.fingerprint === targetFp);
    if (!target) {
      await message('Selected device is no longer available.', {
        title: 'Device unavailable',
        kind: 'warning',
      });
      return;
    }

    await sendGivenPathsToDevice(paths, target);
  });
});

onUnmounted(() => {
  if (dragDropUnlisten) dragDropUnlisten();
});

const handleDeviceClick = async (device: DeviceView) => {
  const selected = await open({
    multiple: true,
    title: `Send to ${device.name}`,
  });

  if (selected && selected.length > 0) {
    const paths = Array.isArray(selected) ? selected : [selected];
    await sendGivenPathsToDevice(paths, device);
  }
};

const handleDragTargetEnter = (device: DeviceView) => {
  activeDropTargetFp.value = device.fingerprint;
};

const handleDragTargetLeave = (device: DeviceView) => {
  if (activeDropTargetFp.value === device.fingerprint) {
    activeDropTargetFp.value = null;
  }
};

const sendGivenPathsToDevice = async (paths: string[], device: DeviceView) => {
  if (sendingPeerFingerprints.value.has(device.fingerprint)) return;

  try {
    await sendFiles(device.fingerprint, paths);
  } catch (e: unknown) {
    console.error('Failed to send files:', e);
    const detail = String(e || '').toLowerCase();
    let userReason = 'Unknown transport error.';
    if (detail.includes('all connection attempts failed')) {
      userReason = 'Peer is unreachable on all known addresses.';
    } else if (detail.includes('quic handshake')) {
      userReason = 'Secure handshake failed.';
    } else if (detail.includes('identity mismatch')) {
      userReason = 'Identity verification failed.';
    } else if (detail.includes('timeout')) {
      userReason = 'Peer did not respond in time.';
    }
    await message(
      `Failed to send files to ${device.name}.\nReason: ${userReason}\nOpen Transfers or Security Events for details, then retry.`,
      { title: 'Transfer Failed', kind: 'error' },
    );
  }
};
</script>

<template>
  <div class="nearby-view">
    <header class="view-header">
      <div class="title-wrap">
        <h2>Nearby</h2>
        <p class="text-muted subtitle">Select a device card to start transfer</p>
      </div>
      <div class="header-actions" v-if="myIdentity">
        <div class="my-identity">
          <span class="identity-label">This device</span>
          <span class="identity-name">{{ myIdentity.device_name }}</span>
        </div>
        <button @click="emit('openSettings')" class="btn btn-secondary">Settings</button>
      </div>
    </header>

    <main class="content">
      <div class="devices-section">
        <div class="devices-grid" v-if="devices.length > 0">
          <DeviceCard
            v-for="device in devices"
            :key="device.fingerprint"
            :device="device"
            :isSending="sendingPeerFingerprints.has(device.fingerprint)"
            @click="handleDeviceClick(device)"
            @drag-target-enter="handleDragTargetEnter(device)"
            @drag-target-leave="handleDragTargetLeave(device)"
          />
        </div>

        <div class="empty-state" v-else>
          <p>Scanning local network</p>
          <p class="text-muted">Keep both devices awake and on the same Wi-Fi/LAN.</p>
        </div>
      </div>
    </main>

    <div v-if="incomingCount > 0" class="incoming-hint">
      {{ incomingCount }} incoming request{{ incomingCount > 1 ? 's' : '' }} waiting in Transfers
    </div>
  </div>
</template>

<style scoped>
.nearby-view {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  background: linear-gradient(160deg, rgba(255, 255, 255, 0.4), transparent 35%);
}

.view-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 14px;
  padding: 26px 28px 14px;
}

.title-wrap {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.subtitle {
  font-size: 0.86rem;
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}

.my-identity {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 2px;
  padding: 7px 10px;
  border-radius: 10px;
  border: 1px solid var(--border-subtle);
  background: rgba(255, 255, 255, 0.5);
}

.identity-label {
  font-size: 0.66rem;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: var(--text-subtle);
}

.identity-name {
  font-size: 0.86rem;
  font-weight: 600;
  color: var(--text-secondary);
}

.content {
  flex: 1;
  padding: 0 28px 26px;
  display: flex;
  flex-direction: column;
  gap: 20px;
  overflow-y: auto;
}

.devices-section {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.devices-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(290px, 1fr));
  gap: 12px;
}

.empty-state {
  min-height: 210px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  border: 1px dashed var(--border-subtle);
  border-radius: 18px;
  background: rgba(255, 255, 255, 0.42);
  color: var(--text-secondary);
}

.incoming-hint {
  margin: 0 28px 20px auto;
  padding: 7px 10px;
  border-radius: 999px;
  border: 1px solid var(--border-subtle);
  background: rgba(255, 255, 255, 0.75);
  color: var(--text-secondary);
  font-size: 0.76rem;
  letter-spacing: 0.03em;
  text-transform: uppercase;
}

@media (max-width: 820px) {
  .view-header {
    flex-direction: column;
    align-items: flex-start;
  }

  .header-actions {
    width: 100%;
    justify-content: space-between;
  }
}
</style>
