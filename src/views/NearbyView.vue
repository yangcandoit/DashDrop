<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed } from 'vue';
import { open, message } from '@tauri-apps/plugin-dialog';
import DeviceCard from '../components/DeviceCard.vue';
import { sendFiles } from '../ipc';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import type { DeviceView } from '../types';
import { myIdentity, devices, incomingQueue, sendingPeerFingerprints, systemError } from '../store';

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
    await message(`Failed to send files to ${device.name}:\n${String(e)}`, { title: 'Transfer Failed', kind: 'error' });
  }
};
</script>

<template>
  <div class="nearby-view">
    <div v-if="systemError" class="system-error-banner" @click="systemError = null">
      ⚠️ {{ systemError }} <span style="opacity:0.7;font-size:0.8em">(click to dismiss)</span>
    </div>
    <header class="view-header">
      <h2>Nearby</h2>
      <div class="my-identity" v-if="myIdentity">
        <div class="status-dot"></div>
        <span class="text-muted">{{ myIdentity.device_name }}</span>
        <button @click="emit('openSettings')" class="btn-icon" style="margin-left:8px; cursor: pointer; background: transparent; border: none; font-size: 1.2rem;" title="Settings">⚙️</button>
      </div>
    </header>

    <main class="content">
      <div class="devices-section">
        <p class="text-muted subtitle">Click a device to send files</p>

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
          <div class="radar-ping"></div>
          <p class="text-muted">Looking for nearby devices...</p>
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
}

.view-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 24px 32px;
  background: linear-gradient(to bottom, rgba(15, 17, 21, 0.8), transparent);
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

.incoming-hint {
  position: absolute;
  bottom: 18px;
  right: 24px;
  padding: 8px 12px;
  border-radius: var(--radius-md);
  background: rgba(59, 130, 246, 0.15);
  border: 1px solid rgba(59, 130, 246, 0.35);
  color: #93c5fd;
  font-size: 0.85rem;
}
</style>
