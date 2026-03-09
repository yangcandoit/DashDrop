<script setup lang="ts">
import { onMounted, ref } from 'vue';
import type { TrustedPeer } from '../types';
import { getTrustedPeers, setTrustedAlias, unpairDevice } from '../ipc';

const emit = defineEmits(['openSettings']);

const peers = ref<TrustedPeer[]>([]);
const loading = ref(true);
const aliasDrafts = ref<Record<string, string>>({});

const loadPeers = async () => {
  loading.value = true;
  peers.value = await getTrustedPeers();
  aliasDrafts.value = Object.fromEntries(
    peers.value.map((peer) => [peer.fingerprint, peer.alias || ""]),
  );
  loading.value = false;
};

const removePeer = async (fp: string) => {
  const ok = window.confirm("Unpair this trusted device?");
  if (!ok) return;
  await unpairDevice(fp);
  await loadPeers();
};

const saveAlias = async (peer: TrustedPeer) => {
  const next = aliasDrafts.value[peer.fingerprint]?.trim() || null;
  await setTrustedAlias(peer.fingerprint, next);
  await loadPeers();
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
      <h2>Trusted Devices</h2>
      <button class="btn btn-secondary" style="padding: 6px 12px;" @click="emit('openSettings')">⚙️</button>
    </header>
    <main class="content">
      <div class="empty-state" v-if="loading">
        <p class="text-muted">Loading trusted devices...</p>
      </div>
      <div class="empty-state" v-else-if="peers.length === 0">
        <p class="text-muted">No trusted devices yet.</p>
      </div>
      <div v-else class="trusted-list">
        <article class="trusted-card" v-for="peer in peers" :key="peer.fingerprint">
          <div class="meta">
            <div class="name">{{ peer.name }}</div>
            <div class="fingerprint text-muted">{{ peer.fingerprint }}</div>
            <div class="paired-at text-muted">Paired: {{ formatPairedAt(peer.paired_at) }}</div>
            <div class="paired-at text-muted">Last used: {{ formatLastUsedAt(peer.last_used_at) }}</div>
            <div class="alias-row">
              <input
                v-model="aliasDrafts[peer.fingerprint]"
                class="alias-input"
                type="text"
                placeholder="Alias (optional)"
              />
              <button class="btn btn-secondary" @click="saveAlias(peer)">Save Alias</button>
            </div>
          </div>
          <button class="btn btn-secondary" @click="removePeer(peer.fingerprint)">Unpair</button>
        </article>
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
  padding: 14px;
  border-radius: var(--radius-md);
  border: 1px solid var(--border-light);
  background: var(--bg-surface);
}

.name {
  font-weight: 600;
}

.fingerprint {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
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
  border-radius: var(--radius-sm);
  border: 1px solid var(--border-light);
  background: var(--bg-surface);
  color: var(--text-primary);
}
</style>
