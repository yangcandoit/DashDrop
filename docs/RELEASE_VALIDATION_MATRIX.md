# DashDrop Release Validation Matrix

Use this matrix for every release candidate. Each platform pairing should be validated on real devices, not only simulators or two instances on the same machine.

## Platform Pairs

| Pair | Discovery | `device_lost` | Small file | Multi-file | 5GB+ bidirectional | Cancel | Recovery |
| --- | --- | --- | --- | --- | --- | --- | --- |
| macOS <-> Windows | [ ] | [ ] | [ ] | [ ] | [ ] | [ ] | [ ] |
| macOS <-> Linux | [ ] | [ ] | [ ] | [ ] | [ ] | [ ] | [ ] |
| Windows <-> Linux | [ ] | [ ] | [ ] | [ ] | [ ] | [ ] | [ ] |

## Required Scenarios Per Pair

- Discovery on same LAN with mDNS available
- Discovery on same LAN with multicast constrained enough to require beacon fallback
- `device_lost` observed after peer sleeps, quits, or disconnects
- Small single file transfer both directions
- Multi-file transfer both directions
- `5GB+` transfer both directions with final history correctness
- Sender-side cancel during pending accept
- Receiver-side cancel during active transfer
- Transfer retry after a forced failure
- Signed pairing link import and trust badge verification
- Fingerprint change warning path on previously trusted peer

## Environment Notes

- Record whether both devices are on Wi-Fi, Ethernet, or mixed
- Record whether VPN, virtual NICs, or endpoint protection are installed
- Record whether firewall rules were pre-approved or granted during the run
- Record whether the build was signed/notarized or unsigned

## Exit Criteria

- Every row above is checked for at least one real run on the release candidate
- Any failure has a linked issue or a documented accepted release limitation
- The corresponding release validation report is attached to the release branch or tag
