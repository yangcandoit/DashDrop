# DashDrop Network Troubleshooting

Last updated: 2026-03-11

This guide covers the most common discovery and transfer failures on macOS, Windows, and Linux.

## Quick checks

Before platform-specific debugging, verify these first:

1. Both devices are on the same LAN or Wi-Fi.
2. Both devices are awake and DashDrop is open.
3. `Settings -> Runtime` shows a listener port and `mDNS registered = true`.
4. If automatic discovery fails, try `Transfers -> Connect by Address`.
5. For first-time peers, compare the `Verification Code` out-of-band before trusting.

## Ports and protocols

DashDrop currently relies on:

- `UDP 5353` for mDNS
- `UDP 53318` for beacon fallback
- `UDP 53319` preferred QUIC listener port
- a random UDP fallback port if `53319` is already in use

If a firewall blocks these ports, discovery or transfer may fail even when both devices are online.

## macOS

### Symptoms

- Nearby stays empty
- Other devices never see this Mac
- Startup succeeds, but discovery is inconsistent

### What to check

1. Allow Local Network access when macOS prompts for it.
2. In `System Settings -> Privacy & Security -> Local Network`, ensure DashDrop is allowed.
3. If you are testing an unsigned build, remove quarantine and re-open the app:

```bash
xattr -dr com.apple.quarantine /Applications/DashDrop.app
codesign --force --deep --sign - /Applications/DashDrop.app
open /Applications/DashDrop.app
```

### Useful signals

- `Settings -> Copy Discovery Diagnostics`
- `/tmp/dashdrop-startup-error.log` if startup fails before the app UI is visible

## Windows

### Symptoms

- Discovery works only one way
- Connect by Address works, but Nearby does not
- Transfers time out even though the peer is visible
- The app or daemon sidecar fails to start because of a missing runtime dependency

### Firewall guidance

Allow DashDrop through Windows Defender Firewall for:

- `UDP 5353`
- `UDP 53318`
- `UDP 53319`
- the runtime fallback UDP listener port if diagnostics show `listener_port_mode = fallback_random`

### Suggested validation steps

1. Open DashDrop on both devices.
2. Confirm `Settings -> Runtime` shows a non-zero local listener port.
3. Confirm the Windows host allowed the firewall prompt for `dashdropd.exe`, not only the UI executable.
4. If startup closes immediately, check:
   - `%APPDATA%\\DashDrop\\startup-error.log`
   - `%TEMP%\\dashdrop-startup-error.log`
5. If discovery fails, compare diagnostics on both peers.
6. If `Connect by Address` succeeds but Nearby does not, the likely cause is mDNS or beacon firewall filtering.
7. If neither Nearby nor Connect by Address works, verify the Windows host is not missing WebView2 Runtime and that the active listener port is allowed through the firewall.

### Notes

- DashDrop now has a baseline Windows local IPC implementation via named pipes.
- Discovery still depends on the local network environment; restrictive enterprise firewall policy can still block it.
- Packaged Windows builds now explicitly request WebView2 bootstrap handling, but real clean-machine verification is still required before release.

## Linux

### Symptoms

- No peers appear automatically
- Discovery appears unstable between networks
- Transfers work only by manual address

### Avahi and firewall checks

1. Ensure `avahi-daemon` is running if your distro uses it for mDNS interoperability.
2. If you use `ufw`, allow:

```bash
sudo ufw allow 5353/udp
sudo ufw allow 53318/udp
sudo ufw allow 53319/udp
```

3. If diagnostics show a fallback listener port, allow that UDP port as well.

### Notes

- On Linux, beacon fallback can help when mDNS is weak or partially blocked, but both can still be filtered by local firewall rules.
- Secret Service availability affects secure key storage posture, not discovery itself.

## When Nearby is empty but the peer is reachable

Use `Transfers -> Connect by Address` and enter `host:port` from a trusted peer in the same LAN.

If this works:

- transport is likely healthy
- discovery is likely being filtered or delayed

If this does not work:

- check firewall policy
- verify the peer listener port from diagnostics
- compare the verification code before trusting

## What to collect for bug reports

Include:

1. Platform and version
2. Whether the failure is discovery-only or also affects manual address connect
3. `Copy Discovery Diagnostics` output from both peers
4. Whether `listener_port_mode` is `fixed` or `fallback_random`
5. Any startup error log path mentioned above
