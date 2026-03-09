<script setup lang="ts">
import { ref, onMounted } from 'vue';
import {
  getAppConfig,
  getLocalIdentity,
  getRuntimeStatus,
  getSecurityPosture,
  getTransferMetrics,
  setAppConfig,
} from '../ipc';
import { open as openDialog, message } from '@tauri-apps/plugin-dialog';
import type { RuntimeStatus, TransferMetrics } from '../types';

const emit = defineEmits(['back']);
const form = ref({
  device_name: '',
  auto_accept_trusted_only: false,
  download_dir: '',
  file_conflict_strategy: 'rename' as 'rename' | 'overwrite' | 'skip',
  max_parallel_streams: 4,
});
const fingerprint = ref('');
const loading = ref(true);
const insecureStorage = ref(false);
const runtimeStatus = ref<RuntimeStatus | null>(null);
const metrics = ref<TransferMetrics | null>(null);

const formatSize = (bytes: number) => {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  const gb = mb / 1024;
  return `${gb.toFixed(1)} GB`;
};

const formatDuration = (ms: number) => {
  if (ms < 1000) return `${ms} ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)} s`;
  const minutes = Math.floor(seconds / 60);
  const remain = Math.round(seconds % 60);
  return `${minutes}m ${remain}s`;
};

const loadRuntime = async () => {
  runtimeStatus.value = await getRuntimeStatus();
  metrics.value = await getTransferMetrics();
};

onMounted(async () => {
  const backendConfig = await getAppConfig();
  form.value.device_name = backendConfig.device_name;
  form.value.auto_accept_trusted_only = backendConfig.auto_accept_trusted_only;
  form.value.download_dir = backendConfig.download_dir || '';
  form.value.file_conflict_strategy = backendConfig.file_conflict_strategy;
  form.value.max_parallel_streams = backendConfig.max_parallel_streams;

  const identity = await getLocalIdentity();
  fingerprint.value = identity.fingerprint;
  const posture = await getSecurityPosture();
  insecureStorage.value = !posture.secure_store_available;
  await loadRuntime();

  loading.value = false;
});

async function copyFingerprint() {
  try {
    await navigator.clipboard.writeText(fingerprint.value);
  } catch (e) {
    console.error('Failed to copy', e);
  }
}

async function pickFolder() {
  const selected = await openDialog({ directory: true });
  if (selected && !Array.isArray(selected)) {
    form.value.download_dir = selected;
  }
}

async function save() {
  const payload = {
    device_name: form.value.device_name,
    auto_accept_trusted_only: form.value.auto_accept_trusted_only,
    download_dir: form.value.download_dir || null,
    file_conflict_strategy: form.value.file_conflict_strategy,
    max_parallel_streams: Math.max(1, Math.min(32, Number(form.value.max_parallel_streams) || 4)),
  };
  try {
    await setAppConfig(payload);
    await loadRuntime();
    emit('back');
  } catch (e: unknown) {
    await message(String(e), { title: 'Save Failed', kind: 'error' });
  }
}
</script>

<template>
  <div class="settings-view">
    <header class="app-header">
      <div class="header-left">
        <button class="btn btn-secondary" @click="emit('back')">Back</button>
        <h2>Settings</h2>
      </div>
    </header>
    <main class="content glass-panel" v-if="!loading">
      <div v-if="insecureStorage" class="security-warning">
        Security warning: system keyring is unavailable. Private key is stored in a local file with reduced protection.
      </div>
      <div class="form-group">
        <label>Device Name (Public)</label>
        <input type="text" v-model="form.device_name" class="input-field" placeholder="Enter device name" />
      </div>
      <div class="form-group">
        <label>Device Fingerprint</label>
        <div class="path-picker">
          <input type="text" :value="fingerprint" class="input-field" readonly style="font-family: monospace; font-size: 0.85em; opacity: 0.8;" />
          <button @click="copyFingerprint" class="btn btn-secondary">Copy</button>
        </div>
      </div>
      <div class="form-group">
        <label>Download Directory</label>
        <div class="path-picker">
          <input type="text" v-model="form.download_dir" class="input-field" placeholder="Default (~/Downloads/DashDrop)" />
          <button @click="pickFolder" class="btn btn-secondary">Browse</button>
        </div>
      </div>
      <div class="form-group">
        <label class="checkbox-label">
          <input type="checkbox" v-model="form.auto_accept_trusted_only" />
          Auto-accept incoming transfers from trusted devices only
        </label>
      </div>
      <div class="form-group">
        <label>File Conflict Strategy</label>
        <select v-model="form.file_conflict_strategy" class="input-field">
          <option value="rename">Rename incoming file</option>
          <option value="overwrite">Overwrite existing file</option>
          <option value="skip">Skip conflicting file</option>
        </select>
      </div>
      <div class="form-group">
        <label>Parallel Streams (1-32)</label>
        <input type="number" min="1" max="32" v-model.number="form.max_parallel_streams" class="input-field" />
      </div>
      <div class="runtime-grid" v-if="runtimeStatus">
        <div class="runtime-card">
          <div class="runtime-label">mDNS Service</div>
          <div class="runtime-value">{{ runtimeStatus.mdns_registered ? 'Registered' : 'Not registered' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Listener Port</div>
          <div class="runtime-value">{{ runtimeStatus.local_port }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Discovered Devices</div>
          <div class="runtime-value">{{ runtimeStatus.discovered_devices }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Trusted Devices</div>
          <div class="runtime-value">{{ runtimeStatus.trusted_devices }}</div>
        </div>
      </div>
      <div class="runtime-grid" v-if="metrics">
        <div class="runtime-card">
          <div class="runtime-label">Completed / Partial</div>
          <div class="runtime-value">{{ metrics.completed }} / {{ metrics.partial }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Failed / Rejected</div>
          <div class="runtime-value">{{ metrics.failed }} / {{ metrics.rejected }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Bytes Sent</div>
          <div class="runtime-value">{{ formatSize(metrics.bytes_sent) }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Bytes Received</div>
          <div class="runtime-value">{{ formatSize(metrics.bytes_received) }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Average Duration</div>
          <div class="runtime-value">{{ formatDuration(metrics.average_duration_ms) }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Failure Distribution</div>
          <div class="runtime-value">
            <span v-if="Object.keys(metrics.failure_distribution || {}).length === 0">No failures</span>
            <span v-else>
              {{ Object.entries(metrics.failure_distribution).map(([k, v]) => `${k}:${v}`).join(" · ") }}
            </span>
          </div>
        </div>
      </div>
      <div class="actions">
        <button @click="loadRuntime" class="btn btn-secondary">Refresh Runtime</button>
        <button @click="save" class="btn btn-primary">Save Changes</button>
      </div>
    </main>
  </div>
</template>

<style scoped>
.settings-view {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  background: linear-gradient(190deg, rgba(255, 255, 255, 0.33), transparent 32%);
}

.app-header {
  display: flex;
  align-items: center;
  padding: 26px 28px 12px;
}

.header-left {
  display: flex;
  align-items: center;
  gap: 16px;
}

.header-left h2 {
  margin: 0;
  font-size: 1.42rem;
  font-weight: 600;
}

.content {
  margin: 0 28px 24px;
  padding: 24px;
  border-radius: 18px;
  display: flex;
  flex-direction: column;
  gap: 24px;
}

.security-warning {
  padding: 10px 12px;
  border-radius: 10px;
  border: 1px solid rgba(157, 58, 51, 0.35);
  background: rgba(157, 58, 51, 0.08);
  color: #7f2f2a;
  font-size: 0.9rem;
}

.form-group {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.form-group label {
  font-size: 0.9rem;
  color: var(--text-muted);
}

.checkbox-label {
  display: flex;
  gap: 10px;
  align-items: center;
}

.input-field {
  padding: 10px 12px;
  border-radius: 10px;
  border: 1px solid var(--border-subtle);
  background: rgba(255, 255, 255, 0.72);
  color: var(--text-secondary);
  font-size: 1rem;
  width: 100%;
}

.path-picker {
  display: flex;
  gap: 12px;
}

.runtime-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;
}

.runtime-card {
  padding: 10px 12px;
  border-radius: 10px;
  border: 1px solid var(--border-subtle);
  background: rgba(255, 255, 255, 0.7);
}

.runtime-label {
  font-size: 0.8rem;
  color: var(--text-muted);
}

.runtime-value {
  margin-top: 4px;
  font-size: 0.95rem;
  font-weight: 600;
}

.actions {
  display: flex;
  gap: 10px;
  align-items: center;
}

@media (max-width: 780px) {
  .runtime-grid {
    grid-template-columns: 1fr;
  }
}
</style>
