<script setup lang="ts">
import { ref } from 'vue';
import { 
  acceptTransfer, rejectTransfer, acceptAndPairTransfer 
} from '../ipc';
import type { FileItemMeta } from '../types';

const props = defineProps<{
  transferId: string;
  senderName: string;
  senderFp: string;
  trusted: boolean;
  items: FileItemMeta[];
  totalSize: number;
}>();

const emit = defineEmits<{
  (e: 'close'): void;
}>();

const isProcessing = ref(false);

const formatBytes = (bytes: number) => {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
};

const handleAccept = async () => {
  isProcessing.value = true;
  try {
    await acceptTransfer(props.transferId);
  } catch (error) {
    console.error("Failed to accept transfer:", error);
  } finally {
    isProcessing.value = false;
    emit('close');
  }
};

const handleAcceptAndPair = async () => {
  isProcessing.value = true;
  try {
    await acceptAndPairTransfer(props.transferId, props.senderFp);
  } catch (error) {
    console.error("Failed to accept & pair:", error);
  } finally {
    isProcessing.value = false;
    emit('close');
  }
};

const handleReject = async () => {
  isProcessing.value = true;
  try {
    await rejectTransfer(props.transferId);
  } catch (error) {
    console.error("Failed to reject transfer:", error);
  } finally {
    isProcessing.value = false;
    emit('close');
  }
};
</script>

<template>
  <div class="modal-backdrop">
    <div class="modal glass-panel animate-fade-in">
      <h2>Incoming Transfer</h2>
      
      <div class="sender-info">
        <div class="avatar text-gradient">{{ senderName.charAt(0).toUpperCase() }}</div>
        <div>
          <h3>{{ senderName }}</h3>
          <p class="text-muted text-sm">wants to send you files</p>
        </div>
      </div>

      <div class="security-warning" v-if="!trusted">
        <svg viewBox="0 0 24 24" fill="none" class="icon-warn" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
        </svg>
        <div>
          <span>This is the first time connecting with this device. Verify identity if possible:</span>
          <br>
          <code style="margin-top: 8px; display: inline-block; word-break: break-all; font-family: monospace; font-size: 0.85em; background: rgba(0,0,0,0.2); padding: 4px; border-radius: 4px;">{{ senderFp }}</code>
        </div>
      </div>

      <div class="file-list">
        <div class="list-header">
          <span>{{ items.length }} items</span>
          <span>{{ formatBytes(totalSize) }}</span>
        </div>
        <div class="files">
          <div v-for="item in items.slice(0, 3)" :key="item.file_id" class="file-item">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-sm">
              <path stroke-linecap="round" stroke-linejoin="round" d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z" />
            </svg>
            <span class="file-name" :title="item.rel_path">{{ item.name }}</span>
            <span class="file-size text-muted">{{ formatBytes(item.size) }}</span>
          </div>
          <div v-if="items.length > 3" class="file-item more">
            + {{ items.length - 3 }} more files...
          </div>
        </div>
      </div>

      <div class="actions">
        <button class="btn btn-secondary" @click="handleReject" :disabled="isProcessing">Decline</button>
        <div class="primary-actions">
          <button v-if="!trusted" class="btn btn-primary outline" @click="handleAcceptAndPair" :disabled="isProcessing">
            Accept & Pair
          </button>
          <button class="btn btn-primary" @click="handleAccept" :disabled="isProcessing">
            Accept
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.modal-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.6);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 100;
  padding: 20px;
}

.modal {
  width: 100%;
  max-width: 440px;
  padding: 24px;
  border-radius: var(--radius-xl);
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.sender-info {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 16px;
  background: rgba(255, 255, 255, 0.03);
  border-radius: var(--radius-md);
}

.avatar {
  width: 48px;
  height: 48px;
  border-radius: 50%;
  background: rgba(59, 130, 246, 0.1);
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 1.5rem;
  font-weight: 600;
}

.security-warning {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  padding: 12px 16px;
  background: rgba(245, 158, 11, 0.1);
  color: var(--warning);
  border-radius: var(--radius-md);
  font-size: 0.9rem;
  line-height: 1.4;
}

.icon-warn {
  width: 20px;
  flex-shrink: 0;
}

.file-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.list-header {
  display: flex;
  justify-content: space-between;
  font-size: 0.9rem;
  font-weight: 500;
  color: var(--text-secondary);
  padding: 0 4px;
}

.files {
  background: var(--bg-surface-elevated);
  border-radius: var(--radius-md);
  padding: 8px;
  max-height: 200px;
  overflow-y: auto;
}

.file-item {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px;
  border-radius: var(--radius-sm);
}

.file-item:hover {
  background: var(--bg-surface-hover);
}

.icon-sm {
  width: 16px;
  height: 16px;
  color: var(--text-tertiary);
}

.file-name {
  flex: 1;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  font-size: 0.95rem;
}

.file-size {
  font-size: 0.85rem;
}

.more {
  justify-content: center;
  color: var(--text-tertiary);
  font-style: italic;
}

.actions {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-top: 8px;
}

.primary-actions {
  display: flex;
  gap: 12px;
}

.outline {
  background: transparent !important;
  color: var(--text-primary) !important;
  border: 1px solid var(--accent-primary) !important;
  box-shadow: none !important;
}

.outline:hover {
  background: rgba(59, 130, 246, 0.1) !important;
}

button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
