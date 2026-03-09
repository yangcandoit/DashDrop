<script setup lang="ts">
import { computed, nextTick, onMounted, ref } from 'vue';
import type { TrustedPeer } from '../types';
import { getTrustedPeers, setTrustedAlias, unpairDevice } from '../ipc';
import { devices } from '../store';
import ConfirmModal from '../components/ConfirmModal.vue';

const emit = defineEmits(['openSettings']);

const peers = ref<TrustedPeer[]>([]);
const loading = ref(true);
const aliasDrafts = ref<Record<string, string>>({});
const editingAliasFp = ref<string | null>(null);
const savingAlias = ref<Record<string, boolean>>({});
const unpairTarget = ref<TrustedPeer | null>(null);

const onlineFingerprints = computed(() => {
  const set = new Set<string>();
  for (const d of devices.value) {
    const alive = Object.keys(d.sessions || {}).length > 0 && d.reachability !== 'offline';
    if (alive) set.add(d.fingerprint);
  }
  return set;
});

const isOnline = (fp: string) => onlineFingerprints.value.has(fp);

const loadPeers = async () => {
  loading.value = true;
  try {
    peers.value = await getTrustedPeers();
    aliasDrafts.value = Object.fromEntries(
      peers.value.map((peer) => [peer.fingerprint, peer.alias || '']),
    );
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
    await setTrustedAlias(peer.fingerprint, next);
    const idx = peers.value.findIndex((p) => p.fingerprint === peer.fingerprint);
    if (idx !== -1) {
      peers.value[idx] = { ...peers.value[idx], alias: next };
    }
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
    await unpairDevice(target.fingerprint);
  } catch (e) {
    console.error('Unpair failed', e);
    peers.value = previous;
    await loadPeers();
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

onMounted(loadPeers);
</script>

<template>
  <div class="view-container animate-fade-in">
    <header class="view-header">
      <div class="title-wrap">
        <h2>Trusted Devices</h2>
        <p class="text-muted">Paired peers and aliases</p>
      </div>
      <button class="btn btn-secondary" @click="emit('openSettings')">Settings</button>
    </header>

    <main class="content">
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
      </div>

      <TransitionGroup v-else name="trusted-list" tag="div" class="trusted-list">
        <article class="trusted-card" v-for="peer in peers" :key="peer.fingerprint">
          <div class="meta">
            <div class="name-row">
              <div class="name">{{ peer.name }}</div>
              <span class="online-chip" :class="isOnline(peer.fingerprint) ? 'online' : 'offline'">
                <span class="dot"></span>
                {{ isOnline(peer.fingerprint) ? 'Online' : 'Offline' }}
              </span>
            </div>
            <div class="fingerprint text-muted">{{ peer.fingerprint }}</div>
            <div class="paired-at text-muted">Paired: {{ formatPairedAt(peer.paired_at) }}</div>
            <div class="paired-at text-muted">Last used: {{ formatLastUsedAt(peer.last_used_at) }}</div>

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

  .trusted-card {
    flex-direction: column;
    align-items: flex-start;
  }

  .alias-row {
    flex-wrap: wrap;
  }
}
</style>
