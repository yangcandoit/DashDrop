use anyhow::{anyhow, Context, Result};
use base64::{
    engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use pkcs8::DecodePrivateKey;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::State;

const PAIRING_LINK_TTL_MS: u64 = 10 * 60 * 1000;
const PAIRING_LINK_FUTURE_SKEW_MS: u64 = 2 * 60 * 1000;
const DASHDROP_PAIRING_PREFIX: &str = "dashdrop://pair?";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatedPairingPayload {
    pub version: u8,
    pub fingerprint: String,
    pub device_name: String,
    pub verification_code: String,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub trust_model: String,
    pub signature_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_proof: Option<IdentityMigrationProof>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentityMigrationProof {
    pub previous_fingerprint: String,
    pub previous_public_key: String,
    pub signature: String, // Signature of (new_fingerprint + issued_at) by old_private_key
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairingPayloadV2Body {
    version: u8,
    fingerprint: String,
    device_name: String,
    verification_code: String,
    issued_at_unix_ms: u64,
    signer_public_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_proof: Option<IdentityMigrationProof>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairingPayloadV1 {
    version: u8,
    fingerprint: String,
    device_name: String,
    verification_code: String,
    issued_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairingPayloadV2 {
    version: u8,
    fingerprint: String,
    device_name: String,
    verification_code: String,
    issued_at_unix_ms: u64,
    signer_public_key: String,
    signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_proof: Option<IdentityMigrationProof>,
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn normalize_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_uppercase())
        .collect()
}

fn normalize_device_name(device_name: &str) -> String {
    device_name.trim().to_string()
}

fn code_from_value(value: &str) -> String {
    if value.is_empty() {
        return "0000-0000-0000".to_string();
    }

    let mut buckets = [0x1357_u32, 0x2468_u32, 0x369c_u32];
    for (index, byte) in value.bytes().enumerate() {
        let bucket = index % buckets.len();
        buckets[bucket] = (buckets[bucket] * 131 + byte as u32 + (index as u32 * 17)) % 10_000;
    }

    buckets
        .into_iter()
        .map(|part| format!("{part:04}"))
        .collect::<Vec<_>>()
        .join("-")
}

fn verification_code_from_fingerprint(fingerprint: &str) -> String {
    code_from_value(&normalize_fingerprint(fingerprint))
}

fn pairing_payload_expires_at_unix_ms(issued_at_unix_ms: u64) -> u64 {
    issued_at_unix_ms.saturating_add(PAIRING_LINK_TTL_MS)
}

fn decode_pairing_payload_source(input: &str) -> Result<String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(anyhow!("Pairing link is empty."));
    }

    if let Some(query) = raw.strip_prefix(DASHDROP_PAIRING_PREFIX) {
        let data = query
            .split('&')
            .find_map(|entry| entry.strip_prefix("data="))
            .ok_or_else(|| anyhow!("Pairing link is missing data."))?;
        return URL_SAFE_NO_PAD
            .decode(data)
            .map_err(|_| anyhow!("Pairing link data is not valid base64url."))
            .and_then(|bytes| {
                String::from_utf8(bytes).context("pairing payload is not valid UTF-8")
            });
    }

    if raw.starts_with('{') {
        return Ok(raw.to_string());
    }

    URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| anyhow!("Pairing link data is not valid base64url."))
        .and_then(|bytes| String::from_utf8(bytes).context("pairing payload is not valid UTF-8"))
}

fn validate_payload_freshness(issued_at_unix_ms: u64, now: u64) -> Result<u64> {
    if issued_at_unix_ms == 0 {
        return Err(anyhow!("Pairing payload issue time is missing."));
    }
    if issued_at_unix_ms.saturating_sub(now) > PAIRING_LINK_FUTURE_SKEW_MS {
        return Err(anyhow!("Pairing payload issue time is invalid."));
    }
    let expires_at_unix_ms = pairing_payload_expires_at_unix_ms(issued_at_unix_ms);
    if now > expires_at_unix_ms {
        return Err(anyhow!(
            "Pairing link expired. Ask the other device to generate a new QR or pairing link."
        ));
    }
    Ok(expires_at_unix_ms)
}

fn validate_v1(payload: PairingPayloadV1, now: u64) -> Result<ValidatedPairingPayload> {
    let fingerprint = normalize_fingerprint(&payload.fingerprint);
    if fingerprint.is_empty() {
        return Err(anyhow!("Pairing payload fingerprint is missing."));
    }
    let device_name = normalize_device_name(&payload.device_name);
    if device_name.is_empty() {
        return Err(anyhow!("Pairing payload device name is missing."));
    }
    let expected_code = verification_code_from_fingerprint(&fingerprint);
    if payload.verification_code.trim() != expected_code {
        return Err(anyhow!("Pairing payload verification code is invalid."));
    }
    let expires_at_unix_ms = validate_payload_freshness(payload.issued_at_unix_ms, now)?;
    Ok(ValidatedPairingPayload {
        version: 1,
        fingerprint,
        device_name,
        verification_code: expected_code,
        issued_at_unix_ms: payload.issued_at_unix_ms,
        expires_at_unix_ms,
        trust_model: "legacy_unsigned".to_string(),
        signature_verified: false,
        migration_proof: None,
    })
}

fn validate_v2(payload: PairingPayloadV2, now: u64) -> Result<ValidatedPairingPayload> {
    let fingerprint = normalize_fingerprint(&payload.fingerprint);
    if fingerprint.is_empty() {
        return Err(anyhow!("Pairing payload fingerprint is missing."));
    }
    let device_name = normalize_device_name(&payload.device_name);
    if device_name.is_empty() {
        return Err(anyhow!("Pairing payload device name is missing."));
    }
    let expected_code = verification_code_from_fingerprint(&fingerprint);
    if payload.verification_code.trim() != expected_code {
        return Err(anyhow!("Pairing payload verification code is invalid."));
    }

    let public_key_bytes = BASE64_STANDARD
        .decode(payload.signer_public_key.trim())
        .map_err(|_| anyhow!("Pairing signer public key is invalid."))?;
    let public_key_array: [u8; 32] = public_key_bytes
        .try_into()
        .map_err(|_| anyhow!("Pairing signer public key length is invalid."))?;
    let verifying_key = VerifyingKey::from_bytes(&public_key_array)
        .map_err(|_| anyhow!("Pairing signer public key is invalid."))?;

    let signature_bytes = BASE64_STANDARD
        .decode(payload.signature.trim())
        .map_err(|_| anyhow!("Pairing link signature encoding is invalid."))?;
    let signature = Signature::try_from(signature_bytes.as_slice())
        .map_err(|_| anyhow!("Pairing link signature length is invalid."))?;

    let body = PairingPayloadV2Body {
        version: 2,
        fingerprint: fingerprint.clone(),
        device_name: device_name.clone(),
        verification_code: expected_code.clone(),
        issued_at_unix_ms: payload.issued_at_unix_ms,
        signer_public_key: payload.signer_public_key.trim().to_string(),
        migration_proof: payload.migration_proof.clone(),
    };
    let canonical =
        serde_json::to_vec(&body).context("serialize pairing payload for verification")?;
    verifying_key
        .verify(&canonical, &signature)
        .map_err(|_| anyhow!("Pairing link signature is invalid."))?;

    // Verify migration proof if present
    if let Some(proof) = &payload.migration_proof {
        let prev_pk_bytes = BASE64_STANDARD
            .decode(proof.previous_public_key.trim())
            .map_err(|_| anyhow!("Previous public key in migration proof is invalid."))?;
        let prev_pk_array: [u8; 32] = prev_pk_bytes
            .try_into()
            .map_err(|_| anyhow!("Previous public key length is invalid."))?;
        let prev_verifying_key = VerifyingKey::from_bytes(&prev_pk_array)
            .map_err(|_| anyhow!("Previous public key is invalid."))?;

        let migration_sig_bytes = BASE64_STANDARD
            .decode(proof.signature.trim())
            .map_err(|_| anyhow!("Migration proof signature encoding is invalid."))?;
        let migration_sig = Signature::try_from(migration_sig_bytes.as_slice())
            .map_err(|_| anyhow!("Migration proof signature length is invalid."))?;

        // Proof confirms: "I (old_pk) authorize transition to (new_fingerprint) at (issued_at)"
        let proof_data = format!("{}:{}", fingerprint, payload.issued_at_unix_ms);
        prev_verifying_key
            .verify(proof_data.as_bytes(), &migration_sig)
            .map_err(|_| anyhow!("Identity migration proof signature is invalid."))?;
    }

    let expires_at_unix_ms = validate_payload_freshness(payload.issued_at_unix_ms, now)?;
    Ok(ValidatedPairingPayload {
        version: 2,
        fingerprint,
        device_name,
        verification_code: expected_code,
        issued_at_unix_ms: payload.issued_at_unix_ms,
        expires_at_unix_ms,
        trust_model: "signed_link".to_string(),
        signature_verified: true,
        migration_proof: payload.migration_proof,
    })
}

#[allow(dead_code)]
pub fn create_identity_migration_proof(
    identity: &crate::crypto::Identity,
    new_fingerprint: &str,
    issued_at_unix_ms: u64,
) -> Result<IdentityMigrationProof> {
    let signing_key = SigningKey::from_pkcs8_der(&identity.key_der)
        .context("decode local identity key for migration proof")?;
    let public_key_base64 = BASE64_STANDARD.encode(signing_key.verifying_key().to_bytes());
    
    // Proof data: "new_fingerprint:issued_at"
    let proof_data = format!("{}:{}", normalize_fingerprint(new_fingerprint), issued_at_unix_ms);
    let signature = BASE64_STANDARD.encode(signing_key.sign(proof_data.as_bytes()).to_bytes());

    Ok(IdentityMigrationProof {
        previous_fingerprint: normalize_fingerprint(&identity.fingerprint),
        previous_public_key: public_key_base64,
        signature,
    })
}

#[allow(dead_code)]
#[tauri::command]
pub async fn generate_identity_migration_proof(
    state: State<'_, Arc<crate::state::AppState>>,
    new_fingerprint: String,
) -> Result<IdentityMigrationProof, String> {
    let now = now_unix_millis();
    create_identity_migration_proof(&state.identity, &new_fingerprint, now)
        .map_err(|error| error.to_string())
}

#[allow(dead_code)]
#[tauri::command]
pub async fn apply_identity_migration(
    state: State<'_, Arc<crate::state::AppState>>,
    payload: ValidatedPairingPayload,
) -> Result<(), String> {
    let proof = payload.migration_proof.ok_or_else(|| "No migration proof found in payload".to_string())?;
    
    let db = state.db.lock().map_err(|_| "Database lock poisoned".to_string())?;
    crate::db::migrate_trusted_peer_identity(
        &db,
        &proof.previous_fingerprint,
        &payload.fingerprint,
        &payload.device_name,
    ).map_err(|error| error.to_string())?;

    // Refresh memory state if needed or emit event
    Ok(())
}

pub fn validate_pairing_payload(input: &str) -> Result<ValidatedPairingPayload> {
    let payload_source = decode_pairing_payload_source(input)?;
    let parsed: serde_json::Value = serde_json::from_str(&payload_source)
        .map_err(|_| anyhow!("Pairing payload is not valid JSON."))?;
    let version = parsed
        .get("version")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| anyhow!("Unsupported pairing payload version."))?;
    let now = now_unix_millis();
    match version {
        1 => validate_v1(
            serde_json::from_value(parsed).context("decode legacy pairing payload")?,
            now,
        ),
        2 => validate_v2(
            serde_json::from_value(parsed).context("decode signed pairing payload")?,
            now,
        ),
        _ => Err(anyhow!("Unsupported pairing payload version.")),
    }
}

pub fn build_signed_pairing_link(
    identity: &crate::crypto::Identity,
    device_name: &str,
) -> Result<String> {
    let fingerprint = normalize_fingerprint(&identity.fingerprint);
    let device_name = normalize_device_name(device_name);
    if fingerprint.is_empty() {
        return Err(anyhow!("Local pairing fingerprint is missing."));
    }
    if device_name.is_empty() {
        return Err(anyhow!("Local pairing device name is missing."));
    }

    let issued_at_unix_ms = now_unix_millis();
    let verification_code = verification_code_from_fingerprint(&fingerprint);
    let signing_key = SigningKey::from_pkcs8_der(&identity.key_der)
        .context("decode local identity key for pairing link")?;
    let signer_public_key = BASE64_STANDARD.encode(signing_key.verifying_key().to_bytes());
    let body = PairingPayloadV2Body {
        version: 2,
        fingerprint,
        device_name,
        verification_code,
        issued_at_unix_ms,
        signer_public_key: signer_public_key.clone(),
        migration_proof: None,
    };
    let canonical = serde_json::to_vec(&body).context("serialize pairing payload for signing")?;
    let signature = BASE64_STANDARD.encode(signing_key.sign(&canonical).to_bytes());
    let payload = PairingPayloadV2 {
        version: body.version,
        fingerprint: body.fingerprint,
        device_name: body.device_name,
        verification_code: body.verification_code,
        issued_at_unix_ms: body.issued_at_unix_ms,
        signer_public_key,
        signature,
        migration_proof: None,
    };
    let encoded = URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).context("serialize signed pairing payload")?);
    Ok(format!("dashdrop://pair?data={encoded}"))
}

#[tauri::command]
pub async fn get_local_pairing_link(
    state: State<'_, Arc<crate::state::AppState>>,
) -> Result<String, String> {
    let device_name = state.config.read().await.device_name.clone();
    build_signed_pairing_link(&state.identity, &device_name).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn validate_pairing_input(input: String) -> Result<ValidatedPairingPayload, String> {
    validate_pairing_payload(&input).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_identity_dir(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("dashdrop-pairing-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp identity dir");
        path
    }

    #[test]
    fn signed_pairing_link_round_trips_through_validator() {
        let dir = temp_identity_dir("signed-roundtrip");
        let identity = crate::crypto::Identity::load_or_create(&dir).expect("create identity");
        let uri =
            build_signed_pairing_link(&identity, "Desk Mac").expect("build signed pairing link");
        let payload = validate_pairing_payload(&uri).expect("validate signed pairing link");

        assert_eq!(payload.version, 2);
        assert_eq!(payload.trust_model, "signed_link");
        assert!(payload.signature_verified);
        assert_eq!(
            payload.fingerprint,
            normalize_fingerprint(&identity.fingerprint)
        );
        assert_eq!(payload.device_name, "Desk Mac");
        assert!(payload.expires_at_unix_ms >= payload.issued_at_unix_ms);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn signed_pairing_link_rejects_tampered_device_name() {
        let dir = temp_identity_dir("signed-tamper");
        let identity = crate::crypto::Identity::load_or_create(&dir).expect("create identity");
        let uri =
            build_signed_pairing_link(&identity, "Desk Mac").expect("build signed pairing link");
        let encoded = uri
            .strip_prefix(DASHDROP_PAIRING_PREFIX)
            .and_then(|query| query.strip_prefix("data="))
            .expect("pairing data");
        let json = String::from_utf8(
            URL_SAFE_NO_PAD
                .decode(encoded)
                .expect("decode signed payload"),
        )
        .expect("signed payload utf8");
        let mut value: serde_json::Value =
            serde_json::from_str(&json).expect("parse signed payload json");
        value["device_name"] = serde_json::json!("Mallory");
        let tampered = format!(
            "{DASHDROP_PAIRING_PREFIX}data={}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).expect("serialize tampered payload"))
        );

        let error =
            validate_pairing_payload(&tampered).expect_err("tampered payload should fail");
        assert!(
            error.to_string().contains("signature is invalid"),
            "unexpected error: {error:#}"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_unsigned_pairing_payload_still_validates() {
        let raw = serde_json::json!({
            "version": 1,
            "fingerprint": "ABCD1234",
            "device_name": "Desk Mac",
            "verification_code": verification_code_from_fingerprint("ABCD1234"),
            "issued_at_unix_ms": now_unix_millis(),
        })
        .to_string();

        let payload = validate_pairing_payload(&raw).expect("validate legacy pairing payload");
        assert_eq!(payload.version, 1);
        assert_eq!(payload.trust_model, "legacy_unsigned");
        assert!(!payload.signature_verified);
    }
}
