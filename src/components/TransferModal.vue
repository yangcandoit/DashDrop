<script setup lang="ts">
import { ref } from 'vue';
import {
  acceptTransfer,
  rejectTransfer,
  acceptAndPairTransfer
} from '../ipc';
import type { FileItemMeta } from '../types';

const props = defineProps<{
  transferId: string;
  notificationId: string;
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
    await acceptTransfer(props.transferId, props.notificationId);
  } catch (error) {
    console.error('Failed to accept transfer:', error);
  } finally {
    isProcessing.value = false;
    emit('close');
  }
};

const handleAcceptAndPair = async () => {
  isProcessing.value = true;
  try {
    await acceptAndPairTransfer(props.transferId, props.notificationId, props.senderFp);
  } catch (error) {
    console.error('Failed to accept & pair:', error);
  } finally {
    isProcessing.value = false;
    emit('close');
  }
};

const handleReject = async () => {
  isProcessing.value = true;
  try {
    await rejectTransfer(props.transferId, props.notificationId);
  } catch (error) {
    console.error('Failed to reject transfer:', error);
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
        <div class="avatar">{{ senderName.charAt(0).toUpperCase() }}</div>
        <div>
          <h3>{{ senderName }}</h3>
          <p class="text-muted text-sm">wants to send files</p>
        </div>
      </div>

      <div class="security-warning" v-if="!trusted">
        <div>
          <span>First-time peer. Verify fingerprint before accepting sensitive files:</span>
          <br />
          <code class="fingerprint">{{ senderFp }}</code>
        </div>
      </div>

      <div class="file-list">
        <div class="list-header">
          <span>{{ items.length }} items</span>
          <span>{{ formatBytes(totalSize) }}</span>
        </div>
        <div class="files">
          <div v-for="item in items.slice(0, 3)" :key="item.file_id" class="file-item">
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
  background: rgba(0, 0, 0, 0.2);
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
  border-radius: 16px;
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.sender-info {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 16px;
  background: var(--surface-muted);
  border: 1px solid var(--border-subtle);
  border-radius: 10px;
}

.avatar {
  width: 48px;
  height: 48px;
  border-radius: 10px;
  background: #f2f2f7;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 1.2rem;
  font-family: var(--font-body);
  font-weight: 600;
  color: var(--text-secondary);
}

.text-sm {
  font-size: 0.84rem;
}

.security-warning {
  padding: 12px 16px;
  border: 1px solid rgba(178, 106, 0, 0.25);
  background: rgba(178, 106, 0, 0.08);
  color: #8a5300;
  border-radius: 10px;
  font-size: 0.9rem;
  line-height: 1.4;
}

.fingerprint {
  margin-top: 8px;
  display: inline-block;
  word-break: break-all;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
  font-size: 0.8em;
  background: #fff;
  border: 1px solid var(--border-subtle);
  padding: 4px 6px;
  border-radius: 6px;
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
  color: var(--text-muted);
  padding: 0 4px;
}

.files {
  background: #fff;
  border: 1px solid var(--border-subtle);
  border-radius: 10px;
  padding: 8px;
  max-height: 200px;
  overflow-y: auto;
}

.file-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px;
  border-radius: 8px;
}

.file-item:hover {
  background: #f2f2f7;
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
  color: var(--text-muted);
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
  color: var(--text-secondary) !important;
  border: 1px solid var(--border-strong) !important;
  box-shadow: none !important;
}

.outline:hover {
  background: rgba(255, 255, 255, 0.7) !important;
}

button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
