<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import type { TransferStatus } from '../types';

const props = defineProps<{
  progress: number; // 0 to 1
  status: TransferStatus;
  bytesReceived: number;
  totalBytes: number;
}>();

const formatBytes = (bytes: number) => {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
};

const progressPct = computed(() => {
  return Math.min(100, Math.max(0, props.progress * 100));
});

const statusText = computed(() => {
  switch (props.status) {
    case 'AwaitingAccept': return 'Waiting for accept...';
    case 'Transferring': return 'Transferring...';
    case 'Complete': return 'Done';
    case 'PartialSuccess': return 'Finished with errors';
    case 'Failed': return 'Failed';
    case 'Cancelled': return 'Cancelled';
    default: return '';
  }
});

const isError = computed(() => props.status === 'Failed' || props.status === 'Cancelled');
const isSuccess = computed(() => props.status === 'Complete');

const speedStr = ref('');
const etaStr = ref('');
let lastBytes = 0;
let lastTime = 0;
let speedHistory: number[] = [];

watch(() => props.bytesReceived, (newBytes) => {
  if (props.status !== 'Transferring') return;
  const now = Date.now();
  if (lastTime === 0 || newBytes < lastBytes) {
    lastTime = now;
    lastBytes = newBytes;
    return;
  }
  const dt = (now - lastTime) / 1000;
  if (dt >= 0.5) {
    const speed = (newBytes - lastBytes) / dt;
    speedHistory.push(speed);
    if (speedHistory.length > 5) speedHistory.shift();
    const avgSpeed = speedHistory.reduce((a, b) => a + b, 0) / speedHistory.length;
    
    speedStr.value = formatBytes(avgSpeed) + '/s';
    
    if (avgSpeed > 0) {
      const remainingBytes = props.totalBytes - newBytes;
      const etaSecs = Math.max(0, Math.round(remainingBytes / avgSpeed));
      if (etaSecs < 60) etaStr.value = `${etaSecs}s left`;
      else {
        const mins = Math.floor(etaSecs / 60);
        const secs = etaSecs % 60;
        etaStr.value = `${mins}m ${secs}s left`;
      }
    }
    
    lastTime = now;
    lastBytes = newBytes;
  }
});

watch(() => props.status, (newStatus) => {
  if (newStatus !== 'Transferring') {
    speedStr.value = '';
    etaStr.value = '';
  }
});

</script>

<template>
  <div class="progress-container">
    <div class="progress-header">
      <div style="display: flex; align-items: baseline; gap: 8px;">
        <span class="status-badge" :class="{ error: isError, success: isSuccess }">
          {{ statusText }}
        </span>
        <span v-if="speedStr && status === 'Transferring'" class="speed-eta text-muted text-sm">
          {{ speedStr }} &bull; {{ etaStr }}
        </span>
      </div>
      <span class="text-muted text-sm" v-if="status === 'Transferring'">
        {{ formatBytes(bytesReceived) }} / {{ formatBytes(totalBytes) }}
      </span>
    </div>
    
    <div class="progress-track" :class="{ error: isError, success: isSuccess }">
      <div 
        class="progress-fill" 
        :style="{ width: `${progressPct}%` }"
        :class="{ active: status === 'Transferring' }"
      ></div>
    </div>
  </div>
</template>

<style scoped>
.progress-container {
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.progress-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.text-sm {
  font-size: 0.85rem;
}

.status-badge {
  font-size: 0.85rem;
  font-weight: 500;
  color: var(--accent-primary);
}

.status-badge.error { color: var(--error); }
.status-badge.success { color: var(--success); }

.progress-track {
  height: 6px;
  background: var(--bg-surface-elevated);
  border-radius: var(--radius-full);
  overflow: hidden;
  position: relative;
}

.progress-fill {
  height: 100%;
  background: var(--accent-gradient);
  border-radius: var(--radius-full);
  transition: width 0.3s ease-out;
}

.progress-track.error .progress-fill {
  background: var(--error);
}

.progress-track.success .progress-fill {
  background: var(--success);
}

/* Striped animation for active transfers */
.progress-fill.active {
  background-image: linear-gradient(
    45deg,
    rgba(255, 255, 255, 0.15) 25%,
    transparent 25%,
    transparent 50%,
    rgba(255, 255, 255, 0.15) 50%,
    rgba(255, 255, 255, 0.15) 75%,
    transparent 75%,
    transparent
  );
  background-size: 1rem 1rem;
  animation: progress-stripes 1s linear infinite;
}

@keyframes progress-stripes {
  from { background-position: 1rem 0; }
  to { background-position: 0 0; }
}
</style>
