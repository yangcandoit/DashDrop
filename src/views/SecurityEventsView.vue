<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { getSecurityEvents } from '../ipc';
import type { SecurityEvent } from '../types';

const emit = defineEmits(['openSettings']);

const loading = ref(true);
const events = ref<SecurityEvent[]>([]);

const load = async () => {
  loading.value = true;
  try {
    events.value = await getSecurityEvents(100, 0);
  } catch (e) {
    console.error('Failed to load security events', e);
  } finally {
    loading.value = false;
  }
};

onMounted(load);

const formatTime = (unix: number) => new Date(unix * 1000).toLocaleString();
</script>

<template>
  <div class="view-container animate-fade-in">
    <header class="view-header">
      <h2>Security Events</h2>
      <div style="display:flex; gap:8px;">
        <button class="btn btn-secondary" @click="load">Refresh</button>
        <button class="btn btn-secondary" style="padding: 6px 12px;" @click="emit('openSettings')">⚙️</button>
      </div>
    </header>
    <main class="content">
      <div v-if="loading" class="empty-state">
        <p class="text-muted">Loading security events...</p>
      </div>
      <div v-else-if="events.length === 0" class="empty-state">
        <p class="text-muted">No security events recorded.</p>
      </div>
      <div v-else class="events-list">
        <article v-for="event in events" :key="event.id" class="event-card">
          <div class="event-head">
            <span class="event-type">{{ event.event_type }}</span>
            <span class="event-time text-muted">{{ formatTime(event.occurred_at_unix) }}</span>
          </div>
          <div class="event-meta text-muted">phase: {{ event.phase }}</div>
          <div v-if="event.peer_fingerprint" class="event-meta text-muted">peer: {{ event.peer_fingerprint }}</div>
          <div class="event-reason">{{ event.reason }}</div>
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
  overflow-y: auto;
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

.events-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.event-card {
  padding: 14px;
  border-radius: var(--radius-md);
  border: 1px solid var(--border-light);
  background: var(--bg-surface);
}

.event-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 6px;
}

.event-type {
  font-weight: 600;
}

.event-meta {
  font-size: 0.84rem;
}

.event-reason {
  margin-top: 8px;
}
</style>

