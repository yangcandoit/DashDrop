<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue';
import { message } from '@tauri-apps/plugin-dialog';
import PairingImportModal from '../components/PairingImportModal.vue';
import type { LocalIdentity, TrustedPeer } from '../types';
import type { PairingQrPayload } from '../security';
import {
  confirmTrustedPeerVerification,
  getLocalIdentity,
  getTrustedPeers,
  pairDevice,
  setTrustedAlias,
  subscribeRuntimeEvents,
  unpairDevice,
} from '../ipc';
import { clearPendingPairingLink, devices, pendingPairingLink } from '../store';
import ConfirmModal from '../components/ConfirmModal.vue';
import { isDeviceOnline } from '../devicePresence';
import { verificationCodeFromFingerprint } from '../security';

const emit = defineEmits(['openSettings']);

const peers = ref<TrustedPeer[]>([]);
const loading = ref(true);
const aliasDrafts = ref<Record<string, string>>({});
const editingAliasFp = ref<string | null>(null);
const savingAlias = ref<Record<string, boolean>>({});
const unpairTarget = ref<TrustedPeer | null>(null);
const loadError = ref<string | null>(null);
const actionError = ref<string | null>(null);
const localIdentity = ref<LocalIdentity | null>(null);
const showPairingImport = ref(false);
const pairingImportBusy = ref(false);
const pairingImportInitialInput = ref('');
const unlistens: Array<() => void> = [];

const showActionError = async (summary: string, error: unknown) => {
  const detail = String(error || '').trim();
  actionError.value = detail ? `${summary} ${detail}` : summary;
  try {
    await message(actionError.value, { title: 'Trusted Devices Error', kind: 'error' });
  } catch (dialogError) {
    console.debug('Unable to show trusted-devices error dialog', dialogError);
  }
};

const onlineFingerprints = computed(() => {
  const set = new Set<string>();
  for (const d of devices.value) {
    if (isDeviceOnline(d)) {
      set.add(d.fingerprint);
    }
  }
  return set;
});

const isOnline = (fp: string) => onlineFingerprints.value.has(fp);
const localVerificationCode = computed(() =>
  localIdentity.value ? verificationCodeFromFingerprint(localIdentity.value.fingerprint) : '',
);

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

const loadPeers = async () => {
  loading.value = true;
  try {
    loadError.value = null;
    peers.value = await getTrustedPeers();
    aliasDrafts.value = Object.fromEntries(
      peers.value.map((peer) => [peer.fingerprint, peer.alias || '']),
    );
  } catch (e) {
    console.error('Failed to load trusted devices', e);
    loadError.value = 'Unable to load trusted devices right now.';
  } finally {
    loading.value = false;
  }
};

const aliasLabel = (peer: TrustedPeer) => peer.alias?.trim() || 'No alias';

const startAliasEdit = async (peer: TrustedPeer) => {
  editingAliasFp.value = peer.fingerprint;
  if (aliasDrafts.value[peer.fingerprint] === undefined) {
    aliasDrafts.value[peer.fingerprint] = peer.alias || '';
  }
  await nextTick();
  const el = document.getElementById(`trusted-alias-${peer.fingerprint}`) as HTMLInputElement | null;
  el?.focus();
  el?.select();
};

const cancelAliasEdit = () => {
  editingAliasFp.value = null;
};

const commitAlias = async (peer: TrustedPeer) => {
  if (savingAlias.value[peer.fingerprint]) return;
  const next = aliasDrafts.value[peer.fingerprint]?.trim() || null;
  const prev = peer.alias?.trim() || null;
  editingAliasFp.value = null;
  if (next === prev) return;

  savingAlias.value[peer.fingerprint] = true;
  try {
    actionError.value = null;
    await setTrustedAlias(peer.fingerprint, next);
    const idx = peers.value.findIndex((p) => p.fingerprint === peer.fingerprint);
    if (idx !== -1) {
      peers.value[idx] = { ...peers.value[idx], alias: next };
    }
  } catch (e) {
    aliasDrafts.value[peer.fingerprint] = prev || '';
    await showActionError('Unable to update this alias.', e);
  } finally {
    savingAlias.value[peer.fingerprint] = false;
  }
};

const openUnpairDialog = (peer: TrustedPeer) => {
  unpairTarget.value = peer;
};

const closeUnpairDialog = () => {
  unpairTarget.value = null;
};

const removePeer = async () => {
  if (!unpairTarget.value) return;
  const target = unpairTarget.value;
  unpairTarget.value = null;

  const previous = peers.value.slice();
  peers.value = peers.value.filter((p) => p.fingerprint !== target.fingerprint);
  delete aliasDrafts.value[target.fingerprint];

  try {
    actionError.value = null;
    await unpairDevice(target.fingerprint);
  } catch (e) {
    console.error('Unpair failed', e);
    peers.value = previous;
    await loadPeers();
    await showActionError('Unable to unpair this device.', e);
  }
};

const formatPairedAt = (unix: number) => {
  if (!unix) return 'Unknown';
  return new Date(unix * 1000).toLocaleString();
};

const formatLastUsedAt = (unix?: number | null) => {
  if (!unix) return 'Never';
  return new Date(unix * 1000).toLocaleString();
};

const formatTrustLevel = (peer: TrustedPeer) => {
  switch (peer.trust_level) {
    case 'mutual_confirmed':
      return 'Mutual confirmed';
    case 'signed_link_verified':
      return 'Signed link verified';
    case 'frozen':
      return 'Frozen';
    default:
      return 'Legacy paired';
  }
};

const trustBadgeClass = (peer: TrustedPeer) => {
  switch (peer.trust_level) {
    case 'mutual_confirmed':
      return 'trust-badge trust-strong';
    case 'signed_link_verified':
      return 'trust-badge trust-medium';
    case 'frozen':
      return 'trust-badge trust-frozen';
    default:
      return 'trust-badge trust-legacy';
  }
};

onMounted(async () => {
  await Promise.all([
    loadPeers(),
    getLocalIdentity()
      .then((identity) => {
        localIdentity.value = identity;
      })
      .catch((error) => {
        console.error('Failed to load local identity for trusted devices', error);
      }),
  ]);

  unlistens.push(
    await subscribeRuntimeEvents(['trusted_peer_updated', 'daemon_control_plane_recovered', 'daemon_event_feed_resync_required'], () => {
      void loadPeers();
    }),
  );
});

onUnmounted(() => {
  for (const unlisten of unlistens) {
    unlisten();
  }
  unlistens.length = 0;
});

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
    actionError.value = null;
    await pairDevice(payload.fingerprint);
    await setTrustedAlias(payload.fingerprint, payload.device_name);
    await confirmTrustedPeerVerification(
      payload.fingerprint,
      payload.trust_model === 'signed_link' && payload.signature_verified
        ? 'signed_pairing_link'
        : 'legacy_unsigned_link',
      mutualConfirmationRequested,
    );
    showPairingImport.value = false;
    pairingImportInitialInput.value = '';
    clearPendingPairingLink();
    await loadPeers();
    await message(
      mutualConfirmationRequested
        ? `Trusted ${payload.device_name} with mutual confirmation recorded.`
        : `Trusted ${payload.device_name}. Mutual confirmation can be completed later after both sides compare the shared pair code.`,
      { title: 'Pairing Complete', kind: 'info' },
    );
  } catch (error) {
    console.error('Failed to import pairing link from trusted devices', error);
    await showActionError('Unable to import this pairing link.', error);
  } finally {
    pairingImportBusy.value = false;
  }
}
</script>

<template>
  <div class="view-container animate-fade-in">
    <header class="view-header">
      <div class="title-wrap">
        <h2>Trusted Devices</h2>
        <p class="text-muted">Paired peers and aliases</p>
      </div>
      <div class="header-actions">
        <button class="btn btn-secondary" @click="showPairingImport = true" :disabled="!localIdentity">
          Import Pairing
        </button>
        <button class="btn btn-secondary" @click="emit('openSettings')">Settings</button>
      </div>
    </header>

    <main class="content">
      <div v-if="loadError" class="error-banner">
        <span>{{ loadError }}</span>
        <button class="btn btn-secondary" @click="loadPeers">Retry</button>
      </div>
      <div v-else-if="actionError" class="error-banner">
        <span>{{ actionError }}</span>
        <button class="btn btn-secondary" @click="actionError = null">Dismiss</button>
      </div>
      <div class="empty-state" v-if="loading">
        <p class="text-muted">Loading trusted devices...</p>
      </div>

      <div class="empty-state" v-else-if="peers.length === 0">
        <svg class="empty-illus" viewBox="0 0 140 90" aria-hidden="true">
          <rect x="8" y="20" width="48" height="56" rx="10" />
          <rect x="84" y="14" width="48" height="62" rx="10" />
          <path d="M59 48h20" />
          <path d="M66 41l-7 7 7 7" />
          <path d="M72 41l7 7-7 7" />
        </svg>
        <p class="text-muted">No trusted devices yet.</p>
        <button class="btn btn-primary" @click="showPairingImport = true" :disabled="!localIdentity">
          Import or Scan Pairing
        </button>
      </div>

      <TransitionGroup v-else name="trusted-list" tag="div" class="trusted-list">
        <article class="trusted-card" v-for="peer in peers" :key="peer.fingerprint">
          <div class="meta">
            <div class="name-row">
              <div class="name">{{ peer.name }}</div>
              <span :class="trustBadgeClass(peer)">{{ formatTrustLevel(peer) }}</span>
              <span class="online-chip" :class="isOnline(peer.fingerprint) ? 'online' : 'offline'">
                <span class="dot"></span>
                {{ isOnline(peer.fingerprint) ? 'Online' : 'Offline' }}
              </span>
            </div>
            <div class="fingerprint text-muted">{{ peer.fingerprint }}</div>
            <div class="paired-at text-muted">Paired: {{ formatPairedAt(peer.paired_at) }}</div>
            <div class="paired-at text-muted">Last used: {{ formatLastUsedAt(peer.last_used_at) }}</div>
            <div v-if="peer.trust_level === 'signed_link_verified'" class="paired-at text-muted">
              Verified via signed link. Waiting for both sides to compare the shared pair code before this becomes mutual.
            </div>
            <div v-if="peer.mutual_confirmed_at" class="paired-at text-muted">
              Mutual confirmation: {{ formatPairedAt(peer.mutual_confirmed_at) }}
            </div>
            <div v-if="peer.trust_level === 'frozen'" class="paired-at trust-warning">
              Frozen{{ peer.frozen_at ? `: ${formatPairedAt(peer.frozen_at)}` : '' }}
            </div>
            <div v-if="peer.trust_level === 'frozen' && peer.freeze_reason" class="paired-at trust-warning">
              {{ peer.freeze_reason }}
            </div>

            <div class="alias-row" v-if="editingAliasFp === peer.fingerprint">
              <input
                :id="`trusted-alias-${peer.fingerprint}`"
                v-model="aliasDrafts[peer.fingerprint]"
                class="alias-input"
                type="text"
                placeholder="Alias"
                @keyup.enter="commitAlias(peer)"
                @blur="commitAlias(peer)"
              />
              <button class="btn btn-secondary" @click="cancelAliasEdit">Cancel</button>
            </div>

            <div class="alias-row" v-else>
              <span class="alias-text">Alias: {{ aliasLabel(peer) }}</span>
              <button class="btn btn-secondary" @click="startAliasEdit(peer)">Edit</button>
            </div>
          </div>
          <button class="btn btn-secondary" @click="openUnpairDialog(peer)">Unpair</button>
        </article>
      </TransitionGroup>
    </main>

    <ConfirmModal
      :open="Boolean(unpairTarget)"
      title="Unpair Device"
      :message="unpairTarget ? `Remove trust for ${unpairTarget.name}? You will need to verify fingerprint again before sensitive transfers.` : ''"
      confirm-text="Unpair"
      cancel-text="Keep Paired"
      tone="danger"
      @confirm="removePeer"
      @cancel="closeUnpairDialog"
    />

    <PairingImportModal
      :open="showPairingImport"
      :local-fingerprint="localIdentity?.fingerprint || ''"
      :local-verification-code="localVerificationCode"
      :initial-input="pairingImportInitialInput"
      :busy="pairingImportBusy"
      @close="closePairingImport"
      @confirm="importPairingLink"
    />
  </div>
</template>

<style scoped>
.view-container {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  position: relative;
  background: var(--surface);
}

.view-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 20px 22px 12px;
  border-bottom: 1px solid var(--border-subtle);
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.title-wrap {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.content {
  flex: 1;
  padding: 14px 22px 22px;
  overflow-y: auto;
}

.error-banner {
  margin-bottom: 12px;
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

.empty-state {
  height: 240px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  align-items: center;
  justify-content: center;
  background: var(--surface-muted);
  border: 1px solid var(--border-subtle);
  border-radius: 12px;
}

.empty-illus {
  width: 132px;
  height: 84px;
  stroke: color-mix(in srgb, var(--accent) 65%, #8a7a63);
  stroke-width: 2.2;
  fill: color-mix(in srgb, var(--surface-muted) 50%, #fff);
}

.trusted-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.trusted-card {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  padding: 12px;
  border-radius: 12px;
  border: 1px solid var(--border-subtle);
  background: #fff;
}

.name-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.name {
  font-weight: 600;
}

.trust-badge {
  display: inline-flex;
  align-items: center;
  padding: 3px 8px;
  border-radius: 999px;
  font-size: 0.76rem;
  font-weight: 700;
}

.trust-strong {
  background: rgba(46, 125, 50, 0.12);
  color: #2f6c31;
}

.trust-medium {
  background: rgba(184, 129, 34, 0.12);
  color: #8a5d16;
}

.trust-legacy {
  background: rgba(100, 96, 90, 0.12);
  color: #5a554e;
}

.trust-frozen {
  background: rgba(157, 58, 51, 0.12);
  color: #8f2d2a;
}

.trust-warning {
  color: #8f2d2a;
}

.online-chip {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 0.7rem;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  border-radius: 999px;
  border: 1px solid var(--border-subtle);
  padding: 2px 8px;
}

.online-chip .dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
}

.online-chip.online {
  color: #2f6b52;
  border-color: rgba(47, 107, 82, 0.35);
  background: rgba(47, 107, 82, 0.08);
}

.online-chip.offline {
  color: var(--text-muted);
  background: #f2f2f7;
}

.fingerprint {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
  font-size: 0.75rem;
}

.paired-at {
  margin-top: 4px;
  font-size: 0.8rem;
}

.alias-row {
  margin-top: 8px;
  display: flex;
  gap: 8px;
  align-items: center;
}

.alias-input {
  min-width: 220px;
  padding: 6px 8px;
  border-radius: 9px;
  border: 1px solid var(--border-subtle);
  background: rgba(255, 255, 255, 0.82);
  color: var(--text-secondary);
}

.alias-text {
  font-size: 0.84rem;
  color: var(--text-secondary);
}

.trusted-list-enter-active,
.trusted-list-leave-active {
  transition: all 220ms ease;
}

.trusted-list-enter-from,
.trusted-list-leave-to {
  opacity: 0;
  transform: translateY(8px);
}

@media (max-width: 860px) {
  .view-header {
    flex-direction: column;
    align-items: flex-start;
    gap: 10px;
  }

  .header-actions {
    width: 100%;
    justify-content: flex-start;
    flex-wrap: wrap;
  }

  .error-banner {
    flex-direction: column;
    align-items: flex-start;
  }

  .trusted-card {
    flex-direction: column;
    align-items: flex-start;
  }

  .alias-row {
    flex-wrap: wrap;
  }
}
</style>
