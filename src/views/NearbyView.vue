<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed } from 'vue';
import { open, message } from '@tauri-apps/plugin-dialog';
import DeviceCard from '../components/DeviceCard.vue';
import { pairDevice, sendFiles } from '../ipc';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import type { DeviceView } from '../types';
import { myIdentity, devices, incomingQueue, sendingPeerFingerprints } from '../store';

const emit = defineEmits(['openSettings']);

const incomingCount = computed(() => incomingQueue.value.length);
const activeDropTargetFp = ref<string | null>(null);
const showTrustConfirm = ref(false);
const trustConfirmBusy = ref(false);
const trustRemember = ref(true);
const pendingTarget = ref<DeviceView | null>(null);
const pendingPaths = ref<string[]>([]);

let dragDropUnlisten: (() => void) | null = null;

onMounted(async () => {
  dragDropUnlisten = await getCurrentWebview().onDragDropEvent(async (event) => {
    if (event.payload.type !== 'drop') return;

    const paths = event.payload.paths;
    const targetFp = activeDropTargetFp.value;
    activeDropTargetFp.value = null;

    if (!targetFp) {
      await message('Drop files directly on a device card.', { title: 'No target device', kind: 'warning' });
      return;
    }

    const target = devices.value.find((d) => d.fingerprint === targetFp);
    if (!target) {
      await message('Selected device is no longer available.', { title: 'Device unavailable', kind: 'warning' });
      return;
    }

    await prepareAndSend(paths, target);
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
    await prepareAndSend(paths, device);
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

const executeSend = async (paths: string[], device: DeviceView) => {
  if (sendingPeerFingerprints.value.has(device.fingerprint)) return;

  try {
    await sendFiles(device.fingerprint, paths);
  } catch (e: unknown) {
    console.error('Failed to send files:', e);
    const detail = String(e || '').toLowerCase();
    let userReason = 'Unknown transport error.';
    let extraHint = '';
    if (detail.includes('all connection attempts failed')) {
      userReason = 'Peer is unreachable on all known addresses.';
      const firstAttempt = String(e || '')
        .split('(')[1]
        ?.split('|')[0]
        ?.replace(/\)$/, '')
        ?.trim();
      if (firstAttempt) {
        extraHint = `First failed endpoint: ${firstAttempt}`;
      }
      if (detail.includes('invalid remote address')) {
        userReason = 'Peer only exposed an unusable IPv6 link-local address.';
        extraHint = 'Wait for discovery refresh, or use Connect by Address with a LAN IPv4 endpoint.';
      } else if (detail.includes('connection refused')) {
        extraHint = 'Peer app may not be listening, or firewall is blocking the receiver port.';
      } else if (detail.includes('timed out')) {
        extraHint = 'Network path exists but peer did not respond in time.';
      }
    } else if (detail.includes('quic handshake')) {
      userReason = 'Secure handshake failed.';
    } else if (detail.includes('identity mismatch')) {
      userReason = 'Identity verification failed.';
    } else if (detail.includes('timeout')) {
      userReason = 'Peer did not respond in time.';
    } else if (detail.includes('device has no reachable address')) {
      userReason = 'Peer is discovered but currently has no usable address.';
      extraHint = 'Open Settings and copy diagnostics on both devices for comparison.';
    }
    await message(
      `Failed to send files to ${device.name}.\nReason: ${userReason}${extraHint ? `\n${extraHint}` : ''}\nOpen Transfers or Security Events for details, then retry.`,
      { title: 'Transfer Failed', kind: 'error' },
    );
  }
};

const prepareAndSend = async (paths: string[], device: DeviceView) => {
  if (device.trusted) {
    await executeSend(paths, device);
    return;
  }

  pendingTarget.value = device;
  pendingPaths.value = [...paths];
  trustRemember.value = true;
  showTrustConfirm.value = true;
};

const closeTrustConfirm = () => {
  if (trustConfirmBusy.value) return;
  showTrustConfirm.value = false;
  pendingTarget.value = null;
  pendingPaths.value = [];
};

const forceCloseTrustConfirm = () => {
  showTrustConfirm.value = false;
  pendingTarget.value = null;
  pendingPaths.value = [];
};

const confirmTrustAndSend = async () => {
  const device = pendingTarget.value;
  if (!device) return;

  trustConfirmBusy.value = true;
  try {
    if (trustRemember.value) {
      await pairDevice(device.fingerprint);
    }
    await executeSend([...pendingPaths.value], device);
    forceCloseTrustConfirm();
  } finally {
    trustConfirmBusy.value = false;
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
          <span class="identity-label">This Device</span>
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

    <div v-if="showTrustConfirm" class="dialog-backdrop" @click.self="closeTrustConfirm">
      <section class="dialog-card">
        <h3>Verify Device Before Sending</h3>
        <p class="text-muted dialog-copy" v-if="pendingTarget">
          You are sending to an untrusted device: <strong>{{ pendingTarget.name }}</strong>.
          Confirm this fingerprint out-of-band before continuing.
        </p>
        <p class="fingerprint-line" v-if="pendingTarget">
          Fingerprint: <code>{{ pendingTarget.fingerprint }}</code>
        </p>
        <label class="remember-row">
          <input type="checkbox" v-model="trustRemember" :disabled="trustConfirmBusy" />
          <span>Pair and remember this device</span>
        </label>
        <div class="dialog-actions">
          <button class="btn btn-secondary" :disabled="trustConfirmBusy" @click="closeTrustConfirm">
            Cancel
          </button>
          <button class="btn btn-primary" :disabled="trustConfirmBusy" @click="confirmTrustAndSend">
            {{ trustConfirmBusy ? "Sending..." : "Confirm and Send" }}
          </button>
        </div>
      </section>
    </div>
  </div>
</template>

<style scoped>
.nearby-view {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  background: var(--surface);
}

.view-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  padding: 20px 22px 12px;
  border-bottom: 1px solid var(--border-subtle);
}

.title-wrap {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.subtitle {
  font-size: 0.9rem;
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.my-identity {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 2px;
  padding: 6px 10px;
  border-radius: 10px;
  border: 1px solid var(--border-subtle);
  background: var(--surface-muted);
}

.identity-label {
  font-size: 0.68rem;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-subtle);
}

.identity-name {
  font-size: 0.88rem;
  font-weight: 600;
  color: var(--text-secondary);
}

.content {
  flex: 1;
  padding: 14px 22px 22px;
  overflow-y: auto;
}

.devices-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 10px;
}

.empty-state {
  min-height: 200px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  border: 1px solid var(--border-subtle);
  border-radius: 12px;
  background: var(--surface-muted);
  color: var(--text-secondary);
}

.incoming-hint {
  margin: 0 22px 16px auto;
  padding: 6px 10px;
  border-radius: 999px;
  border: 1px solid var(--border-subtle);
  color: var(--text-muted);
  font-size: 0.76rem;
}

.dialog-backdrop {
  position: absolute;
  inset: 0;
  background: rgba(33, 30, 24, 0.38);
  backdrop-filter: blur(6px);
  display: flex;
  justify-content: center;
  align-items: center;
  padding: 18px;
  z-index: 50;
}

.dialog-card {
  width: min(560px, 100%);
  border-radius: 16px;
  border: 1px solid var(--border-subtle);
  background: #fff;
  box-shadow: var(--shadow-soft);
  padding: 18px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.dialog-copy {
  font-size: 0.9rem;
}

.fingerprint-line {
  border: 1px solid var(--border-subtle);
  border-radius: 10px;
  background: var(--surface-muted);
  padding: 8px 10px;
  font-size: 0.8rem;
  color: var(--text-secondary);
  overflow-wrap: anywhere;
}

.remember-row {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 0.86rem;
  color: var(--text-secondary);
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
  }

  .header-actions {
    width: 100%;
    justify-content: space-between;
  }

  .devices-grid {
    grid-template-columns: 1fr;
  }

  .dialog-actions {
    width: 100%;
  }

  .dialog-actions .btn {
    flex: 1;
  }
}
</style>
