<script setup lang="ts">
import { computed, ref, watchEffect } from 'vue';
import type { DeviceView } from '../types';

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
  const sessions = Object.values(props.device.sessions || {});
  if (sessions.length > 0 && sessions[0].addrs && sessions[0].addrs.length > 0) {
    return sessions[0].addrs[0];
  }
  if (props.device.last_seen && props.device.last_seen > 0) {
    return `Last seen ${timeAgo(props.device.last_seen)}`;
  }
  return 'Offline';
};

const isOffline = ref(Object.keys(props.device.sessions || {}).length === 0);
watchEffect(() => {
  isOffline.value = Object.keys(props.device.sessions || {}).length === 0;
});

const platformKind = computed(() => props.device.platform.toLowerCase());
</script>

<template>
  <div
    class="device-card glass-panel animate-fade-in"
    :class="{ 'drag-over': isDragOver, trusted: device.trusted, offline: isOffline }"
    @dragenter="onDragEnter"
    @dragover="onDragOver"
    @dragleave="onDragLeave"
    @drop="onDrop"
    @click="emit('click')"
  >
    <div class="avatar-bg">
      <div class="avatar">{{ initial }}</div>
    </div>

    <div class="info">
      <h3>{{ device.name }}</h3>
      <div class="meta text-muted">
        <span class="platform-pill" :class="platformKind">
          <svg v-if="platformKind === 'windows'" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M3 4.5 10.5 3v8H3v-6.5Zm0 8.5h7.5v8L3 19.5V13Zm9 0h9v9.5L12 21v-8Zm0-10 9-1.5V11h-9V3Z"/>
          </svg>
          <svg v-else-if="platformKind === 'mac'" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M15.4 4.1c.9-1.1 1.4-2.6 1.2-4.1-1.4.1-3.1 1-4 2.1-.8.9-1.5 2.4-1.3 3.8 1.6.1 3.2-.8 4.1-1.8Zm3.8 12.8c-.6 1.4-.9 2-1.7 3.2-1.2 1.8-2.9 4.1-4.9 4.1-1.8 0-2.3-1.2-4.4-1.2-2.1 0-2.7 1.2-4.5 1.2-2 0-3.6-2-4.8-3.8C-2.2 14.3-.9 7.8 2.6 6c1.9-1 4.6-.8 6.2.4 1 .7 1.7 1.3 3.2 1.3 1.4 0 2-.6 3.2-1.3 1.4-.9 3.9-1 5.8-.5-1.4 1-2.5 2.4-2.5 4.4 0 2.4 1.9 3.6 3 4.1-.4 1.1-.8 1.7-1.3 2.5Z"/>
          </svg>
          <svg v-else-if="platformKind === 'linux'" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M12 2c2.8 0 4.6 2.7 4.6 6v4.5c1.1.5 1.9 1.6 1.9 2.8 0 1.8-1.5 3.2-3.3 3.2H8.8c-1.8 0-3.3-1.4-3.3-3.2 0-1.2.8-2.3 1.9-2.8V8c0-3.3 1.8-6 4.6-6Zm-1.8 3.6a1 1 0 1 0 0 2 1 1 0 0 0 0-2Zm3.6 0a1 1 0 1 0 0 2 1 1 0 0 0 0-2Zm-.2 8.6c0 .8-.7 1.5-1.6 1.5-.9 0-1.6-.7-1.6-1.5h3.2Z"/>
          </svg>
          <svg v-else-if="platformKind === 'android'" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M7.4 8.2h9.2c.7 0 1.2.5 1.2 1.2v7.5c0 .7-.5 1.2-1.2 1.2h-.9v2c0 .5-.4.9-.9.9s-.9-.4-.9-.9v-2h-4v2c0 .5-.4.9-.9.9s-.9-.4-.9-.9v-2h-.9c-.7 0-1.2-.5-1.2-1.2V9.4c0-.7.5-1.2 1.2-1.2Zm.8-2.3 1.4.8a4.5 4.5 0 0 1 4.8 0l1.4-.8.5.9-1.4.8a4.8 4.8 0 0 1 1.1 2.2H7.1c.2-.8.6-1.6 1.1-2.2L6.8 6.8l.5-.9Zm2.4 4.2a.6.6 0 1 0 0 1.3.6.6 0 0 0 0-1.3Zm2.8 0a.6.6 0 1 0 0 1.3.6.6 0 0 0 0-1.3Z"/>
          </svg>
          <svg v-else-if="platformKind === 'ios'" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M16.7 4.2h-2.1l-.5-1.4A1.2 1.2 0 0 0 13 2h-2a1.2 1.2 0 0 0-1.1.8l-.5 1.4H7.3C6 4.2 5 5.2 5 6.5v13c0 1.3 1 2.3 2.3 2.3h9.4c1.3 0 2.3-1 2.3-2.3v-13c0-1.3-1-2.3-2.3-2.3Zm-4.7 15.5a1.2 1.2 0 1 1 0-2.4 1.2 1.2 0 0 1 0 2.4Zm4.8-4H7.2V6.8h9.6v8.9Z"/>
          </svg>
          <svg v-else viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="12" cy="12" r="7" />
          </svg>
          <span>{{ device.platform }}</span>
        </span>
        <span class="dot">•</span>
        <span>{{ getAddr() }}</span>
      </div>
    </div>

    <div class="status" v-if="isSending">
      <div class="state-badge sending">Sending</div>
    </div>
    <div class="status" v-else-if="device.trusted">
      <div class="state-badge trusted">Trusted</div>
    </div>

    <div class="drag-overlay" v-if="isDragOver">
      <div class="drop-text">Drop to send</div>
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
  border-radius: 16px;
  cursor: pointer;
  transition: transform 220ms ease, border-color 220ms ease, background-color 220ms ease;
  overflow: hidden;
}

.device-card:hover {
  transform: translateY(-2px);
  border-color: var(--border-strong);
  background: rgba(255, 255, 255, 0.86);
}

.device-card.drag-over {
  transform: scale(1.01);
  border-color: color-mix(in srgb, var(--accent) 42%, transparent);
  box-shadow: 0 0 0 3px rgba(178, 79, 52, 0.12);
}

.device-card.offline {
  opacity: 0.72;
  filter: grayscale(0.32);
}

.device-card.offline:hover {
  opacity: 0.92;
  filter: grayscale(0.16);
}

.avatar-bg {
  width: 48px;
  height: 48px;
  border-radius: 12px;
  background: linear-gradient(170deg, rgba(178, 79, 52, 0.12), rgba(214, 174, 140, 0.3));
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  border: 1px solid rgba(178, 79, 52, 0.18);
}

.avatar {
  font-size: 1.25rem;
  font-family: var(--font-display);
  font-weight: 600;
  color: #6f3224;
}

.device-card:hover .avatar-bg {
  background: linear-gradient(170deg, rgba(178, 79, 52, 0.18), rgba(214, 174, 140, 0.4));
}

.device-card.trusted .avatar-bg {
  border-color: rgba(47, 107, 82, 0.34);
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
  color: var(--text-primary);
}

.meta {
  font-size: 0.8rem;
  display: flex;
  align-items: center;
  gap: 6px;
  color: var(--text-muted);
}

.platform-pill {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 7px;
  border-radius: 999px;
  border: 1px solid var(--border-subtle);
  background: rgba(255, 255, 255, 0.68);
  color: var(--text-secondary);
}

.platform-pill svg {
  width: 12px;
  height: 12px;
  fill: currentColor;
}

.platform-pill.windows {
  color: #2563a6;
}

.platform-pill.mac {
  color: #5a4b46;
}

.platform-pill.linux {
  color: #8a5a2f;
}

.platform-pill.android {
  color: #2f6b52;
}

.platform-pill.ios {
  color: #3a5b9d;
}

.dot {
  opacity: 0.5;
  font-size: 0.8em;
}

.status {
  padding-left: 8px;
}

.state-badge {
  display: inline-flex;
  align-items: center;
  padding: 4px 8px;
  border-radius: 999px;
  border: 1px solid var(--border-subtle);
  font-size: 0.72rem;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  font-weight: 700;
}

.state-badge.trusted {
  color: var(--success);
  border-color: rgba(47, 107, 82, 0.35);
  background: rgba(47, 107, 82, 0.08);
}

.state-badge.sending {
  color: #7b3a29;
  border-color: rgba(178, 79, 52, 0.3);
  background: rgba(178, 79, 52, 0.1);
}

.drag-overlay {
  position: absolute;
  inset: 0;
  background: rgba(255, 252, 246, 0.88);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  border: 2px dashed color-mix(in srgb, var(--accent) 55%, transparent);
  border-radius: 16px;
  z-index: 10;
}

.drop-text {
  font-family: var(--font-display);
  font-size: 1rem;
  font-weight: 600;
  color: #5f2c20;
}
</style>
