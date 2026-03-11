function normalizedFingerprint(fingerprint: string): string {
  return String(fingerprint || "")
    .toUpperCase()
    .replace(/[^A-Z0-9]/g, "");
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
