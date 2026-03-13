<script setup lang="ts">
import { computed, ref, onMounted, onUnmounted } from 'vue';
import {
  getTransferHistory,
  openTransferFolder,
  subscribeRuntimeEvents,
} from '../ipc';
import type { TransferView } from '../types';

const emit = defineEmits(['openSettings']);

const history = ref<TransferView[]>([]);
const loading = ref(true);
const loadError = ref<string | null>(null);
const unlistens: Array<() => void> = [];
const peerFilter = ref('');
const directionFilter = ref<'All' | 'Send' | 'Receive'>('All');
const statusFilter = ref<'All' | TransferView['status']>('All');
const timeFilter = ref<'All' | '24h' | '7d' | '30d'>('All');

const load = async () => {
  try {
    loading.value = true;
    loadError.value = null;
    history.value = await getTransferHistory(50, 0);
  } catch (e) {
    console.error("Failed to load history", e);
    loadError.value = 'Unable to load transfer history right now.';
  } finally {
    loading.value = false;
  }
};

onMounted(load);
onMounted(async () => {
  unlistens.push(
    await subscribeRuntimeEvents(
      [
        'daemon_control_plane_recovered',
        'transfer_complete',
        'transfer_partial',
        'transfer_rejected',
        'transfer_cancelled_by_sender',
        'transfer_cancelled_by_receiver',
        'transfer_failed',
        'daemon_event_feed_resync_required',
      ],
      () => {
        void load();
      },
    ),
  );
});

onUnmounted(() => {
  for (const unlisten of unlistens) {
    unlisten();
  }
  unlistens.length = 0;
});

const openFolder = (id: string) => {
  openTransferFolder(id).catch((e) => {
    console.error("Failed to open folder", e);
    loadError.value = 'Unable to open the transfer folder.';
  });
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
      <div class="title-wrap">
        <h2>History</h2>
        <p class="text-muted">Recent completed and failed transfers</p>
      </div>
      <button class="btn btn-secondary" @click="emit('openSettings')">Settings</button>
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
      <div v-if="loadError" class="error-banner">
        <span>{{ loadError }}</span>
        <button class="btn btn-secondary" @click="load">Retry</button>
      </div>
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
              class="btn btn-secondary folder-btn">
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

.filters {
  margin-bottom: 12px;
  display: grid;
  grid-template-columns: 1fr 160px 160px 220px;
  gap: 10px;
}

.filter-input,
.filter-select {
  width: 100%;
  min-height: 38px;
  padding: 8px 10px;
  border-radius: 10px;
  border: 1px solid var(--border-subtle);
  background: #fff;
  color: var(--text-secondary);
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
  height: 200px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--surface-muted);
  border: 1px solid var(--border-subtle);
  border-radius: 12px;
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
  gap: 14px;
  padding: 12px;
  background: #fff;
  border: 1px solid var(--border-subtle);
  border-radius: 12px;
}

.card-left {
  display: flex;
  align-items: center;
  gap: 10px;
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
  font-size: 0.72rem;
  padding: 4px 8px;
  border-radius: 999px;
  border: 1px solid var(--border-subtle);
  background: #f2f2f7;
  font-weight: 600;
  letter-spacing: 0.02em;
}

.status-badge.completed { background: rgba(47, 107, 82, 0.12); color: #2f6b52; }
.status-badge.failed { background: rgba(157, 58, 51, 0.12); color: #9d3a33; }
.status-badge.cancelledbyuser,
.status-badge.cancelledbysender,
.status-badge.cancelledbyreceiver,
.status-badge.rejected { background: rgba(178, 106, 0, 0.12); color: #8a5300; }
.status-badge.partialcompleted { background: rgba(178, 106, 0, 0.12); color: #8a5300; }

.folder-btn {
  min-height: 28px;
  padding: 4px 8px;
  font-size: 0.75rem;
}

@media (max-width: 960px) {
  .filters {
    grid-template-columns: 1fr;
  }

  .error-banner {
    flex-direction: column;
    align-items: flex-start;
  }

  .view-header {
    flex-direction: column;
    align-items: flex-start;
    gap: 10px;
  }

  .history-card {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
