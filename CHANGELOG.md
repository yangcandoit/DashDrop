# Changelog

## [0.3.0-rc.1] - 2026-03-13

### 🚀 Major Architectural Shift: Daemon-Backed Runtime
- **Split Process Model**: Migrated from a single-process Tauri app to a "UI + Daemon" architecture (`dashdropd`). This enables background transfers and persistent discovery even when the main window is closed.
- **Local IPC Layer**: Implemented a secure, cross-platform IPC (Unix Domain Sockets / Named Pipes) with mandatory Access Token authentication and UID/SID verification.
- **Event Replay & Checkpoints**: Added a robust synchronization mechanism to replay missed runtime events to the UI after reconnection.

### ✨ New Features
- **Resumable Transfers**: Added support for resuming partially downloaded files (`.dashdrop.part`) and smart-skipping identical files.
- **Windows Explorer Integration**: Added "Send with DashDrop" to the Windows context menu (Shell Extension).
- **Native BLE Assist (Win/Mac)**: 
    - macOS: Fully integrated Swift-based BLE bridge for seamless discovery.
    - Windows: Implemented a native WinRT-based BLE bridge in Rust for robust peer discovery in complex networks.
- **1:N Stress Tested**: Verified persistence and network stability under heavy concurrent loads (up to 120 rounds of concurrent transfers).

### 🛡️ Security & Trust
- **Stronger Trust Levels**: Introduced granular trust states (`legacy_paired`, `signed_link_verified`, `mutual_confirmed`).
- **Security Audit Log**: All identity mismatches and handshake failures are now recorded in a local SQLite audit trail.
- **Fingerprint Lockdown**: Automatic freezing of trusted relationships upon fingerprint changes.

### ⚠️ Known Limitations (v0.3.0)
- **Linux BLE**: Currently lacks a native BLE bridge (mDNS/UDP Beacon only).
- **SoftAP (Experimental)**: P2P Wi-Fi hotspots are not yet automatically triggered (requires manual network joining).
- **VPN Compatibility**: Discovery may be unreliable when certain VPNs (with split-tunneling disabled) are active.

### 🔧 Improvements & Fixes
- Migrated all runtime state to **SQLite** as the single source of truth.
- Optimized UI state management with `vue-i18n` (Initial support for Chinese).
- Enhanced "Next Steps" diagnostics for all common network and protocol errors.
