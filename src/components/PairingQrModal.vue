<script setup lang="ts">
import { ref, watch } from 'vue';
import QRCode from 'qrcode';
import { PAIRING_LINK_TTL_MINUTES } from '../security';

const props = defineProps<{
  open: boolean;
  deviceName: string;
  verificationCode: string;
  pairingUri: string;
}>();

const emit = defineEmits<{
  (e: 'close'): void;
  (e: 'copy-uri'): void;
}>();

const qrSvg = ref('');
const qrError = ref<string | null>(null);
const qrLoading = ref(false);

async function renderQr() {
  if (!props.open || !props.pairingUri) {
    qrSvg.value = '';
    qrError.value = null;
    return;
  }

  qrLoading.value = true;
  qrError.value = null;
  try {
    qrSvg.value = await QRCode.toString(props.pairingUri, {
      type: 'svg',
      errorCorrectionLevel: 'M',
      margin: 1,
      width: 320,
      color: {
        dark: '#1f2b21',
        light: '#ffffff',
      },
    });
  } catch (error) {
    console.error('Failed to render pairing QR', error);
    qrSvg.value = '';
    qrError.value = 'Unable to render pairing QR right now.';
  } finally {
    qrLoading.value = false;
  }
}

watch(
  () => [props.open, props.pairingUri] as const,
  () => {
    void renderQr();
  },
  { immediate: true },
);
</script>

<template>
  <div v-if="props.open" class="dialog-backdrop" @click.self="emit('close')">
    <section class="dialog-card pairing-card">
      <div class="pairing-header">
        <div>
          <h3>Pair This Device</h3>
          <p class="text-muted pairing-copy">
            Scan this QR on another DashDrop device, or copy the pairing link and move it out-of-band.
          </p>
        </div>
        <button class="btn btn-secondary" @click="emit('close')">Close</button>
      </div>

      <div class="pairing-summary">
        <div class="summary-chip">
          <span class="summary-label">Device</span>
          <strong>{{ props.deviceName || 'DashDrop Device' }}</strong>
        </div>
        <div class="summary-chip">
          <span class="summary-label">Verify</span>
          <strong>{{ props.verificationCode }}</strong>
        </div>
      </div>

      <div class="pairing-body">
        <div class="qr-shell">
          <div v-if="qrLoading" class="qr-placeholder">Rendering QR…</div>
          <div v-else-if="qrError" class="qr-placeholder qr-error">{{ qrError }}</div>
          <div v-else class="qr-svg" v-html="qrSvg"></div>
        </div>
        <div class="pairing-side">
          <label class="pairing-label">Pairing Link</label>
          <textarea class="pairing-uri" :value="props.pairingUri" readonly />
          <p class="text-muted pairing-hint">
            This pairing link is signed by this device identity. After the other device imports it, compare both the short verification code and the shared pair code on both screens before trusting. Pairing links expire after about {{ PAIRING_LINK_TTL_MINUTES }} minutes, so generate a fresh one if the other device says it expired.
          </p>
          <button class="btn btn-primary" @click="emit('copy-uri')">Copy Pairing Link</button>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.dialog-backdrop {
  position: absolute;
  inset: 0;
  background: rgba(33, 30, 24, 0.4);
  backdrop-filter: blur(8px);
  display: flex;
  justify-content: center;
  align-items: center;
  padding: 18px;
  z-index: 60;
}

.dialog-card {
  width: min(760px, 100%);
  border-radius: 20px;
  border: 1px solid var(--border-subtle);
  background: #fff;
  box-shadow: var(--shadow-soft);
  padding: 20px;
}

.pairing-card {
  display: flex;
  flex-direction: column;
  gap: 18px;
}

.pairing-header {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  align-items: flex-start;
}

.pairing-copy {
  margin-top: 6px;
}

.pairing-summary {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;
}

.summary-chip {
  border-radius: 14px;
  padding: 12px 14px;
  border: 1px solid rgba(50, 63, 48, 0.12);
  background: linear-gradient(180deg, rgba(246, 243, 235, 0.92), rgba(250, 249, 245, 0.98));
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.summary-label,
.pairing-label {
  font-size: 0.78rem;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: rgba(72, 82, 68, 0.82);
}

.pairing-body {
  display: grid;
  grid-template-columns: minmax(240px, 320px) minmax(0, 1fr);
  gap: 16px;
  align-items: stretch;
}

.qr-shell {
  min-height: 320px;
  border-radius: 18px;
  border: 1px solid rgba(50, 63, 48, 0.12);
  background:
    radial-gradient(circle at top left, rgba(207, 223, 195, 0.42), transparent 52%),
    linear-gradient(180deg, rgba(250, 248, 242, 0.98), rgba(243, 239, 228, 0.92));
  display: flex;
  justify-content: center;
  align-items: center;
  padding: 18px;
}

.qr-svg :deep(svg) {
  width: 100%;
  height: auto;
  max-width: 280px;
  display: block;
}

.qr-placeholder {
  color: rgba(56, 67, 53, 0.72);
  font-size: 0.95rem;
}

.qr-error {
  color: #9a3f35;
}

.pairing-side {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.pairing-uri {
  min-height: 150px;
  resize: none;
  border-radius: 14px;
  border: 1px solid var(--border-subtle);
  padding: 12px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.82rem;
  line-height: 1.4;
  color: #2c312d;
  background: #fbfaf7;
}

.pairing-hint {
  margin: 0;
}

@media (max-width: 860px) {
  .pairing-header,
  .pairing-body {
    grid-template-columns: 1fr;
    display: grid;
  }

  .pairing-summary {
    grid-template-columns: 1fr;
  }

  .pairing-header {
    align-items: stretch;
  }

  .pairing-header .btn {
    width: 100%;
  }
}
</style>
