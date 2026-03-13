# Platform Integration Scaffold

Updated: 2026-03-13

## Scope

This document records the minimum platform-entry scaffold that now exists for future Finder / Shell / system-share work without changing the current frontend state contract or IPC payload shape.

Current scope:

1. Packaged builds declare DashDrop as an alternate `Open With` target for common user file types via `src-tauri/tauri.conf.json > bundle.fileAssociations`.
2. The runtime startup path normalizer now accepts local `file://` URLs in addition to plain filesystem paths and ignores macOS `-psn_*` process-serial arguments.
3. The existing startup share intake remains the handoff boundary: platform launchers only need to deliver file paths or local file URLs.

## Why this shape

This keeps the platform integration surface narrow:

1. No change to frontend store contracts.
2. No new daemon payload format.
3. No native share-extension implementation yet.
4. Finder / Explorer / launcher entry points can reuse the same "launch app with selected paths" handoff.

## Registered file groups

The bundle currently declares alternate open handlers for:

1. Plain text
2. Markdown
3. CSV
4. JSON
5. Rich text
6. PDF
7. Word `.doc`
8. Word `.docx`
9. Excel `.xls`
10. Excel `.xlsx`
11. PowerPoint `.ppt`
12. PowerPoint `.pptx`
13. Images
14. Audio
15. Video
16. Zip / 7z / tar / gzip archives

The associations are intentionally `Alternate` rather than owner/default so packaged builds can appear in system "Open With" UI without trying to replace the user's normal editor/viewer defaults.
They also carry Linux-facing `mimeType` values, and the text / Office / archive families are now split so `.desktop` exports do not rely on a single catch-all MIME bucket for mixed file types.

## Planned follow-ons

The next native/platform-specific layers should build on this scaffold instead of inventing new payloads:

1. macOS Finder Quick Action or Share Extension that forwards security-scoped file references into the existing local handoff path.
2. Windows Explorer shell entry that launches DashDrop with selected file paths.
3. Linux desktop / file-manager actions that invoke the packaged app with selected files.
4. Optional manifest-based handoff only if later native integrations need richer metadata than plain paths/bookmarks.

## Real-device validation still required

The following points were not fully field-verified by this change and still need packaged-device checks:

1. macOS Finder `Open With` exposure across the declared file groups in a signed `.app` bundle.
2. Windows Explorer `Open with` surfacing for packaged installer builds.
3. Linux desktop-environment behavior for `.desktop` association export across GNOME/KDE variants.
4. Any future bookmark-based macOS share flow, because the current scaffold only normalizes local paths and `file://` URLs.

## Suggested packaged validation pass

Use the existing path-handoff boundary when validating packaged builds:

1. Build a packaged app bundle or installer.
2. Launch DashDrop once so the OS registers deep links and file associations.
3. From Finder / Explorer / the Linux file manager, use `Open With DashDrop` on at least one file from each declared group.
4. Confirm the running instance receives the file via the existing startup/share queue path instead of opening a new business flow.
5. On macOS, also validate a `file://localhost/...` launch source if the handoff comes from automation or AppleScript glue.

Recommended spot checks:

1. A filename containing spaces.
2. A filename containing `%20` or URL query / fragment suffixes in the launcher handoff path.
3. A second launch while DashDrop is already running, to confirm the platform entry still feeds the existing single-instance handoff path.
