# Upgrade & Migration Template

## Scope
- From version:
- To version:
- Applies to: `macOS / Windows / Linux`

## Pre-upgrade Checklist
- [ ] Backup local database files if applicable
- [ ] Export config snapshot if needed
- [ ] Confirm both peers are within supported protocol window (`N`/`N-1`)

## Database and State Migration
- Migration ID:
- Backward compatibility:
- Rollback strategy:

## Protocol Compatibility
- Supported protocol versions:
- Deprecated fields/events:
- Removed fields/events:

## Runtime Configuration Changes
- Added config keys:
- Removed config keys:
- Default changes:

## User-visible Changes
- UI/interaction changes:
- Security warnings/behavior changes:

## Rollout Plan
1. Canary rollout scope:
2. Full rollout condition:
3. Abort condition:

## Validation Plan
- [ ] `cargo check`
- [ ] `cargo test`
- [ ] `npm run build`
- [ ] `npm run test:e2e`
- [ ] Cross-device smoke tests

## Rollback Plan
1. Stop rollout
2. Restore previous installer/version
3. Restore database/config from backup if required
4. Verify transfer and discovery paths
