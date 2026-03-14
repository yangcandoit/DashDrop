<script setup lang="ts">
import { ref, onMounted, onUnmounted, watch } from 'vue';
import {
  confirmTrustedPeerVerification,
  getAppConfig,
  copyTextToClipboard,
  getDiscoveryDiagnostics,
  getLocalIdentity,
  getLocalBleAssistCapsule,
  getLocalPairingLink,
  getRuntimeStatus,
  getSecurityPosture,
  getTransferMetrics,
  pairDevice,
  setTrustedAlias,
  setAppConfig,
  subscribeRuntimeEvents,
} from '../ipc';
import { open as openDialog, message } from '@tauri-apps/plugin-dialog';
import type {
  AppConfig,
  BleAssistCapsule,
  DiscoveryDiagnostics,
  LocalIdentity,
  RuntimeStatus,
  TransferMetrics,
} from '../types';
import type { PairingQrPayload } from '../security';
import { pairingUriFromIdentity, verificationCodeFromFingerprint } from '../security';
import PairingImportModal from '../components/PairingImportModal.vue';
import PairingQrModal from '../components/PairingQrModal.vue';
import { clearPendingPairingLink, pendingPairingLink } from '../store';

const emit = defineEmits(['back']);
const form = ref({
  device_name: '',
  auto_accept_trusted_only: false,
  download_dir: '',
  file_conflict_strategy: 'rename' as 'rename' | 'overwrite' | 'skip',
  max_parallel_streams: 4,
  launch_at_login: false,
});
const localIdentity = ref<LocalIdentity | null>(null);
const fingerprint = ref('');
const verificationCode = ref('');
const pairingUri = ref('');
const showPairingQr = ref(false);
const showPairingImport = ref(false);
const pairingImportBusy = ref(false);
const pairingImportInitialInput = ref('');
const loading = ref(true);
const insecureStorage = ref(false);
const runtimeStatus = ref<RuntimeStatus | null>(null);
const metrics = ref<TransferMetrics | null>(null);
const replayDiagnostics = ref<DiscoveryDiagnostics['runtime_event_replay'] | null>(null);
const progressPersistenceDiagnostics = ref<DiscoveryDiagnostics['transfer_progress_persistence'] | null>(null);
const linkCapabilities = ref<DiscoveryDiagnostics['link_capabilities'] | null>(null);
const bleAssistCapsule = ref<BleAssistCapsule | null>(null);
const loadError = ref<string | null>(null);
const unlistens: Array<() => void> = [];

function refreshPairingUri(identity = localIdentity.value) {
  if (!identity) {
    pairingUri.value = '';
    return Promise.resolve('');
  }
  return getLocalPairingLink()
    .then((link) => {
      pairingUri.value = link;
      return link;
    })
    .catch((error) => {
      console.warn('Falling back to legacy local pairing link generation.', error);
      const link = pairingUriFromIdentity(identity.fingerprint, identity.device_name, Date.now());
      pairingUri.value = link;
      return link;
    });
}

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

const formatCheckpointAge = (updatedAtUnixMs?: number) => {
  if (!updatedAtUnixMs || updatedAtUnixMs <= 0) return 'n/a';
  const ageMs = Math.max(0, Date.now() - updatedAtUnixMs);
  if (ageMs < 1000) return `${ageMs} ms ago`;
  const ageSec = Math.floor(ageMs / 1000);
  if (ageSec < 60) return `${ageSec}s ago`;
  const minutes = Math.floor(ageSec / 60);
  const seconds = ageSec % 60;
  return `${minutes}m ${seconds}s ago`;
};

const formatAgeMs = (ageMs?: number | null) => {
  if (ageMs === undefined || ageMs === null || ageMs < 0) return 'n/a';
  if (ageMs < 1000) return `${ageMs} ms`;
  const ageSec = Math.floor(ageMs / 1000);
  if (ageSec < 60) return `${ageSec}s`;
  const minutes = Math.floor(ageSec / 60);
  const seconds = ageSec % 60;
  if (minutes < 60) return `${minutes}m ${seconds}s`;
  const hours = Math.floor(minutes / 60);
  const remainMinutes = minutes % 60;
  return `${hours}h ${remainMinutes}m`;
};

const formatReplayCheckpoints = (
  checkpoints: NonNullable<DiscoveryDiagnostics['runtime_event_replay']>['checkpoints'],
) =>
  (checkpoints ?? [])
    .map(
      (checkpoint) =>
        `${checkpoint.consumer_id}:${checkpoint.seq} ` +
        `[${checkpoint.lifecycle_state ?? 'unknown'} / ${checkpoint.recovery_state ?? 'unknown'}] ` +
        `lag ${checkpoint.lag_events ?? 0} · ${formatAgeMs(checkpoint.age_ms ?? null)}`,
    )
    .join(' · ');

const formatTimestampAge = (value?: number | null) => {
  if (!value || value <= 0) return 'n/a';
  return formatCheckpointAge(value);
};

const formatAbsoluteTimestamp = (value?: number | null) => {
  if (!value || value <= 0) return 'n/a';
  return new Date(value).toLocaleString();
};

const formatDeadline = (value?: number | null) => {
  if (!value || value <= 0) return 'n/a';
  const deltaMs = value - Date.now();
  if (deltaMs <= 0) return `due now · ${formatAbsoluteTimestamp(value)}`;
  return `in ${formatAgeMs(deltaMs)} · ${formatAbsoluteTimestamp(value)}`;
};

const formatReplaySource = (
  value?: 'memory_hot_window' | 'persisted_catch_up' | 'resync_required' | 'empty' | null,
) => {
  switch (value) {
    case 'memory_hot_window':
      return 'Memory hot window';
    case 'persisted_catch_up':
      return 'Persisted catch-up';
    case 'resync_required':
      return 'Resync required';
    case 'empty':
      return 'Empty result';
    default:
      return 'n/a';
  }
};

const formatReplayResyncReason = (
  value?:
    | 'cursor_before_oldest_available'
    | 'cursor_after_latest_available'
    | 'persisted_catch_up_empty'
    | 'persisted_journal_unavailable'
    | null,
) => {
  switch (value) {
    case 'cursor_before_oldest_available':
      return 'Cursor fell behind retained window';
    case 'cursor_after_latest_available':
      return 'Cursor was ahead of daemon replay';
    case 'persisted_catch_up_empty':
      return 'Persisted catch-up returned no gap';
    case 'persisted_journal_unavailable':
      return 'Persisted journal unavailable';
    default:
      return 'No resync observed';
  }
};

const formatBlePermissionState = (value?: string | null) => {
  switch (value) {
    case 'not_supported':
      return 'Not supported on this platform';
    case 'not_requested':
      return 'Runtime not requesting OS permission yet';
    case 'granted':
      return 'Granted';
    case 'denied':
      return 'Denied';
    case 'restricted':
      return 'Restricted';
    case 'prompt_required':
      return 'Prompt required';
    default:
      if (!value) return 'Unknown';
      return value
        .split('_')
        .map((segment) => segment.charAt(0).toUpperCase() + segment.slice(1))
        .join(' ');
  }
};

const loadRuntime = async () => {
  const [runtime, transferMetrics, diagnostics] = await Promise.all([
    getRuntimeStatus(),
    getTransferMetrics(),
    getDiscoveryDiagnostics(),
  ]);
  runtimeStatus.value = runtime;
  metrics.value = transferMetrics;
  replayDiagnostics.value = diagnostics.runtime_event_replay ?? null;
  progressPersistenceDiagnostics.value = diagnostics.transfer_progress_persistence ?? null;
  linkCapabilities.value = diagnostics.link_capabilities ?? null;
  bleAssistCapsule.value = null;
  if (diagnostics.link_capabilities?.ble_baseline_enabled) {
    try {
      bleAssistCapsule.value = await getLocalBleAssistCapsule();
    } catch (error) {
      console.warn('Unable to load local BLE assist capsule preview.', error);
    }
  }
};

const applyLocalIdentity = (identity: LocalIdentity) => {
  localIdentity.value = identity;
  fingerprint.value = identity.fingerprint;
  verificationCode.value = verificationCodeFromFingerprint(identity.fingerprint);
  pairingUri.value = '';
};

const applyConfigToForm = (backendConfig: AppConfig) => {
  form.value.device_name = backendConfig.device_name;
  form.value.auto_accept_trusted_only = backendConfig.auto_accept_trusted_only;
  form.value.download_dir = backendConfig.download_dir || '';
  form.value.file_conflict_strategy = backendConfig.file_conflict_strategy;
  form.value.max_parallel_streams = backendConfig.max_parallel_streams;
  form.value.launch_at_login = backendConfig.launch_at_login;
};

const loadConfig = async () => {
  applyConfigToForm(await getAppConfig());
};

const loadIdentity = async () => {
  applyLocalIdentity(await getLocalIdentity());
};

const refreshRuntimeOnEvent = (event: string) => {
  if (
    event === 'device_discovered' ||
    event === 'device_updated' ||
    event === 'device_lost' ||
    event === 'daemon_control_plane_recovered' ||
    event === 'transfer_complete' ||
    event === 'transfer_partial' ||
    event === 'transfer_rejected' ||
    event === 'transfer_cancelled_by_sender' ||
    event === 'transfer_cancelled_by_receiver' ||
    event === 'transfer_failed' ||
    event === 'system_error'
  ) {
    void loadRuntime();
  }
};

const refreshConfigOnEvent = (event: string) => {
  if (event === 'app_config_updated') {
    void loadConfig();
    void loadIdentity();
    void loadRuntime();
    return;
  }
  if (event === 'daemon_event_feed_resync_required') {
    void loadConfig();
    void loadIdentity();
    void loadRuntime();
  }
};

onMounted(async () => {
  try {
    loadError.value = null;
    const [backendConfig, identity, posture] = await Promise.all([
      getAppConfig(),
      getLocalIdentity(),
      getSecurityPosture(),
    ]);

    applyConfigToForm(backendConfig);
    applyLocalIdentity(identity);
    insecureStorage.value = !posture.secure_store_available;
    await loadRuntime();
  } catch (e) {
    console.error('Failed to load settings', e);
    loadError.value = 'Unable to load current settings. Check backend/runtime status and retry.';
  } finally {
    loading.value = false;
  }

  unlistens.push(
    await subscribeRuntimeEvents(
      [
        'device_discovered',
        'device_updated',
        'device_lost',
        'daemon_control_plane_recovered',
        'transfer_complete',
        'transfer_partial',
        'transfer_rejected',
        'transfer_cancelled_by_sender',
        'transfer_cancelled_by_receiver',
        'transfer_failed',
        'system_error',
        'app_config_updated',
        'daemon_event_feed_resync_required',
      ],
      ({ event }) => {
        refreshRuntimeOnEvent(event);
        refreshConfigOnEvent(event);
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

watch(pendingPairingLink, (value) => {
  if (!value) {
    return;
  }
  pairingImportInitialInput.value = value;
  showPairingImport.value = true;
}, { immediate: true });

async function copyFingerprint() {
  try {
    await copyToClipboard(fingerprint.value);
  } catch (e) {
    console.error('Failed to copy', e);
  }
}

async function copyVerificationCode() {
  try {
    await copyToClipboard(verificationCode.value);
  } catch (e) {
    console.error('Failed to copy verification code', e);
  }
}

async function copyPairingLink() {
  try {
    const link = await refreshPairingUri();
    await copyToClipboard(link);
    await message('Pairing link copied to clipboard.', { title: 'Copied', kind: 'info' });
  } catch (e) {
    console.error('Failed to copy pairing link', e);
  }
}

async function openPairingQr() {
  try {
    await refreshPairingUri();
    showPairingQr.value = true;
  } catch (e) {
    console.error('Failed to prepare pairing QR', e);
  }
}

async function importPairingLink({
  payload,
  mutualConfirmationRequested,
}: {
  payload: PairingQrPayload;
  mutualConfirmationRequested: boolean;
}) {
  pairingImportBusy.value = true;
  try {
    await pairDevice(payload.fingerprint);
    await setTrustedAlias(payload.fingerprint, payload.device_name);
    await confirmTrustedPeerVerification(
      payload.fingerprint,
      payload.trust_model === 'signed_link' && payload.signature_verified
        ? 'signed_pairing_link'
        : 'legacy_unsigned_link',
      mutualConfirmationRequested,
    );
    showPairingImport.value = false;
    pairingImportInitialInput.value = '';
    clearPendingPairingLink();
    await loadRuntime();
    await message(
      mutualConfirmationRequested
        ? `Trusted ${payload.device_name} with mutual confirmation recorded.`
        : `Trusted ${payload.device_name}. Mutual confirmation can be completed later after both sides compare the shared pair code.`,
      { title: 'Pairing Complete', kind: 'info' },
    );
  } catch (e) {
    console.error('Failed to import pairing link', e);
    await message(String(e), { title: 'Pairing Failed', kind: 'error' });
  } finally {
    pairingImportBusy.value = false;
  }
}

function closePairingImport() {
  if (pairingImportBusy.value) return;
  showPairingImport.value = false;
  pairingImportInitialInput.value = '';
  clearPendingPairingLink();
}

async function copyToClipboard(text: string) {
  let lastError: unknown = null;
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch (clipboardErr) {
    lastError = clipboardErr;
    const textarea = document.createElement('textarea');
    textarea.value = text;
    textarea.setAttribute('readonly', 'true');
    textarea.style.position = 'fixed';
    textarea.style.top = '-9999px';
    textarea.style.left = '-9999px';
    textarea.style.opacity = '0';
    document.body.appendChild(textarea);
    textarea.focus();
    textarea.select();
    const copied = document.execCommand('copy');
    document.body.removeChild(textarea);
    if (copied) {
      return;
    }
    lastError = clipboardErr;
  }
  try {
    await copyTextToClipboard(text);
    return;
  } catch (nativeErr) {
    lastError = nativeErr;
  }
  throw lastError instanceof Error ? lastError : new Error(String(lastError));
}

async function copyDiscoveryDiagnostics() {
  try {
    const diagnostics = await getDiscoveryDiagnostics();
    await copyToClipboard(JSON.stringify(diagnostics, null, 2));
    try {
      await message('Discovery diagnostics copied to clipboard.', { title: 'Copied', kind: 'info' });
    } catch (noticeError) {
      console.debug('Unable to show copied notice:', noticeError);
    }
  } catch (e: unknown) {
    try {
      await message(String(e), { title: 'Copy Diagnostics Failed', kind: 'error' });
    } catch (noticeError) {
      console.debug('Unable to show diagnostics error notice:', noticeError);
    }
  }
}

async function pickFolder() {
  const selected = await openDialog({ directory: true });
  if (selected && !Array.isArray(selected)) {
    form.value.download_dir = selected;
  }
}

async function save() {
  const deviceName = form.value.device_name.trim();
  if (!deviceName) {
    await message('Device name cannot be empty.', { title: 'Invalid Settings', kind: 'warning' });
    return;
  }

  const payload = {
    device_name: deviceName,
    auto_accept_trusted_only: form.value.auto_accept_trusted_only,
    download_dir: form.value.download_dir || null,
    file_conflict_strategy: form.value.file_conflict_strategy,
    max_parallel_streams: Math.max(1, Math.min(32, Number(form.value.max_parallel_streams) || 4)),
    launch_at_login: form.value.launch_at_login,
  };
  try {
    await setAppConfig(payload);
    await loadIdentity();
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
    <div v-if="loading" style="padding: 24px; color: #888;">Loading settings…</div>
    <main class="content glass-panel" v-if="!loading">
      <div v-if="loadError" class="security-warning">
        {{ loadError }}
      </div>
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
        <label>Verification Code</label>
        <div class="path-picker">
          <input type="text" :value="verificationCode" class="input-field" readonly style="font-family: monospace; font-size: 0.92em; letter-spacing: 0.08em;" />
          <button @click="copyVerificationCode" class="btn btn-secondary">Copy</button>
        </div>
        <p class="text-muted" style="margin-top: 6px;">Use this short code when comparing identity out-of-band with another device.</p>
        <div class="pairing-actions">
          <button @click="copyPairingLink" class="btn btn-secondary" :disabled="!pairingUri">Copy Pairing Link</button>
          <button @click="openPairingQr" class="btn btn-primary" :disabled="!localIdentity">Show Pairing QR</button>
          <button @click="showPairingImport = true" class="btn btn-secondary">Import or Scan Pairing</button>
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
        <label class="checkbox-label">
          <input type="checkbox" v-model="form.launch_at_login" />
          Launch DashDrop automatically when you sign in
        </label>
        <p class="text-muted" style="margin: 0;">
          Registers a per-user startup entry on this desktop so DashDrop is ready in the tray after login.
        </p>
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
          <div class="runtime-label">Runtime Profile</div>
          <div class="runtime-value">{{ runtimeStatus.runtime_profile || 'unknown' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">mDNS Service</div>
          <div class="runtime-value">{{ runtimeStatus.mdns_registered ? 'Registered' : 'Not registered' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Requested Mode</div>
          <div class="runtime-value">{{ runtimeStatus.requested_control_plane_mode || 'in_process' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Control Plane</div>
          <div class="runtime-value">{{ runtimeStatus.control_plane_mode || 'in_process' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Daemon Status</div>
          <div class="runtime-value">{{ runtimeStatus.daemon_status || 'inactive' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Daemon Attach</div>
          <div class="runtime-value">{{ runtimeStatus.daemon_connect_strategy || 'unknown' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Attach Attempts</div>
          <div class="runtime-value">{{ runtimeStatus.daemon_connect_attempts ?? 0 }}</div>
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
        <div class="runtime-card">
          <div class="runtime-label">Headless Idle Exit</div>
          <div class="runtime-value">
            {{ runtimeStatus.daemon_idle_monitor_enabled ? `Enabled (${runtimeStatus.daemon_idle_timeout_secs ?? 0}s)` : 'Disabled' }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Idle Exit Deadline</div>
          <div class="runtime-value">
            {{ runtimeStatus.daemon_idle_deadline_unix_ms ? formatDeadline(runtimeStatus.daemon_idle_deadline_unix_ms) : 'Blocked or disabled' }}
          </div>
        </div>
        <div class="runtime-card runtime-card-wide" v-if="(runtimeStatus.daemon_idle_blockers || []).length > 0">
          <div class="runtime-label">Idle Exit Blockers</div>
          <div class="runtime-value">
            {{ (runtimeStatus.daemon_idle_blockers || []).join(' · ') }}
          </div>
        </div>
        <div class="runtime-card runtime-card-wide" v-if="runtimeStatus.daemon_binary_path">
          <div class="runtime-label">Daemon Binary</div>
          <div class="runtime-value runtime-path">{{ runtimeStatus.daemon_binary_path }}</div>
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
      <div class="runtime-grid" v-if="replayDiagnostics">
        <div class="runtime-card">
          <div class="runtime-label">Replay Generation</div>
          <div class="runtime-value">{{ replayDiagnostics.generation || 'unknown' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Latest Event Seq</div>
          <div class="runtime-value">{{ replayDiagnostics.latest_seq ?? 0 }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Hot Window</div>
          <div class="runtime-value">
            {{ replayDiagnostics.memory_window_len ?? 0 }} / {{ replayDiagnostics.memory_window_capacity ?? 0 }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Persisted Replay</div>
          <div class="runtime-value">
            {{ replayDiagnostics.persisted_oldest_seq ?? 'n/a' }} → {{ replayDiagnostics.latest_seq ?? 0 }}
            ({{ replayDiagnostics.persisted_window_len ?? 0 }})
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Retention Mode</div>
          <div class="runtime-value">{{ replayDiagnostics.retention_mode ?? 'baseline' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Oldest Recoverable Seq</div>
          <div class="runtime-value">{{ replayDiagnostics.oldest_recoverable_seq ?? 'n/a' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Retention Cutoff</div>
          <div class="runtime-value">{{ replayDiagnostics.retention_cutoff_reason ?? 'baseline_capacity' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Persisted Journal</div>
          <div class="runtime-value">{{ replayDiagnostics.persisted_journal_health ?? 'available' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Pinned Checkpoints</div>
          <div class="runtime-value">
            {{ replayDiagnostics.retention_pinned_checkpoint_count ?? 0 }}
            <span v-if="replayDiagnostics.oldest_retention_pinned_checkpoint_seq !== undefined">
              @ {{ replayDiagnostics.oldest_retention_pinned_checkpoint_seq ?? 'n/a' }}
            </span>
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Persisted Segments</div>
          <div class="runtime-value">
            {{ replayDiagnostics.persisted_segment_count ?? 0 }}
            <span v-if="replayDiagnostics.latest_persisted_segment_id !== undefined">
              ({{ replayDiagnostics.oldest_persisted_segment_id ?? 'n/a' }} → {{ replayDiagnostics.latest_persisted_segment_id ?? 'n/a' }})
            </span>
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Compacted Segments</div>
          <div class="runtime-value">
            {{ replayDiagnostics.compacted_segment_count ?? 0 }}
            <span v-if="replayDiagnostics.latest_compacted_segment_id !== undefined">
              (latest {{ replayDiagnostics.latest_compacted_segment_id ?? 'n/a' }})
            </span>
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Compaction Watermark</div>
          <div class="runtime-value">
            seq {{ replayDiagnostics.compaction_watermark_seq ?? 0 }} · seg {{ replayDiagnostics.compaction_watermark_segment_id ?? 0 }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Checkpoint Count</div>
          <div class="runtime-value">{{ replayDiagnostics.checkpoint_count ?? 0 }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Active / Idle / Stale</div>
          <div class="runtime-value">
            {{ replayDiagnostics.active_checkpoint_count ?? 0 }} / {{ replayDiagnostics.idle_checkpoint_count ?? 0 }} / {{ replayDiagnostics.stale_checkpoint_count ?? 0 }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Resync Required</div>
          <div class="runtime-value">{{ replayDiagnostics.resync_required_checkpoint_count ?? 0 }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Feed Requests</div>
          <div class="runtime-value">{{ replayDiagnostics.metrics?.total_feed_requests ?? 0 }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Hot / Persisted Replays</div>
          <div class="runtime-value">
            {{ replayDiagnostics.metrics?.memory_feed_requests ?? 0 }} / {{ replayDiagnostics.metrics?.persisted_catch_up_requests ?? 0 }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Resync Requests</div>
          <div class="runtime-value">{{ replayDiagnostics.metrics?.resync_required_requests ?? 0 }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Replay Path</div>
          <div class="runtime-value">
            {{ formatReplaySource(replayDiagnostics.metrics?.last_replay_source) }}
            <span v-if="replayDiagnostics.metrics?.last_replay_source_at_unix_ms">
              · {{ formatTimestampAge(replayDiagnostics.metrics?.last_replay_source_at_unix_ms) }}
            </span>
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Resync Cause</div>
          <div class="runtime-value">
            {{ formatReplayResyncReason(replayDiagnostics.metrics?.last_resync_reason) }}
            <span v-if="replayDiagnostics.metrics?.last_resync_reason && replayDiagnostics.metrics?.last_resync_required_at_unix_ms">
              · {{ formatTimestampAge(replayDiagnostics.metrics?.last_resync_required_at_unix_ms) }}
            </span>
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Checkpoint Saves / Loads</div>
          <div class="runtime-value">
            {{ replayDiagnostics.metrics?.checkpoint_saves ?? 0 }} / {{ replayDiagnostics.metrics?.checkpoint_loads ?? 0 }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Checkpoint Heartbeats</div>
          <div class="runtime-value">{{ replayDiagnostics.metrics?.checkpoint_heartbeats ?? 0 }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Pruned / Expired Misses</div>
          <div class="runtime-value">
            {{ replayDiagnostics.metrics?.pruned_checkpoint_count ?? 0 }} / {{ replayDiagnostics.metrics?.expired_checkpoint_misses ?? 0 }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Persisted Catch-up</div>
          <div class="runtime-value">
            {{ formatTimestampAge(replayDiagnostics.metrics?.last_persisted_catch_up_at_unix_ms) }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Resync</div>
          <div class="runtime-value">
            {{ formatTimestampAge(replayDiagnostics.metrics?.last_resync_required_at_unix_ms) }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Checkpoint Prune</div>
          <div class="runtime-value">
            {{ formatTimestampAge(replayDiagnostics.metrics?.last_checkpoint_prune_at_unix_ms) }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Compaction</div>
          <div class="runtime-value">
            {{ formatTimestampAge(replayDiagnostics.last_compacted_at_unix_ms) }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Checkpoint Heartbeat</div>
          <div class="runtime-value">
            {{ formatTimestampAge(replayDiagnostics.metrics?.last_checkpoint_heartbeat_at_unix_ms) }}
          </div>
        </div>
        <div class="runtime-card runtime-card-wide" v-if="(replayDiagnostics.checkpoints?.length ?? 0) > 0">
          <div class="runtime-label">Replay Checkpoints</div>
          <div class="runtime-value">
            {{ formatReplayCheckpoints(replayDiagnostics.checkpoints) }}
          </div>
        </div>
      </div>
      <div class="runtime-grid" v-if="progressPersistenceDiagnostics">
        <div class="runtime-card">
          <div class="runtime-label">Progress Flush Policy</div>
          <div class="runtime-value">
            {{ formatDuration(progressPersistenceDiagnostics.flush_interval_ms ?? 0) }}
            or
            {{ formatSize(progressPersistenceDiagnostics.flush_threshold_bytes ?? 0) }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Pending Progress Writes</div>
          <div class="runtime-value">{{ progressPersistenceDiagnostics.pending_transfer_count ?? 0 }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Queued / Coalesced</div>
          <div class="runtime-value">
            {{ progressPersistenceDiagnostics.schedule_requests ?? 0 }} / {{ progressPersistenceDiagnostics.coalesced_updates ?? 0 }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Interval / Threshold Flushes</div>
          <div class="runtime-value">
            {{ progressPersistenceDiagnostics.interval_flushes ?? 0 }} / {{ progressPersistenceDiagnostics.threshold_flushes ?? 0 }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Force / Terminal Flushes</div>
          <div class="runtime-value">
            {{ progressPersistenceDiagnostics.force_flushes ?? 0 }} / {{ progressPersistenceDiagnostics.terminal_flushes ?? 0 }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Successful / Failed Writes</div>
          <div class="runtime-value">
            {{ progressPersistenceDiagnostics.successful_writes ?? 0 }} / {{ progressPersistenceDiagnostics.failed_writes ?? 0 }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Progress Flush</div>
          <div class="runtime-value">
            {{ formatTimestampAge(progressPersistenceDiagnostics.last_flush_at_unix_ms) }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Forced Flush</div>
          <div class="runtime-value">
            {{ formatTimestampAge(progressPersistenceDiagnostics.last_force_flush_at_unix_ms) }}
          </div>
        </div>
      </div>
      <div class="runtime-grid" v-if="linkCapabilities">
        <div class="runtime-card">
          <div class="runtime-label">BLE Baseline</div>
          <div class="runtime-value">{{ linkCapabilities.ble_baseline_enabled ? 'Enabled' : 'Diagnostics only / disabled' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">BLE Provider</div>
          <div class="runtime-value">{{ linkCapabilities.provider_name || 'uninitialized' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Scanner / Advertiser</div>
          <div class="runtime-value">
            {{ linkCapabilities.scanner_state || 'idle' }} / {{ linkCapabilities.advertiser_state || 'idle' }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">BLE Support</div>
          <div class="runtime-value">{{ linkCapabilities.ble_supported ? 'Supported' : 'Not supported' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">BLE Permission</div>
          <div class="runtime-value">{{ formatBlePermissionState(linkCapabilities.ble_permission_state) }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">BLE Runtime Availability</div>
          <div class="runtime-value">{{ linkCapabilities.ble_runtime_available ? 'Available' : 'Fallback to QR / short code' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">BLE Bridge Mode</div>
          <div class="runtime-value">{{ linkCapabilities.bridge_mode || 'direct provider only' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">P2P / SoftAP Capability</div>
          <div class="runtime-value">
            {{ linkCapabilities.p2p_capable ? 'P2P capable' : 'P2P unavailable' }} ·
            {{ linkCapabilities.softap_capable ? 'SoftAP capable' : 'SoftAP unavailable' }}
          </div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Single Radio Risk</div>
          <div class="runtime-value">{{ linkCapabilities.single_radio_risk ? 'Present' : 'Not detected' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Observed Capsules</div>
          <div class="runtime-value">{{ linkCapabilities.observed_capsule_count ?? 0 }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Capsule Ingest</div>
          <div class="runtime-value">{{ formatTimestampAge(linkCapabilities.last_capsule_ingested_at_unix_ms) }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Observation Prune</div>
          <div class="runtime-value">{{ formatTimestampAge(linkCapabilities.last_observation_prune_at_unix_ms) }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Bridge Snapshot</div>
          <div class="runtime-value">{{ formatTimestampAge(linkCapabilities.last_bridge_snapshot_at_unix_ms) }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Last Advertisement Refresh</div>
          <div class="runtime-value">{{ formatTimestampAge(linkCapabilities.last_advertisement_request_at_unix_ms) }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Rolling Identifier</div>
          <div class="runtime-value">{{ linkCapabilities.rolling_identifier_mode || 'disabled' }}</div>
        </div>
        <div class="runtime-card">
          <div class="runtime-label">Ephemeral Capsule</div>
          <div class="runtime-value">{{ linkCapabilities.ephemeral_capsule_mode || 'disabled' }}</div>
        </div>
        <div class="runtime-card" v-if="bleAssistCapsule">
          <div class="runtime-label">Rolling Identifier</div>
          <div class="runtime-value">{{ bleAssistCapsule.rolling_identifier }}</div>
        </div>
        <div class="runtime-card" v-if="bleAssistCapsule">
          <div class="runtime-label">Capsule Integrity Tag</div>
          <div class="runtime-value">{{ bleAssistCapsule.integrity_tag }}</div>
        </div>
        <div class="runtime-card" v-if="bleAssistCapsule">
          <div class="runtime-label">Capsule Lifetime</div>
          <div class="runtime-value">
            {{ formatDuration(bleAssistCapsule.rotation_window_ms) }} rotation · expires
            {{ formatAbsoluteTimestamp(bleAssistCapsule.expires_at_unix_ms) }}
          </div>
        </div>
        <div class="runtime-card runtime-card-wide" v-if="(linkCapabilities.notes?.length ?? 0) > 0">
          <div class="runtime-label">Link Capability Notes</div>
          <div class="runtime-value">{{ linkCapabilities.notes?.join(' · ') }}</div>
        </div>
        <div class="runtime-card runtime-card-wide" v-if="linkCapabilities.bridge_file_path">
          <div class="runtime-label">BLE Bridge Source</div>
          <div class="runtime-value">{{ linkCapabilities.bridge_file_path }}</div>
        </div>
        <div class="runtime-card runtime-card-wide" v-if="linkCapabilities.advertisement_file_path">
          <div class="runtime-label">BLE Advertisement Source</div>
          <div class="runtime-value">{{ linkCapabilities.advertisement_file_path }}</div>
        </div>
        <div class="runtime-card runtime-card-wide" v-if="linkCapabilities.advertised_rolling_identifier">
          <div class="runtime-label">Advertised Rolling Identifier</div>
          <div class="runtime-value">{{ linkCapabilities.advertised_rolling_identifier }}</div>
        </div>
        <div class="runtime-card runtime-card-wide" v-if="linkCapabilities.last_error">
          <div class="runtime-label">BLE Provider Error</div>
          <div class="runtime-value">{{ linkCapabilities.last_error }}</div>
        </div>
        <div class="runtime-card runtime-card-wide" v-if="(linkCapabilities.recent_capsules?.length ?? 0) > 0">
          <div class="runtime-label">Recent Observed Capsules</div>
          <div class="runtime-value">
            {{
              linkCapabilities.recent_capsules
                ?.map((capsule) => `${capsule.rolling_identifier} (${capsule.transport_hint}) @ ${formatAbsoluteTimestamp(capsule.last_seen_at_unix_ms)}`)
                .join(' · ')
            }}
          </div>
        </div>
      </div>
      <div class="actions">
        <button @click="loadRuntime" class="btn btn-secondary">Refresh Runtime</button>
        <button @click="copyDiscoveryDiagnostics" class="btn btn-secondary">Copy Discovery Diagnostics</button>
        <button @click="save" class="btn btn-primary">Save Changes</button>
      </div>
    </main>
    <PairingQrModal
      :open="showPairingQr"
      :device-name="localIdentity?.device_name || form.device_name"
      :verification-code="verificationCode"
      :pairing-uri="pairingUri"
      @close="showPairingQr = false"
      @copy-uri="copyPairingLink"
    />
    <PairingImportModal
      :open="showPairingImport"
      :local-fingerprint="localIdentity?.fingerprint || ''"
      :local-verification-code="verificationCode"
      :initial-input="pairingImportInitialInput"
      :busy="pairingImportBusy"
      @close="closePairingImport"
      @confirm="importPairingLink"
    />
  </div>
</template>

<style scoped>
.settings-view {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  background: var(--surface);
}

.app-header {
  display: flex;
  align-items: center;
  padding: 20px 22px 12px;
  border-bottom: 1px solid var(--border-subtle);
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
  margin: 14px 22px 22px;
  padding: 18px;
  border-radius: 12px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  flex: 1;
  min-height: 0;
  overflow-y: auto;
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
  background: #fff;
  color: var(--text-secondary);
  font-size: 1rem;
  width: 100%;
}

.path-picker {
  display: flex;
  gap: 12px;
}

.pairing-actions {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
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
  background: #fff;
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

  .pairing-actions .btn,
  .path-picker .btn {
    flex: 1;
  }
}
</style>
