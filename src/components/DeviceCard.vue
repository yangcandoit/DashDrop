<script setup lang="ts">
import { ref } from 'vue';
import type { DeviceInfo } from '../types';

const props = defineProps<{
  device: DeviceInfo;
  isSending?: boolean;
}>();

const emit = defineEmits<{
  (e: 'drop'): void;
  (e: 'click'): void;
}>();

const isDragOver = ref(false);

function onDragOver(e: DragEvent) {
  e.preventDefault();
  if (e.dataTransfer && e.dataTransfer.types.includes('Files')) {
    isDragOver.value = true;
  }
}

function onDragLeave(e: DragEvent) {
  e.preventDefault();
  isDragOver.value = false;
}

function onDrop(e: DragEvent) {
  e.preventDefault();
  isDragOver.value = false;
  
  if (!e.dataTransfer) return;
  
  // Note: In Tauri v2 with tauri-plugin-fs, we can't easily get the absolute path 
  // from a standard web DragEvent because browsers protect it.
  // We need to use Tauri's drop event listener on the window instead, OR
  // use the older API if available. For MVP, we'll emit the drop and let 
  // NearbyView handle it via Tauri window events, or standard click-to-select.
  // To keep it simple, we just emit 'drop' so the parent knows *this* card was dropped on.
  // The actual paths will be handled by the parent using @tauri-apps/api/event `tauri://drop`.
  emit('drop'); 
}

// Compute an avatar initial
const initial = props.device.name.charAt(0).toUpperCase();

// Format address
const timeAgo = (unixSecs: number) => {
  const diff = Math.floor(Date.now() / 1000 - unixSecs);
  if (diff < 60) return 'Just now';
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return new Date(unixSecs * 1000).toLocaleDateString();
};

const getAddr = () => {
  const sessions = Object.values(props.device.sessions || {});
  if (sessions.length > 0 && sessions[0].addrs && sessions[0].addrs.length > 0) {
    return sessions[0].addrs[0];
  }
  if (props.device.last_seen && props.device.last_seen > 0) {
    return `Last seen ${timeAgo(props.device.last_seen)}`;
  }
  return "Offline";
};

const isOffline = ref(Object.keys(props.device.sessions || {}).length === 0);
import { watchEffect } from 'vue';
watchEffect(() => {
  isOffline.value = Object.keys(props.device.sessions || {}).length === 0;
});
</script>

<template>
  <div 
    class="device-card glass-panel animate-fade-in"
    :class="{ 'drag-over': isDragOver, 'trusted': device.trusted, 'offline': isOffline }"
    @dragover="onDragOver"
    @dragleave="onDragLeave"
    @drop="onDrop"
    @click="emit('click')"
  >
    <div class="avatar-bg">
      <div class="avatar text-gradient">{{ initial }}</div>
    </div>
    
    <div class="info">
      <h3>{{ device.name }}</h3>
      <div class="meta text-muted">
        <span>{{ device.platform }}</span>
        <span class="dot">•</span>
        <span>{{ getAddr() }}</span>
      </div>
    </div>

    <div class="status" v-if="isSending">
      <div class="spinner-badge">
        <svg class="spinner-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <line x1="12" y1="2" x2="12" y2="6"></line>
          <line x1="12" y1="18" x2="12" y2="22"></line>
          <line x1="4.93" y1="4.93" x2="7.76" y2="7.76"></line>
          <line x1="16.24" y1="16.24" x2="19.07" y2="19.07"></line>
          <line x1="2" y1="12" x2="6" y2="12"></line>
          <line x1="18" y1="12" x2="22" y2="12"></line>
          <line x1="4.93" y1="19.07" x2="7.76" y2="16.24"></line>
          <line x1="16.24" y1="4.93" x2="19.07" y2="7.76"></line>
        </svg>
        Connecting...
      </div>
    </div>
    <div class="status" v-else-if="device.trusted">
      <div class="trusted-badge">
        <svg viewBox="0 0 24 24" fill="none" class="icon" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z" />
        </svg>
        Trusted
      </div>
    </div>
    
    <!-- Drag overlay -->
    <div class="drag-overlay" v-if="isDragOver">
      <div class="drop-text text-gradient">Drop to send</div>
    </div>
  </div>
</template>

<style scoped>
.device-card {
  position: relative;
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 16px;
  border-radius: var(--radius-xl);
  cursor: pointer;
  transition: all 0.3s cubic-bezier(0.4, 0, 0.2, 1);
  overflow: hidden;
}

.device-card:hover {
  transform: translateY(-2px);
  background: var(--bg-surface-hover);
  border-color: var(--border-focus);
}

.device-card.drag-over {
  transform: scale(1.02);
  border-color: var(--accent-primary);
  box-shadow: 0 0 20px rgba(59, 130, 246, 0.2);
}

.device-card.offline {
  opacity: 0.6;
  filter: grayscale(0.8);
}

.device-card.offline:hover {
  opacity: 0.8;
  filter: grayscale(0.5);
}

.avatar-bg {
  width: 48px;
  height: 48px;
  border-radius: 50%;
  background: rgba(255, 255, 255, 0.05);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  border: 1px solid var(--border-light);
}

.avatar {
  font-size: 1.25rem;
  font-weight: 600;
}

.device-card:hover .avatar-bg {
  background: rgba(255, 255, 255, 0.1);
}

.device-card.trusted .avatar-bg {
  border-color: rgba(16, 185, 129, 0.3);
}

.info {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}

h3 {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-bottom: 2px;
}

.meta {
  font-size: 0.85rem;
  display: flex;
  align-items: center;
  gap: 6px;
}

.dot {
  opacity: 0.5;
  font-size: 0.8em;
}

.status {
  padding-left: 8px;
}

.trusted-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 4px 8px;
  background: rgba(16, 185, 129, 0.1);
  color: var(--success);
  border-radius: var(--radius-sm);
  font-size: 0.75rem;
  font-weight: 500;
}

.icon {
  width: 14px;
  height: 14px;
}

.drag-overlay {
  position: absolute;
  inset: 0;
  background: rgba(15, 17, 21, 0.85);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  border: 2px dashed var(--accent-primary);
  border-radius: var(--radius-xl);
  z-index: 10;
}

.drop-text {
  font-size: 1.1rem;
  font-weight: 600;
}

.spinner-badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  background: rgba(59, 130, 246, 0.1);
  color: var(--accent-primary);
  border-radius: var(--radius-sm);
  font-size: 0.75rem;
  font-weight: 500;
}

.spinner-icon {
  width: 14px;
  height: 14px;
  animation: spin 1s linear infinite;
}

@keyframes spin {
  100% {
    transform: rotate(360deg);
  }
}
</style>
