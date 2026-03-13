<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue';
import jsQR, { type QRCode } from 'jsqr';
import { validatePairingInput as validatePairingInputCommand } from '../ipc';
import type { PairingQrPayload } from '../security';
import { PAIRING_LINK_TTL_MINUTES, sharedVerificationCode } from '../security';

const props = defineProps<{
  open: boolean;
  localFingerprint: string;
  localVerificationCode: string;
  initialInput?: string;
  busy?: boolean;
}>();

const emit = defineEmits<{
  (e: 'close'): void;
  (
    e: 'confirm',
    payload: { payload: PairingQrPayload; mutualConfirmationRequested: boolean },
  ): void;
}>();

const pairingInput = ref('');
const parsedPayload = ref<PairingQrPayload | null>(null);
const parseError = ref<string | null>(null);
const validationBusy = ref(false);
const verified = ref(false);
const mutualConfirmationRequested = ref(false);
const scannerError = ref<string | null>(null);
const scannerNotice = ref<string | null>(null);
const scannerBusy = ref(false);
const cameraActive = ref(false);
const cameraLoading = ref(false);
const fileInput = ref<HTMLInputElement | null>(null);
const cameraVideo = ref<HTMLVideoElement | null>(null);
const cameraOverlay = ref<HTMLCanvasElement | null>(null);
const scanSuccessVisible = ref(false);
let cameraStream: MediaStream | null = null;
let cameraScanTimer: number | null = null;
let scanSuccessTimer: number | null = null;
let detectorBusy = false;
let validateRequestVersion = 0;

type QrDetection = {
  value: string;
  location?: QRCode['location'] | null;
  decoder: 'barcode_detector' | 'jsqr';
};

const issuedAtLabel = computed(() => {
  if (!parsedPayload.value?.issued_at_unix_ms) {
    return 'Unknown';
  }
  return new Date(parsedPayload.value.issued_at_unix_ms).toLocaleString();
});

const sharedPairingCode = computed(() => {
  if (!props.localFingerprint || !parsedPayload.value?.fingerprint) {
    return '';
  }
  return sharedVerificationCode(props.localFingerprint, parsedPayload.value.fingerprint);
});

const barcodeDetectorSupported = computed(
  () => typeof window !== 'undefined' && typeof window.BarcodeDetector === 'function',
);

const fileScanningSupported = computed(
  () => typeof window !== 'undefined' && typeof window.createImageBitmap === 'function',
);

const cameraScanningSupported = computed(
  () => typeof navigator !== 'undefined' && Boolean(navigator.mediaDevices?.getUserMedia),
);

function clearScannerFeedback() {
  scannerError.value = null;
  scannerNotice.value = null;
}

function clearScanSuccess() {
  if (scanSuccessTimer !== null) {
    window.clearTimeout(scanSuccessTimer);
    scanSuccessTimer = null;
  }
  scanSuccessVisible.value = false;
}

function showScanSuccess() {
  clearScanSuccess();
  scanSuccessVisible.value = true;
  scanSuccessTimer = window.setTimeout(() => {
    scanSuccessVisible.value = false;
    scanSuccessTimer = null;
  }, 1_600);
}

function resetState() {
  pairingInput.value = '';
  parsedPayload.value = null;
  parseError.value = null;
  validationBusy.value = false;
  verified.value = false;
  mutualConfirmationRequested.value = false;
  clearScannerFeedback();
  clearScanSuccess();
  stopCamera();
}

async function validateCurrentInput() {
  const requestVersion = ++validateRequestVersion;
  const raw = pairingInput.value.trim();
  if (!raw) {
    parsedPayload.value = null;
    parseError.value = null;
    validationBusy.value = false;
    verified.value = false;
    mutualConfirmationRequested.value = false;
    return;
  }

  validationBusy.value = true;
  try {
    const payload = await validatePairingInputCommand(raw);
    if (requestVersion !== validateRequestVersion) {
      return;
    }
    parsedPayload.value = payload;
    parseError.value = null;
  } catch (error) {
    if (requestVersion !== validateRequestVersion) {
      return;
    }
    parsedPayload.value = null;
    parseError.value = error instanceof Error ? error.message : String(error);
    verified.value = false;
    mutualConfirmationRequested.value = false;
  } finally {
    if (requestVersion === validateRequestVersion) {
      validationBusy.value = false;
    }
  }
}

function confirmImport() {
  if (!parsedPayload.value || !verified.value || props.busy) {
    return;
  }
  emit('confirm', {
    payload: parsedPayload.value,
    mutualConfirmationRequested:
      parsedPayload.value.trust_model === 'signed_link' && mutualConfirmationRequested.value,
  });
}

function createBarcodeDetector(): BarcodeDetector {
  if (!barcodeDetectorSupported.value || !window.BarcodeDetector) {
    throw new Error('QR scanning is not available in this runtime. Paste the pairing link instead.');
  }
  return new window.BarcodeDetector({ formats: ['qr_code'] });
}

function createDecodeCanvas(width: number, height: number) {
  const canvas = document.createElement('canvas');
  canvas.width = Math.max(1, Math.trunc(width));
  canvas.height = Math.max(1, Math.trunc(height));
  const context = canvas.getContext('2d', { willReadFrequently: true });
  if (!context) {
    throw new Error('Unable to initialize an image decoder canvas.');
  }
  return { canvas, context };
}

function decodeQrFromImageData(imageData: ImageData): QRCode | null {
  return jsQR(imageData.data, imageData.width, imageData.height, {
    inversionAttempts: 'attemptBoth',
  });
}

async function detectQrFromBitmap(bitmap: ImageBitmap): Promise<QrDetection | null> {
  if (barcodeDetectorSupported.value) {
    try {
      const detector = createBarcodeDetector();
      const codes = await detector.detect(bitmap);
      const qrValue = codes.find((code) => typeof code.rawValue === 'string' && code.rawValue.trim())?.rawValue;
      if (qrValue) {
        return {
          value: qrValue.trim(),
          location: null,
          decoder: 'barcode_detector',
        };
      }
    } catch (error) {
      console.debug('BarcodeDetector bitmap decode failed, falling back to jsQR.', error);
    }
  }

  const { context } = createDecodeCanvas(bitmap.width, bitmap.height);
  context.drawImage(bitmap, 0, 0, bitmap.width, bitmap.height);
  const decoded = decodeQrFromImageData(context.getImageData(0, 0, bitmap.width, bitmap.height));
  if (!decoded?.data?.trim()) {
    return null;
  }
  return {
    value: decoded.data.trim(),
    location: decoded.location,
    decoder: 'jsqr',
  };
}

function paintCameraOverlay(location?: QRCode['location'] | null) {
  const overlay = cameraOverlay.value;
  const video = cameraVideo.value;
  if (!overlay || !video) {
    return;
  }

  const width = Math.max(1, Math.trunc(video.clientWidth));
  const height = Math.max(1, Math.trunc(video.clientHeight));
  overlay.width = width;
  overlay.height = height;

  const context = overlay.getContext('2d');
  if (!context) {
    return;
  }

  context.clearRect(0, 0, width, height);
  if (!location || !video.videoWidth || !video.videoHeight) {
    return;
  }

  const scale = Math.max(width / video.videoWidth, height / video.videoHeight);
  const offsetX = (width - video.videoWidth * scale) / 2;
  const offsetY = (height - video.videoHeight * scale) / 2;
  const project = (point: { x: number; y: number }) => ({
    x: point.x * scale + offsetX,
    y: point.y * scale + offsetY,
  });
  const corners = [
    project(location.topLeftCorner),
    project(location.topRightCorner),
    project(location.bottomRightCorner),
    project(location.bottomLeftCorner),
  ];

  context.beginPath();
  context.moveTo(corners[0].x, corners[0].y);
  for (const corner of corners.slice(1)) {
    context.lineTo(corner.x, corner.y);
  }
  context.closePath();
  context.lineWidth = 3;
  context.strokeStyle = 'rgba(80, 219, 148, 0.95)';
  context.shadowColor = 'rgba(80, 219, 148, 0.45)';
  context.shadowBlur = 18;
  context.stroke();

  for (const corner of corners) {
    context.beginPath();
    context.fillStyle = '#ffffff';
    context.arc(corner.x, corner.y, 4, 0, Math.PI * 2);
    context.fill();
  }
}

async function detectQrFromVideo(video: HTMLVideoElement): Promise<QrDetection | null> {
  const width = Math.max(1, Math.trunc(video.videoWidth || video.clientWidth));
  const height = Math.max(1, Math.trunc(video.videoHeight || video.clientHeight));
  if (!width || !height) {
    return null;
  }

  if (barcodeDetectorSupported.value) {
    try {
      const detector = createBarcodeDetector();
      const codes = await detector.detect(video);
      const qrValue = codes.find((code) => typeof code.rawValue === 'string' && code.rawValue.trim())?.rawValue;
      if (qrValue) {
        return {
          value: qrValue.trim(),
          location: null,
          decoder: 'barcode_detector',
        };
      }
    } catch (error) {
      console.debug('BarcodeDetector video decode failed, falling back to jsQR.', error);
    }
  }

  const { context } = createDecodeCanvas(width, height);
  context.drawImage(video, 0, 0, width, height);
  const decoded = decodeQrFromImageData(context.getImageData(0, 0, width, height));
  if (!decoded?.data?.trim()) {
    return null;
  }
  return {
    value: decoded.data.trim(),
    location: decoded.location,
    decoder: 'jsqr',
  };
}

async function applyScannedValue(rawValue: string, sourceLabel: string) {
  pairingInput.value = rawValue.trim();
  await validateCurrentInput();
  verified.value = false;
  mutualConfirmationRequested.value = false;
  if (parseError.value) {
    throw new Error(parseError.value);
  }
  showScanSuccess();
  scannerNotice.value = `${sourceLabel} loaded. Confirm the remote verification code before trusting it.`;
  scannerError.value = null;
}

async function scanImageFile(file: File) {
  scannerBusy.value = true;
  clearScannerFeedback();
  try {
    if (!fileScanningSupported.value) {
      throw new Error('This runtime cannot decode QR images directly. Paste the pairing link instead.');
    }
    const bitmap = await createImageBitmap(file);
    try {
      const detection = await detectQrFromBitmap(bitmap);
      if (!detection) {
        throw new Error('No QR code was detected in that image.');
      }
      await applyScannedValue(detection.value, 'QR image');
    } finally {
      bitmap.close();
    }
  } catch (error) {
    scannerError.value = error instanceof Error ? error.message : String(error);
    scannerNotice.value = null;
  } finally {
    scannerBusy.value = false;
    if (fileInput.value) {
      fileInput.value.value = '';
    }
  }
}

function triggerImagePicker() {
  clearScannerFeedback();
  fileInput.value?.click();
}

function onImageFileSelected(event: Event) {
  const input = event.target as HTMLInputElement | null;
  const file = input?.files?.[0];
  if (!file) {
    return;
  }
  void scanImageFile(file);
}

function stopCamera() {
  if (cameraScanTimer !== null) {
    window.clearTimeout(cameraScanTimer);
    cameraScanTimer = null;
  }
  if (cameraStream) {
    for (const track of cameraStream.getTracks()) {
      track.stop();
    }
    cameraStream = null;
  }
  if (cameraVideo.value) {
    cameraVideo.value.srcObject = null;
  }
  paintCameraOverlay(null);
  detectorBusy = false;
  cameraActive.value = false;
  cameraLoading.value = false;
}

async function scanCameraFrame() {
  if (!cameraActive.value || !cameraVideo.value || detectorBusy) {
    return;
  }
  detectorBusy = true;
  try {
    const detection = await detectQrFromVideo(cameraVideo.value);
    paintCameraOverlay(detection?.location);
    if (detection) {
      await applyScannedValue(detection.value, 'Camera scan');
      stopCamera();
      return;
    }
  } catch (error) {
    scannerError.value = error instanceof Error ? error.message : String(error);
    scannerNotice.value = null;
    stopCamera();
    return;
  } finally {
    detectorBusy = false;
  }

  cameraScanTimer = window.setTimeout(() => {
    void scanCameraFrame();
  }, 250);
}

async function startCameraScan() {
  clearScannerFeedback();
  if (!cameraScanningSupported.value) {
    scannerError.value = 'This runtime does not expose camera access. Paste the pairing link instead.';
    return;
  }

  stopCamera();
  cameraLoading.value = true;
  try {
    cameraStream = await navigator.mediaDevices.getUserMedia({
      video: {
        facingMode: 'environment',
      },
      audio: false,
    });
    cameraActive.value = true;
    await nextTick();
    if (!cameraVideo.value) {
      throw new Error('Camera preview failed to start.');
    }
    cameraVideo.value.srcObject = cameraStream;
    await cameraVideo.value.play();
    paintCameraOverlay(null);
    scannerNotice.value = 'Point the camera at the other device QR code and keep it inside the guide frame.';
    scannerError.value = null;
    void scanCameraFrame();
  } catch (error) {
    stopCamera();
    scannerError.value =
      error instanceof Error ? error.message : 'Unable to access the camera for QR scanning.';
    scannerNotice.value = null;
  } finally {
    cameraLoading.value = false;
  }
}

watch(
  () => props.open,
  (open) => {
    if (open) {
      pairingInput.value = props.initialInput?.trim() || '';
      void validateCurrentInput();
      verified.value = false;
      mutualConfirmationRequested.value = false;
      clearScannerFeedback();
    } else {
      resetState();
    }
  },
);

watch(
  () => props.initialInput,
  (value) => {
    if (!props.open) {
      return;
    }
    pairingInput.value = value?.trim() || '';
    void validateCurrentInput();
    verified.value = false;
    mutualConfirmationRequested.value = false;
  },
);

watch(pairingInput, () => {
  void validateCurrentInput();
});

onBeforeUnmount(() => {
  stopCamera();
});
</script>

<template>
  <div v-if="props.open" class="dialog-backdrop" @click.self="emit('close')">
    <section class="dialog-card pairing-card">
      <div class="pairing-header">
        <div>
          <h3>Import Pairing Link</h3>
          <p class="text-muted pairing-copy">
            Paste a DashDrop pairing link, load a QR image, or scan the other device QR code.
          </p>
        </div>
        <button class="btn btn-secondary" @click="emit('close')" :disabled="props.busy">Close</button>
      </div>

      <div class="pairing-body import-body">
        <div class="pairing-side">
          <div class="scanner-actions">
            <div>
              <label class="pairing-label">Pairing Input</label>
              <p class="text-muted pairing-hint">
                Imported QR content is validated before trust can be confirmed.
              </p>
            </div>
            <div class="scanner-button-row">
              <input
                ref="fileInput"
                type="file"
                accept="image/*"
                class="scanner-file-input"
                @change="onImageFileSelected"
              />
              <button class="btn btn-secondary" @click="triggerImagePicker" :disabled="props.busy || scannerBusy">
                {{ scannerBusy ? 'Scanning…' : 'Import QR Image' }}
              </button>
              <button
                class="btn btn-secondary"
                @click="cameraActive ? stopCamera() : startCameraScan()"
                :disabled="props.busy || scannerBusy || cameraLoading"
              >
                {{
                  cameraLoading
                    ? 'Opening Camera…'
                    : cameraActive
                      ? 'Stop Camera'
                      : 'Use Camera'
                }}
              </button>
            </div>
          </div>

          <textarea
            v-model="pairingInput"
            class="pairing-uri"
            placeholder="Paste dashdrop://pair?... here"
            :disabled="props.busy"
          />

          <div v-if="cameraActive" class="camera-preview">
            <video ref="cameraVideo" class="camera-video" autoplay muted playsinline />
            <canvas ref="cameraOverlay" class="camera-overlay" />
            <div class="camera-guide" :class="{ success: scanSuccessVisible }" aria-hidden="true">
              <div class="guide-corner top-left"></div>
              <div class="guide-corner top-right"></div>
              <div class="guide-corner bottom-left"></div>
              <div class="guide-corner bottom-right"></div>
              <div class="guide-label">{{ scanSuccessVisible ? 'QR detected' : 'Align QR here' }}</div>
            </div>
          </div>

          <p v-if="scannerError" class="import-error">{{ scannerError }}</p>
          <p v-else-if="scannerNotice" class="scanner-notice">{{ scannerNotice }}</p>
          <p v-else-if="validationBusy" class="scanner-notice">Validating pairing link signature and freshness…</p>
          <p v-if="parseError" class="import-error">{{ parseError }}</p>
          <p v-else class="text-muted pairing-hint">
            DashDrop validates the embedded verification code, freshness window, and signed-link metadata before allowing trust.
          </p>
          <p v-if="!barcodeDetectorSupported" class="text-muted pairing-hint">
            Native QR detection is unavailable here, so DashDrop is using a pure-JavaScript decoder fallback.
          </p>
          <p v-if="!fileScanningSupported" class="text-muted pairing-hint">
            This runtime cannot decode uploaded QR images, so use camera scan or paste the pairing link directly.
          </p>
          <p v-if="!cameraScanningSupported" class="text-muted pairing-hint">
            Camera access is unavailable in this runtime, so use QR image import or paste the pairing link directly.
          </p>
          <p class="text-muted pairing-hint">
            Pairing links stay valid for about {{ PAIRING_LINK_TTL_MINUTES }} minutes.
          </p>
        </div>

        <div class="import-preview">
          <div class="summary-chip">
            <span class="summary-label">This Device Code</span>
            <strong>{{ props.localVerificationCode || 'Unavailable' }}</strong>
          </div>
          <div class="summary-chip">
            <span class="summary-label">Remote Device</span>
            <strong>{{ parsedPayload?.device_name || 'Waiting for valid pairing link' }}</strong>
          </div>
          <div class="summary-chip">
            <span class="summary-label">Remote Verify</span>
            <strong>{{ parsedPayload?.verification_code || '---- ---- ----' }}</strong>
          </div>
          <div class="summary-chip">
            <span class="summary-label">Issued</span>
            <strong>{{ issuedAtLabel }}</strong>
          </div>
          <div class="summary-chip">
            <span class="summary-label">Trust Model</span>
            <strong>
              {{
                parsedPayload?.trust_model === 'signed_link'
                  ? (parsedPayload.signature_verified ? 'Signed link verified' : 'Signed link')
                  : 'Legacy unsigned link'
              }}
            </strong>
          </div>
          <div class="summary-chip">
            <span class="summary-label">Shared Pair Code</span>
            <strong>{{ sharedPairingCode || 'Waiting for valid pairing link' }}</strong>
          </div>
          <div class="summary-block" v-if="parsedPayload">
            <span class="summary-label">Fingerprint</span>
            <code class="fingerprint">{{ parsedPayload.fingerprint }}</code>
          </div>
          <label class="verify-row" :class="{ disabled: !parsedPayload }">
            <input v-model="verified" type="checkbox" :disabled="!parsedPayload || props.busy" />
            I compared the remote verification code and the shared pair code with the other device before trusting it.
          </label>
          <label
            v-if="parsedPayload?.trust_model === 'signed_link'"
            class="verify-row"
            :class="{ disabled: !parsedPayload }"
          >
            <input
              v-model="mutualConfirmationRequested"
              type="checkbox"
              :disabled="!parsedPayload || props.busy"
            />
            The other device also imported my signed pairing link and showed the same shared pair code.
          </label>
          <p v-if="sharedPairingCode" class="text-muted pairing-hint">
            If the other device also imports your pairing link, both import screens should show the same shared pair code.
          </p>
          <p
            v-if="parsedPayload?.trust_model === 'signed_link' && !mutualConfirmationRequested"
            class="text-muted pairing-hint"
          >
            Signed-link trust is recorded immediately, but it stays below mutual confirmation until both devices compare the same shared pair code.
          </p>
          <p v-if="parsedPayload?.trust_model === 'legacy_unsigned'" class="text-muted pairing-hint">
            This link uses the older unsigned format, so keep the manual code comparison strict before trusting it.
          </p>
          <div class="dialog-actions">
            <button class="btn btn-secondary" @click="emit('close')" :disabled="props.busy">Cancel</button>
            <button class="btn btn-primary" @click="confirmImport" :disabled="!parsedPayload || !verified || props.busy || validationBusy">
              {{ props.busy ? 'Pairing…' : 'Trust Device' }}
            </button>
          </div>
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
  width: min(860px, 100%);
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

.pairing-label,
.summary-label {
  font-size: 0.78rem;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: rgba(72, 82, 68, 0.82);
}

.pairing-body {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.pairing-side,
.import-preview {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.scanner-actions {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  align-items: flex-start;
}

.scanner-button-row {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  justify-content: flex-end;
}

.scanner-file-input {
  display: none;
}

.pairing-uri {
  min-height: 220px;
  resize: vertical;
  border-radius: 14px;
  border: 1px solid var(--border-subtle);
  padding: 12px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.82rem;
  line-height: 1.4;
  color: #2c312d;
  background: #fbfaf7;
}

.camera-preview {
  position: relative;
  border-radius: 16px;
  overflow: hidden;
  border: 1px solid rgba(50, 63, 48, 0.12);
  background: #111;
  min-height: 220px;
}

.camera-video {
  display: block;
  width: 100%;
  max-height: 320px;
  object-fit: cover;
}

.camera-overlay {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  pointer-events: none;
}

.camera-guide {
  position: absolute;
  inset: 14px;
  display: flex;
  align-items: center;
  justify-content: center;
  pointer-events: none;
  transition: transform 180ms ease, opacity 180ms ease;
}

.camera-guide::before {
  content: '';
  position: absolute;
  inset: 16% 18%;
  border-radius: 18px;
  border: 2px solid rgba(255, 255, 255, 0.34);
  background: linear-gradient(180deg, rgba(255, 255, 255, 0.02), rgba(255, 255, 255, 0.08));
  box-shadow: inset 0 0 0 1px rgba(0, 0, 0, 0.16);
}

.camera-guide.success {
  transform: scale(1.02);
}

.camera-guide.success::before {
  border-color: rgba(80, 219, 148, 0.9);
  box-shadow:
    0 0 0 2px rgba(80, 219, 148, 0.18),
    inset 0 0 0 1px rgba(80, 219, 148, 0.22);
}

.guide-corner {
  position: absolute;
  width: 26px;
  height: 26px;
  border-color: rgba(255, 255, 255, 0.92);
  border-style: solid;
  border-width: 0;
}

.camera-guide.success .guide-corner {
  border-color: rgba(80, 219, 148, 0.96);
}

.guide-corner.top-left {
  top: 16%;
  left: 18%;
  border-top-width: 4px;
  border-left-width: 4px;
  border-top-left-radius: 12px;
}

.guide-corner.top-right {
  top: 16%;
  right: 18%;
  border-top-width: 4px;
  border-right-width: 4px;
  border-top-right-radius: 12px;
}

.guide-corner.bottom-left {
  bottom: 16%;
  left: 18%;
  border-bottom-width: 4px;
  border-left-width: 4px;
  border-bottom-left-radius: 12px;
}

.guide-corner.bottom-right {
  bottom: 16%;
  right: 18%;
  border-bottom-width: 4px;
  border-right-width: 4px;
  border-bottom-right-radius: 12px;
}

.guide-label {
  position: absolute;
  bottom: 10%;
  padding: 6px 10px;
  border-radius: 999px;
  background: rgba(15, 18, 16, 0.66);
  color: #f6fbf7;
  font-size: 0.78rem;
  letter-spacing: 0.04em;
}

.camera-guide.success .guide-label {
  background: rgba(25, 93, 61, 0.82);
}

.summary-chip,
.summary-block {
  border-radius: 14px;
  padding: 12px 14px;
  border: 1px solid rgba(50, 63, 48, 0.12);
  background: linear-gradient(180deg, rgba(246, 243, 235, 0.92), rgba(250, 249, 245, 0.98));
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.summary-block {
  gap: 8px;
}

.fingerprint {
  overflow-wrap: anywhere;
  color: #223126;
  font-size: 0.86rem;
}

.verify-row {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  font-size: 0.92rem;
  color: #2f372f;
}

.verify-row.disabled {
  opacity: 0.55;
}

.import-error {
  margin: 0;
  color: #9a3f35;
  font-size: 0.9rem;
}

.scanner-notice {
  margin: 0;
  color: #23513d;
  font-size: 0.9rem;
}

.pairing-hint {
  margin: 0;
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  margin-top: auto;
}

@media (max-width: 860px) {
  .pairing-body {
    grid-template-columns: 1fr;
  }

  .pairing-header {
    flex-direction: column;
  }

  .scanner-actions {
    flex-direction: column;
  }

  .scanner-button-row {
    justify-content: flex-start;
  }

  .dialog-actions {
    flex-direction: column;
  }
}
</style>
