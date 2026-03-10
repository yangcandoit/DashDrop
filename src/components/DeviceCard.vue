<script setup lang="ts">
import { computed, ref } from 'vue';
import type { DeviceView } from '../types';
import { firstUsableAddress, hasAnySession, hasUsableAddress, isDeviceOnline } from '../devicePresence';

const props = defineProps<{
  device: DeviceView;
  isSending?: boolean;
}>();

const emit = defineEmits<{
  (e: 'click'): void;
  (e: 'drag-target-enter'): void;
  (e: 'drag-target-leave'): void;
}>();

const isDragOver = ref(false);

function onDragOver(e: DragEvent) {
  e.preventDefault();
  if (e.dataTransfer && e.dataTransfer.types.includes('Files')) {
    isDragOver.value = true;
  }
}

function onDragEnter(e: DragEvent) {
  e.preventDefault();
  if (e.dataTransfer && e.dataTransfer.types.includes('Files')) {
    isDragOver.value = true;
    emit('drag-target-enter');
  }
}

function onDragLeave(e: DragEvent) {
  e.preventDefault();
  isDragOver.value = false;
  emit('drag-target-leave');
}

function onDrop(e: DragEvent) {
  e.preventDefault();
  isDragOver.value = false;
  emit('drag-target-enter');
}

const initial = props.device.name.charAt(0).toUpperCase();

const timeAgo = (unixSecs: number) => {
  const diff = Math.floor(Date.now() / 1000 - unixSecs);
  if (diff < 60) return 'Just now';
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return new Date(unixSecs * 1000).toLocaleDateString();
};

const getAddr = () => {
  const usableAddr = firstUsableAddress(props.device);
  if (usableAddr) {
    return usableAddr;
  }
  if (hasAnySession(props.device) && !hasUsableAddress(props.device)) {
    if (props.device.reachability === 'offline' || props.device.reachability === 'offline_candidate') {
      return 'Unreachable (address pending)';
    }
    return 'Discovering (address pending)';
  }
  if (props.device.last_seen && props.device.last_seen > 0) {
    return `Last seen ${timeAgo(props.device.last_seen)}`;
  }
  return 'Offline';
};

const isOffline = computed(() => !isDeviceOnline(props.device));
</script>

<template>
  <div
    class="device-card animate-fade-in"
    :class="{ 'drag-over': isDragOver, offline: isOffline }"
    @dragenter="onDragEnter"
    @dragover="onDragOver"
    @dragleave="onDragLeave"
    @drop="onDrop"
    @click="emit('click')"
  >
    <div class="avatar">{{ initial }}</div>

    <div class="info">
      <h3>{{ device.name }}</h3>
      <div class="meta text-muted">
        <span>{{ device.platform }}</span>
        <span class="dot">•</span>
        <span>{{ getAddr() }}</span>
      </div>
    </div>

    <div class="status" v-if="isSending">Sending</div>
    <div class="status trusted" v-else-if="device.trusted">Trusted</div>

    <div class="drag-overlay" v-if="isDragOver">Drop to send</div>
  </div>
</template>

<style scoped>
.device-card {
  position: relative;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px;
  border-radius: 12px;
  border: 1px solid var(--border-subtle);
  background: #fff;
  cursor: pointer;
}

.device-card:hover {
  border-color: var(--border-strong);
}

.device-card.drag-over {
  border-color: color-mix(in srgb, var(--accent) 45%, transparent);
  box-shadow: 0 0 0 2px rgba(0, 113, 227, 0.14);
}

.device-card.offline {
  opacity: 0.74;
}

.avatar {
  width: 40px;
  height: 40px;
  border-radius: 10px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: #f2f2f7;
  border: 1px solid var(--border-subtle);
  color: #3c3c43;
  font-size: 1.05rem;
  font-weight: 700;
}

.info {
  flex: 1;
  min-width: 0;
}

h3 {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  font-size: 1.02rem;
}

.meta {
  margin-top: 2px;
  font-size: 0.83rem;
  display: flex;
  align-items: center;
  gap: 6px;
}

.dot {
  opacity: 0.5;
}

.status {
  font-size: 0.75rem;
  color: var(--text-muted);
  border: 1px solid var(--border-subtle);
  border-radius: 999px;
  padding: 3px 8px;
}

.status.trusted {
  color: var(--success);
  border-color: rgba(47, 125, 50, 0.3);
  background: rgba(47, 125, 50, 0.06);
}

.drag-overlay {
  position: absolute;
  inset: 0;
  border-radius: 12px;
  border: 1px dashed color-mix(in srgb, var(--accent) 55%, transparent);
  background: rgba(255, 255, 255, 0.92);
  display: flex;
  align-items: center;
  justify-content: center;
  color: #005bb5;
  font-size: 0.86rem;
  font-weight: 600;
}
</style>
