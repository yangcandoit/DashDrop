export type RuntimeEventName =
  | "device_discovered"
  | "device_updated"
  | "device_lost"
  | "transfer_started"
  | "transfer_incoming"
  | "transfer_accepted"
  | "transfer_progress"
  | "transfer_complete"
  | "transfer_partial"
  | "transfer_rejected"
  | "transfer_cancelled_by_sender"
  | "transfer_cancelled_by_receiver"
  | "transfer_failed"
  | "transfer_error"
  | "system_error"
  | "trusted_peer_updated"
  | "app_config_updated"
  | "identity_mismatch"
  | "fingerprint_changed"
  | "external_share_received"
  | "pairing_link_received"
  | "app_navigation_requested"
  | "app_window_revealed"
  | "daemon_control_plane_recovered"
  | "daemon_event_feed_resync_required";

export const DAEMON_MODE_LOCAL_SHELL_EVENTS = [
  "system_error",
  "external_share_received",
  "pairing_link_received",
  "app_navigation_requested",
  "app_window_revealed",
] as const satisfies RuntimeEventName[];

const daemonModeLocalShellEventSet = new Set<RuntimeEventName>(DAEMON_MODE_LOCAL_SHELL_EVENTS);

export function splitRuntimeEventSources(events: Iterable<RuntimeEventName>): {
  daemonFeedEvents: RuntimeEventName[];
  localShellEvents: RuntimeEventName[];
} {
  const daemonFeedEvents: RuntimeEventName[] = [];
  const localShellEvents: RuntimeEventName[] = [];
  const seenDaemonFeed = new Set<RuntimeEventName>();
  const seenLocalShell = new Set<RuntimeEventName>();

  for (const event of events) {
    if (!seenDaemonFeed.has(event)) {
      seenDaemonFeed.add(event);
      daemonFeedEvents.push(event);
    }
    if (daemonModeLocalShellEventSet.has(event) && !seenLocalShell.has(event)) {
      seenLocalShell.add(event);
      localShellEvents.push(event);
    }
  }

  return {
    daemonFeedEvents,
    localShellEvents,
  };
}
