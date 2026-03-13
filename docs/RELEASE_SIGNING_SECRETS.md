# DashDrop Release Signing Secrets

This document defines the minimum GitHub Actions secrets required for signed release builds.

## macOS signing and notarization

Required repository secrets:

- `APPLE_CERTIFICATE`
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`
- `APPLE_PASSWORD`
- `APPLE_TEAM_ID`

Behavior:

- If all values are present, the macOS bundle job can sign and notarize the app bundle.
- If any value is missing, the workflow still produces an unsigned macOS artifact and records that state in `SIGNING_STATUS.txt`.

## Tauri updater signing

Required repository secrets:

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

Behavior:

- If both values are present, updater signing is enabled.
- If either value is missing, updater signing is disabled and the workflow records that state in `SIGNING_STATUS.txt`.

## Windows code signing

Required repository secrets:

- `WINDOWS_CERT_BASE64`
- `WINDOWS_CERT_PASSWORD`

Behavior:

- If both values are present, the workflow signs `.exe` and `.msi` outputs with `signtool`.
- If either value is missing, the workflow leaves Windows artifacts unsigned and records that state in `SIGNING_STATUS.txt`.

## Release checklist linkage

Before creating a public release:

1. Confirm the required secrets are present for the platforms you intend to ship.
2. Confirm `SIGNING_STATUS.txt` in workflow artifacts matches the expected release posture.
3. Include unsigned limitations in release notes when any signing path is intentionally disabled.
