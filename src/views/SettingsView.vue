<script setup lang="ts">
import { ref, onMounted } from 'vue';
import { getAppConfig, setAppConfig, getLocalIdentity } from '../ipc';
import { open as openDialog, message } from '@tauri-apps/plugin-dialog';

const emit = defineEmits(['back']);
const form = ref({ device_name: '', download_dir: '' });
const fingerprint = ref('');
const loading = ref(true);
onMounted(async () => {
  const backendConfig = await getAppConfig();
  form.value.device_name = backendConfig.device_name;
  form.value.download_dir = backendConfig.download_dir || '';
  
  const identity = await getLocalIdentity();
  fingerprint.value = identity.fingerprint;
  
  loading.value = false;
});

async function copyFingerprint() {
  try {
    await navigator.clipboard.writeText(fingerprint.value);
  } catch (e) {
    console.error("Failed to copy", e);
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
    download_dir: form.value.download_dir || null 
  };
  try {
    await setAppConfig(payload);
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
        <button class="btn-icon" @click="emit('back')">← Back</button>
        <h2>Settings</h2>
      </div>
    </header>
    <main class="content glass-panel" v-if="!loading">
      <div class="form-group">
        <label>Device Name (Public)</label>
        <input type="text" v-model="form.device_name" class="input-field" placeholder="Enter device name" />
      </div>
      <div class="form-group">
        <label>Device Fingerprint</label>
        <div class="path-picker">
          <input type="text" :value="fingerprint" class="input-field" readonly style="font-family: monospace; font-size: 0.85em; opacity: 0.8;" />
          <button @click="copyFingerprint" class="btn-secondary">Copy</button>
        </div>
      </div>
      <div class="form-group">
        <label>Download Directory</label>
        <div class="path-picker">
          <input type="text" v-model="form.download_dir" class="input-field" placeholder="Default (~/Downloads/DashDrop)" />
          <button @click="pickFolder" class="btn-secondary">Browse</button>
        </div>
      </div>
      <div class="actions">
        <button @click="save" class="btn-primary">Save Changes</button>
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
}

.app-header {
  display: flex;
  align-items: center;
  padding: 24px 32px;
  background: linear-gradient(to bottom, rgba(15, 17, 21, 0.8), transparent);
}

.header-left {
  display: flex;
  align-items: center;
  gap: 16px;
}

.header-left h2 {
  margin: 0;
  font-size: 1.5rem;
  font-weight: 600;
}

.btn-icon {
  background: transparent;
  border: none;
  color: var(--text-primary);
  font-size: 1rem;
  cursor: pointer;
  padding: 8px;
}

.content {
  margin: 0 32px 32px 32px;
  padding: 24px;
  display: flex;
  flex-direction: column;
  gap: 24px;
}

.form-group {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.form-group label {
  font-size: 0.9rem;
  color: var(--text-secondary);
}

.input-field {
  padding: 10px 12px;
  border-radius: 8px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  background: rgba(0, 0, 0, 0.2);
  color: var(--text-primary);
  font-size: 1rem;
  width: 100%;
}

.path-picker {
  display: flex;
  gap: 12px;
}

.btn-secondary {
  padding: 10px 16px;
  border-radius: 8px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  background: rgba(255, 255, 255, 0.05);
  color: var(--text-primary);
  cursor: pointer;
}

.btn-primary {
  padding: 12px 24px;
  border-radius: 8px;
  border: none;
  background: var(--accent-gradient);
  color: white;
  font-weight: 600;
  cursor: pointer;
  align-self: flex-start;
}
</style>
