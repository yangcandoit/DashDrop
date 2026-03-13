function normalizedFingerprint(fingerprint: string): string {
  return String(fingerprint || "")
    .toUpperCase()
    .replace(/[^A-Z0-9]/g, "");
}

function normalizedDeviceName(deviceName: string): string {
  return String(deviceName || "").trim();
}

export const PAIRING_LINK_TTL_MS = 10 * 60 * 1000;
export const PAIRING_LINK_TTL_MINUTES = Math.round(PAIRING_LINK_TTL_MS / 60_000);
export const PAIRING_LINK_FUTURE_SKEW_MS = 2 * 60 * 1000;

function encodeBase64Url(value: string): string {
  if (typeof window !== "undefined" && typeof window.btoa === "function") {
    const bytes = new TextEncoder().encode(value);
    let binary = "";
    for (const byte of bytes) {
      binary += String.fromCharCode(byte);
    }
    return window
      .btoa(binary)
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/g, "");
  }

  if (typeof Buffer !== "undefined") {
    return Buffer.from(value, "utf8")
      .toString("base64")
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/g, "");
  }

  return value;
}

function decodeBase64Url(value: string): string {
  const normalized = String(value || "")
    .trim()
    .replace(/-/g, "+")
    .replace(/_/g, "/");
  if (!normalized) {
    return "";
  }
  const padded = normalized + "=".repeat((4 - (normalized.length % 4 || 4)) % 4);

  if (typeof window !== "undefined" && typeof window.atob === "function") {
    const binary = window.atob(padded);
    const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
    return new TextDecoder().decode(bytes);
  }

  if (typeof Buffer !== "undefined") {
    return Buffer.from(padded, "base64").toString("utf8");
  }

  return padded;
}

function codeFromValue(value: string): string {
  if (!value) {
    return "0000-0000-0000";
  }

  const buckets = [0x1357, 0x2468, 0x369c];
  for (let index = 0; index < value.length; index += 1) {
    const bucket = index % buckets.length;
    const code = value.charCodeAt(index);
    buckets[bucket] = (buckets[bucket] * 131 + code + index * 17) % 10_000;
  }

  return buckets.map((part) => String(part).padStart(4, "0")).join("-");
}

export function verificationCodeFromFingerprint(fingerprint: string): string {
  return codeFromValue(normalizedFingerprint(fingerprint));
}

export function sharedVerificationCode(localFingerprint: string, peerFingerprint: string): string {
  const pair = [normalizedFingerprint(localFingerprint), normalizedFingerprint(peerFingerprint)]
    .filter(Boolean)
    .sort()
    .join("|");
  return codeFromValue(pair);
}

export type PairingQrPayload = {
  version: 1 | 2;
  fingerprint: string;
  device_name: string;
  verification_code: string;
  issued_at_unix_ms: number;
  expires_at_unix_ms?: number;
  trust_model?: "signed_link" | "legacy_unsigned";
  signature_verified?: boolean;
  signer_public_key?: string;
  signature?: string;
};

export function pairingPayloadExpiresAtUnixMs(payload: Pick<PairingQrPayload, "issued_at_unix_ms">): number {
  return Math.max(0, Math.trunc(payload.issued_at_unix_ms || 0)) + PAIRING_LINK_TTL_MS;
}

export function pairingQrPayloadFromIdentity(
  fingerprint: string,
  deviceName: string,
  issuedAtUnixMs = Date.now(),
): PairingQrPayload {
  const normalizedFp = normalizedFingerprint(fingerprint);
  return {
    version: 1,
    fingerprint: normalizedFp,
    device_name: normalizedDeviceName(deviceName),
    verification_code: verificationCodeFromFingerprint(normalizedFp),
    issued_at_unix_ms: issuedAtUnixMs,
  };
}

export function pairingUriFromIdentity(
  fingerprint: string,
  deviceName: string,
  issuedAtUnixMs = Date.now(),
): string {
  const payload = pairingQrPayloadFromIdentity(fingerprint, deviceName, issuedAtUnixMs);
  const encoded = encodeBase64Url(JSON.stringify(payload));
  return `dashdrop://pair?data=${encoded}`;
}

function assertPairingQrPayload(value: unknown): PairingQrPayload {
  const payload = value as Partial<PairingQrPayload> | null;
  if (!payload || (payload.version !== 1 && payload.version !== 2)) {
    throw new Error("Unsupported pairing payload version.");
  }
  const fingerprint = normalizedFingerprint(payload.fingerprint || "");
  if (!fingerprint) {
    throw new Error("Pairing payload fingerprint is missing.");
  }
  const deviceName = normalizedDeviceName(payload.device_name || "");
  if (!deviceName) {
    throw new Error("Pairing payload device name is missing.");
  }
  const verificationCode = String(payload.verification_code || "").trim();
  if (verificationCode !== verificationCodeFromFingerprint(fingerprint)) {
    throw new Error("Pairing payload verification code is invalid.");
  }
  const issuedAtUnixMs =
    typeof payload.issued_at_unix_ms === "number" && Number.isFinite(payload.issued_at_unix_ms)
      ? Math.max(0, Math.trunc(payload.issued_at_unix_ms))
      : 0;
  const basePayload: PairingQrPayload = {
    version: payload.version,
    fingerprint,
    device_name: deviceName,
    verification_code: verificationCode,
    issued_at_unix_ms: issuedAtUnixMs,
  };
  if (payload.version === 2) {
    const signerPublicKey = String(payload.signer_public_key || "").trim();
    const signature = String(payload.signature || "").trim();
    if (!signerPublicKey) {
      throw new Error("Pairing signer public key is missing.");
    }
    if (!signature) {
      throw new Error("Pairing link signature is missing.");
    }
    basePayload.signer_public_key = signerPublicKey;
    basePayload.signature = signature;
    basePayload.trust_model = "signed_link";
  }
  return basePayload;
}

function assertPairingPayloadFreshness(payload: PairingQrPayload, nowUnixMs = Date.now()) {
  if (payload.issued_at_unix_ms <= 0) {
    throw new Error("Pairing payload issue time is missing.");
  }

  if (payload.issued_at_unix_ms - nowUnixMs > PAIRING_LINK_FUTURE_SKEW_MS) {
    throw new Error("Pairing payload issue time is invalid.");
  }

  if (nowUnixMs > pairingPayloadExpiresAtUnixMs(payload)) {
    throw new Error("Pairing link expired. Ask the other device to generate a new QR or pairing link.");
  }
}

export function parsePairingInput(input: string): PairingQrPayload {
  const raw = String(input || "").trim();
  if (!raw) {
    throw new Error("Pairing link is empty.");
  }

  let payloadSource = raw;
  if (raw.startsWith("dashdrop://")) {
    const url = new URL(raw);
    if (url.protocol !== "dashdrop:" || url.hostname !== "pair") {
      throw new Error("Unsupported DashDrop pairing link.");
    }
    const data = url.searchParams.get("data");
    if (!data) {
      throw new Error("Pairing link is missing data.");
    }
    payloadSource = decodeBase64Url(data);
  } else if (!raw.startsWith("{")) {
    payloadSource = decodeBase64Url(raw);
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(payloadSource);
  } catch {
    throw new Error("Pairing payload is not valid JSON.");
  }

  const payload = assertPairingQrPayload(parsed);
  assertPairingPayloadFreshness(payload);
  payload.expires_at_unix_ms = pairingPayloadExpiresAtUnixMs(payload);
  if (payload.trust_model === undefined) {
    payload.trust_model = payload.version === 2 ? "signed_link" : "legacy_unsigned";
  }
  if (payload.signature_verified === undefined) {
    payload.signature_verified = false;
  }
  return payload;
}
