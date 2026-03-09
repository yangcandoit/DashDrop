<script setup lang="ts">
import { computed, ref, onMounted, onUnmounted } from 'vue';
import {
  getTransferHistory,
  openTransferFolder,
  onTransferCancelledByReceiver,
  onTransferCancelledBySender,
  onTransferComplete,
  onTransferFailed,
  onTransferPartial,
  onTransferRejected,
} from '../ipc';
import type { TransferView } from '../types';

const emit = defineEmits(['openSettings']);

const history = ref<TransferView[]>([]);
const loading = ref(true);
const unlistens: Array<() => void> = [];
const peerFilter = ref('');
const directionFilter = ref<'All' | 'Send' | 'Receive'>('All');
const statusFilter = ref<'All' | TransferView['status']>('All');
const timeFilter = ref<'All' | '24h' | '7d' | '30d'>('All');

const load = async () => {
  try {
    loading.value = true;
    history.value = await getTransferHistory(50, 0);
  } catch (e) {
    console.error("Failed to load history", e);
  } finally {
    loading.value = false;
  }
};

onMounted(load);
onMounted(async () => {
  unlistens.push(
    await onTransferComplete(() => void load()),
    await onTransferPartial(() => void load()),
    await onTransferRejected(() => void load()),
    await onTransferCancelledBySender(() => void load()),
    await onTransferCancelledByReceiver(() => void load()),
    await onTransferFailed(() => void load()),
  );
});

onUnmounted(() => {
  for (const unlisten of unlistens) {
    unlisten();
  }
  unlistens.length = 0;
});

const openFolder = (id: string) => {
  openTransferFolder(id).catch(e => console.error("Failed to open folder", e));
};

const formatSize = (bytes: number) => {
  if (bytes < 1024) return bytes + ' B';
  const kb = bytes / 1024;
  if (kb < 1024) return kb.toFixed(1) + ' KB';
  const mb = kb / 1024;
  if (mb < 1024) return mb.toFixed(1) + ' MB';
  const gb = mb / 1024;
  return gb.toFixed(1) + ' GB';
};

const formatEndedAt = (ts?: number | null) => {
  if (!ts) return 'Unknown time';
  return new Date(ts * 1000).toLocaleString();
};

const statusOptions: Array<'All' | TransferView['status']> = [
  'All',
  'Completed',
  'PartialCompleted',
  'Rejected',
  'CancelledBySender',
  'CancelledByReceiver',
  'Failed',
];

const filteredHistory = computed(() => {
  const keyword = peerFilter.value.trim().toLowerCase();
  const nowSec = Math.floor(Date.now() / 1000);
  const minTs =
    timeFilter.value === '24h'
      ? nowSec - 24 * 3600
      : timeFilter.value === '7d'
        ? nowSec - 7 * 24 * 3600
        : timeFilter.value === '30d'
          ? nowSec - 30 * 24 * 3600
          : 0;
  return history.value.filter((t) => {
    const directionOk = directionFilter.value === 'All' || t.direction === directionFilter.value;
    const statusOk = statusFilter.value === 'All' || t.status === statusFilter.value;
    const timeOk = minTs === 0 || (t.ended_at_unix ?? 0) >= minTs;
    const peerOk =
      keyword.length === 0 ||
      t.peer_name.toLowerCase().includes(keyword) ||
      t.peer_fingerprint.toLowerCase().includes(keyword);
    return directionOk && statusOk && peerOk && timeOk;
  });
});
</script>

<template>
  <div class="view-container animate-fade-in">
    <header class="view-header">
      <h2>History</h2>
      <button class="btn btn-secondary" style="padding: 6px 12px;" @click="emit('openSettings')">⚙️</button>
    </header>
    <main class="content">
      <section class="filters">
        <input
          v-model="peerFilter"
          class="filter-input"
          type="text"
          placeholder="Filter by peer name or fingerprint"
        />
        <select v-model="directionFilter" class="filter-select">
          <option value="All">All Directions</option>
          <option value="Send">Send</option>
          <option value="Receive">Receive</option>
        </select>
        <select v-model="timeFilter" class="filter-select">
          <option value="All">All Time</option>
          <option value="24h">Last 24 hours</option>
          <option value="7d">Last 7 days</option>
          <option value="30d">Last 30 days</option>
        </select>
        <select v-model="statusFilter" class="filter-select">
          <option v-for="status in statusOptions" :key="status" :value="status">
            {{ status === 'All' ? 'All Statuses' : status }}
          </option>
        </select>
      </section>
      <div v-if="loading" class="empty-state">
        <p class="text-muted">Loading history...</p>
      </div>
      <div v-else-if="history.length === 0" class="empty-state">
        <p class="text-muted">No past transfers.</p>
      </div>
      <div v-else-if="filteredHistory.length === 0" class="empty-state">
        <p class="text-muted">No records match current filters.</p>
      </div>
      <div v-else class="history-list">
        <div v-for="t in filteredHistory" :key="t.id" class="history-card">
          <div class="card-left">
            <div class="icon">
              {{ t.direction === 'Send' ? '⬆️' : '⬇️' }}
            </div>
            <div class="details">
              <div class="peer-name">{{ t.direction === 'Send' ? 'To ' : 'From ' }}{{ t.peer_name }}</div>
              <div class="meta text-muted">{{ t.items.length }} files • {{ formatSize(t.total_bytes) }}</div>
              <div class="meta text-muted">{{ formatEndedAt(t.ended_at_unix) }}</div>
              <div class="meta text-muted" v-if="t.error">{{ t.error }}</div>
            </div>
          </div>
          <div class="card-right">
            <span :class="['status-badge', t.status.toLowerCase()]">{{ t.status }}</span>
            <button 
              v-if="t.status === 'Completed' && t.direction === 'Receive'" 
              @click="openFolder(t.id)" 
              class="btn btn-secondary" style="padding: 4px 8px; font-size: 0.8rem;">
              Folder
            </button>
          </div>
        </div>
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
  overflow-y: auto;
}

.filters {
  margin-bottom: 12px;
  display: grid;
  grid-template-columns: 1fr 160px 160px 220px;
  gap: 10px;
}

.filter-input,
.filter-select {
  width: 100%;
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border-light);
  background: var(--bg-surface);
  color: var(--text-primary);
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

.history-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.history-card {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 16px;
  background: var(--bg-surface);
  border: 1px solid var(--border-light);
  border-radius: var(--radius-md);
}

.card-left {
  display: flex;
  align-items: center;
  gap: 16px;
}

.icon {
  font-size: 1.5rem;
  width: 40px;
  height: 40px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(255,255,255,0.05);
  border-radius: 50%;
}

.peer-name {
  font-weight: 500;
  margin-bottom: 4px;
}

.meta {
  font-size: 0.85rem;
}

.card-right {
  display: flex;
  align-items: center;
  gap: 12px;
}

.status-badge {
  font-size: 0.75rem;
  padding: 2px 8px;
  border-radius: 12px;
  background: rgba(255,255,255,0.1);
  font-weight: 500;
}

.status-badge.completed { background: rgba(16, 185, 129, 0.2); color: #34d399; }
.status-badge.failed { background: rgba(239, 68, 68, 0.2); color: #f87171; }
.status-badge.cancelledbyuser, .status-badge.cancelledbysender, .status-badge.cancelledbyreceiver, .status-badge.rejected { background: rgba(245, 158, 11, 0.2); color: #fbbf24; }
.status-badge.partialcompleted { background: rgba(59, 130, 246, 0.2); color: #60a5fa; }

@media (max-width: 960px) {
  .filters {
    grid-template-columns: 1fr;
  }
}
</style>
