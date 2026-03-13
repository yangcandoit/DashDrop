import assert from "node:assert/strict";
import test from "node:test";

import {
  PAIRING_LINK_TTL_MS,
  pairingUriFromIdentity,
  parsePairingInput,
  verificationCodeFromFingerprint,
} from "../src/security.ts";

test("pairing URI round-trips into a validated pairing payload", () => {
  const issuedAt = Date.now();
  const uri = pairingUriFromIdentity("ab:cd:12:34", "Desk Mac", issuedAt);
  const payload = parsePairingInput(uri);

  assert.deepEqual(payload, {
    version: 1,
    fingerprint: "ABCD1234",
    device_name: "Desk Mac",
    verification_code: verificationCodeFromFingerprint("ABCD1234"),
    issued_at_unix_ms: issuedAt,
    expires_at_unix_ms: issuedAt + PAIRING_LINK_TTL_MS,
    trust_model: "legacy_unsigned",
    signature_verified: false,
  });
});

test("pairing parser rejects mismatched verification codes", () => {
  const invalidPayload = JSON.stringify({
    version: 1,
    fingerprint: "ABCD1234",
    device_name: "Desk Mac",
    verification_code: "0000-0000-0000",
    issued_at_unix_ms: 1234567890,
  });

  assert.throws(
    () => parsePairingInput(invalidPayload),
    /verification code is invalid/i,
  );
});

test("pairing parser rejects expired links", () => {
  const expiredPayload = JSON.stringify({
    version: 1,
    fingerprint: "ABCD1234",
    device_name: "Desk Mac",
    verification_code: verificationCodeFromFingerprint("ABCD1234"),
    issued_at_unix_ms: Date.now() - PAIRING_LINK_TTL_MS - 1_000,
  });

  assert.throws(
    () => parsePairingInput(expiredPayload),
    /expired/i,
  );
});
