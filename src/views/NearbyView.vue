<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed, watch } from 'vue';
import { open, message } from '@tauri-apps/plugin-dialog';
import PairingImportModal from '../components/PairingImportModal.vue';
import DeviceCard from '../components/DeviceCard.vue';
import {
  confirmTrustedPeerVerification,
  getDiscoveryDiagnostics,
  pairDevice,
  sendFiles,
  setTrustedAlias,
  subscribeRuntimeEvents,
} from '../ipc';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import type { DeviceView, DiscoveryDiagnostics } from '../types';
import type { PairingQrPayload } from '../security';
import {
  myIdentity,
  devices,
  incomingQueue,
  sendingPeerFingerprints,
  externalSharePaths,
  externalShareSource,
  clearExternalShare,
  clearPendingPairingLink,
  pendingPairingLink,
} from '../store';
import { isDeviceOnline } from '../devicePresence';
import { sharedVerificationCode, verificationCodeFromFingerprint } from '../security';

const emit = defineEmits(['openSettings', 'openTransfers']);

const incomingCount = computed(() => incomingQueue.value.length);
const queuedShareCount = computed(() => externalSharePaths.value.length);
const queuedExternalShareKey = computed(() => externalSharePaths.value.join('\u0000'));
const activeDropTargetFp = ref<string | null>(null);
const showTrustConfirm = ref(false);
const trustConfirmBusy = ref(false);
const trustRemember = ref(true);
const trustVerified = ref(false);
const pendingTarget = ref<DeviceView | null>(null);
const pendingPaths = ref<string[]>([]);
const pendingPathsFromExternalShare = ref(false);
const pendingExternalShareKey = ref<string | null>(null);
const showPairingImport = ref(false);
const pairingImportBusy = ref(false);
const pairingImportInitialInput = ref('');
const recentlyPairedFingerprint = ref<string | null>(null);
const discoveryQuickHints = ref<string[]>([]);
let clearRecentlyPairedTimer: ReturnType<typeof setTimeout> | null = null;
const runtimeUnlistens: Array<() => void> = [];

const localVerificationCode = computed(() =>
  myIdentity.value ? verificationCodeFromFingerprint(myIdentity.value.fingerprint) : '',
);
const visibleDevices = computed(() => {
  const highlightedFp = recentlyPairedFingerprint.value;
  const indexed = devices.value.map((device, index) => ({ device, index }));
  indexed.sort((left, right) => {
    const leftHighlighted = left.device.fingerprint === highlightedFp;
    const rightHighlighted = right.device.fingerprint === highlightedFp;
    if (leftHighlighted !== rightHighlighted) {
      return leftHighlighted ? -1 : 1;
    }
    return left.index - right.index;
  });
  return indexed.map(({ device }) => device);
});

const discoveryScopeHint = computed(() => {
  const matchingHint = discoveryQuickHints.value.find((hint) => {
    const normalized = hint.toLowerCase();
    return (
      normalized.includes('connect by address') ||
      normalized.includes('vlan') ||
      normalized.includes('subnet')
    );
  });
  if (matchingHint) {
    return matchingHint;
  }
  return "Automatic discovery is only expected to work on the same LAN/subnet. If your devices are separated by VLANs/subnets, or multicast is filtered, open Transfers and use Connect by Address.";
});

watch(
  pendingPairingLink,
  (value) => {
    if (!value) {
      return;
    }
    pairingImportInitialInput.value = value;
    showPairingImport.value = true;
  },
  { immediate: true },
);

let dragDropUnlisten: (() => void) | null = null;

const loadDiscoveryHints = async () => {
  try {
    const diagnostics: DiscoveryDiagnostics = await getDiscoveryDiagnostics();
    discoveryQuickHints.value = diagnostics.quick_hints ?? [];
  } catch (error) {
    console.debug('Failed to load Nearby discovery diagnostics', error);
  }
};

const canSendToDevice = (device: DeviceView) => isDeviceOnline(device);
const verificationCode = (fingerprint: string) =>
  myIdentity.value
    ? sharedVerificationCode(myIdentity.value.fingerprint, fingerprint)
    : verificationCodeFromFingerprint(fingerprint);

const showUnavailableDeviceMessage = async (device: DeviceView) => {
  await message(
    `${device.name} is currently unavailable for transfer.\nBring both devices online and on the same LAN, then retry.`,
    { title: 'Device unavailable', kind: 'warning' },
  );
};

onMounted(async () => {
  await loadDiscoveryHints();
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
    if (!canSendToDevice(target)) {
      await showUnavailableDeviceMessage(target);
      return;
    }

    await prepareAndSend(paths, target);
  });

  runtimeUnlistens.push(
    await subscribeRuntimeEvents(
      [
        'device_discovered',
        'device_updated',
        'device_lost',
        'daemon_control_plane_recovered',
        'system_error',
      ],
      () => {
        void loadDiscoveryHints();
      },
    ),
  );
});

onUnmounted(() => {
  if (dragDropUnlisten) dragDropUnlisten();
  for (const unlisten of runtimeUnlistens) {
    unlisten();
  }
  runtimeUnlistens.length = 0;
  if (clearRecentlyPairedTimer) {
    clearTimeout(clearRecentlyPairedTimer);
    clearRecentlyPairedTimer = null;
  }
});

function markRecentlyPaired(fingerprint: string) {
  recentlyPairedFingerprint.value = fingerprint;
  if (clearRecentlyPairedTimer) {
    clearTimeout(clearRecentlyPairedTimer);
  }
  clearRecentlyPairedTimer = setTimeout(() => {
    recentlyPairedFingerprint.value = null;
    clearRecentlyPairedTimer = null;
  }, 12_000);
}

const handleDeviceClick = async (device: DeviceView) => {
  if (!canSendToDevice(device)) {
    await showUnavailableDeviceMessage(device);
    return;
  }
  if (queuedShareCount.value > 0) {
    await prepareAndSend([...externalSharePaths.value], device, { fromExternalShare: true });
    return;
  }
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
  if (sendingPeerFingerprints.value.has(device.fingerprint)) return false;

  try {
    await sendFiles(device.fingerprint, paths);
    return true;
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
    return false;
  }
};

const maybeClearExternalShare = (shareKey: string | null) => {
  if (!shareKey) return;
  if (queuedExternalShareKey.value === shareKey) {
    clearExternalShare();
  }
};

const prepareAndSend = async (
  paths: string[],
  device: DeviceView,
  options: { fromExternalShare?: boolean; externalShareKey?: string | null } = {},
) => {
  const fromExternalShare = options.fromExternalShare === true;
  const externalShareKey = fromExternalShare
    ? (options.externalShareKey ?? queuedExternalShareKey.value)
    : null;
  if (device.trusted) {
    const sent = await executeSend(paths, device);
    if (sent && fromExternalShare) {
      maybeClearExternalShare(externalShareKey);
    }
    return;
  }

  pendingTarget.value = device;
  pendingPaths.value = [...paths];
  pendingPathsFromExternalShare.value = fromExternalShare;
  pendingExternalShareKey.value = externalShareKey;
  trustRemember.value = true;
  trustVerified.value = false;
  showTrustConfirm.value = true;
};

const closeTrustConfirm = () => {
  if (trustConfirmBusy.value) return;
  showTrustConfirm.value = false;
  pendingTarget.value = null;
  pendingPaths.value = [];
  pendingPathsFromExternalShare.value = false;
  pendingExternalShareKey.value = null;
  trustVerified.value = false;
};

const forceCloseTrustConfirm = () => {
  showTrustConfirm.value = false;
  pendingTarget.value = null;
  pendingPaths.value = [];
  pendingPathsFromExternalShare.value = false;
  pendingExternalShareKey.value = null;
};

const confirmTrustAndSend = async () => {
  const device = pendingTarget.value;
  if (!device) return;
  if (!trustVerified.value) return;

  trustConfirmBusy.value = true;
  try {
    if (trustRemember.value) {
      await pairDevice(device.fingerprint);
      markRecentlyPaired(device.fingerprint);
    }
    const sent = await executeSend([...pendingPaths.value], device);
    if (sent) {
      if (pendingPathsFromExternalShare.value) {
        maybeClearExternalShare(pendingExternalShareKey.value);
      }
      forceCloseTrustConfirm();
    }
  } finally {
    trustConfirmBusy.value = false;
  }
};

function closePairingImport() {
  if (pairingImportBusy.value) return;
  showPairingImport.value = false;
  pairingImportInitialInput.value = '';
  clearPendingPairingLink();
}

async function importPairingLink({
  payload,
  mutualConfirmationRequested,
}: {
  payload: PairingQrPayload;
  mutualConfirmationRequested: boolean;
}) {
  pairingImportBusy.value = true;
  try {
    await pairDevice(payload.fingerprint);
    await setTrustedAlias(payload.fingerprint, payload.device_name);
    await confirmTrustedPeerVerification(
      payload.fingerprint,
      payload.trust_model === 'signed_link' && payload.signature_verified
        ? 'signed_pairing_link'
        : 'legacy_unsigned_link',
      mutualConfirmationRequested,
    );
    markRecentlyPaired(payload.fingerprint);
    showPairingImport.value = false;
    pairingImportInitialInput.value = '';
    clearPendingPairingLink();
    await message(
      mutualConfirmationRequested
        ? `Trusted ${payload.device_name} with mutual confirmation recorded.`
        : `Trusted ${payload.device_name}. Mutual confirmation can be completed later after both sides compare the shared pair code.`,
      { title: 'Pairing Complete', kind: 'info' },
    );
  } catch (error) {
    console.error('Failed to import pairing link from Nearby', error);
    await message(String(error), { title: 'Pairing Failed', kind: 'error' });
  } finally {
    pairingImportBusy.value = false;
  }
}
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
        <button @click="showPairingImport = true" class="btn btn-secondary">Import Pairing</button>
        <button @click="emit('openSettings')" class="btn btn-secondary">Settings</button>
      </div>
    </header>

    <main class="content">
      <div v-if="queuedShareCount > 0" class="share-banner">
        <div class="share-copy">
          <strong>{{ queuedShareCount }} shared item{{ queuedShareCount > 1 ? 's' : '' }}</strong>
          <span class="text-muted">
            {{ externalShareSource ? `Source: ${externalShareSource}. ` : '' }}Choose a nearby device to send them.
          </span>
        </div>
        <button class="btn btn-secondary" @click="clearExternalShare">Clear</button>
      </div>
      <div class="devices-section">
        <div class="devices-grid" v-if="devices.length > 0">
          <DeviceCard
            v-for="device in visibleDevices"
            :key="device.fingerprint"
            :device="device"
            :isSending="sendingPeerFingerprints.has(device.fingerprint)"
            :disabled="!canSendToDevice(device)"
            :highlighted="device.fingerprint === recentlyPairedFingerprint"
            @click="handleDeviceClick(device)"
            @drag-target-enter="handleDragTargetEnter(device)"
            @drag-target-leave="handleDragTargetLeave(device)"
          />
        </div>

        <div class="empty-state" v-else>
          <p>Scanning local network</p>
          <p class="text-muted">Keep both devices awake and on the same Wi-Fi/LAN.</p>
          <p class="empty-detail text-muted">{{ discoveryScopeHint }}</p>
          <div class="empty-actions">
            <button class="btn btn-secondary" @click="emit('openTransfers')">Open Transfers</button>
            <button class="btn btn-secondary" @click="emit('openSettings')">Open Diagnostics</button>
          </div>
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
        <p class="verification-line" v-if="pendingTarget">
          Shared verification code: <code>{{ verificationCode(pendingTarget.fingerprint) }}</code>
        </p>
        <label class="remember-row">
          <input type="checkbox" v-model="trustVerified" :disabled="trustConfirmBusy" />
          <span>I compared this shared code on both devices</span>
        </label>
        <label class="remember-row">
          <input type="checkbox" v-model="trustRemember" :disabled="trustConfirmBusy" />
          <span>Pair and remember this device</span>
        </label>
        <div class="dialog-actions">
          <button class="btn btn-secondary" :disabled="trustConfirmBusy" @click="closeTrustConfirm">
            Cancel
          </button>
          <button class="btn btn-primary" :disabled="trustConfirmBusy || !trustVerified" @click="confirmTrustAndSend">
            {{ trustConfirmBusy ? "Sending..." : "Confirm and Send" }}
          </button>
        </div>
      </section>
    </div>

    <PairingImportModal
      :open="showPairingImport"
      :local-fingerprint="myIdentity?.fingerprint || ''"
      :local-verification-code="localVerificationCode"
      :initial-input="pairingImportInitialInput"
      :busy="pairingImportBusy"
      @close="closePairingImport"
      @confirm="importPairingLink"
    />
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

.share-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;
  padding: 10px 12px;
  border-radius: 12px;
  border: 1px solid color-mix(in srgb, var(--accent) 20%, var(--border-subtle));
  background: color-mix(in srgb, var(--accent) 6%, #fff);
}

.share-copy {
  display: flex;
  flex-direction: column;
  gap: 4px;
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

.empty-detail {
  max-width: 520px;
  text-align: center;
  line-height: 1.45;
}

.empty-actions {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
  justify-content: center;
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

.fingerprint-line,
.verification-line {
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
