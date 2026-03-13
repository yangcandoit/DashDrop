import assert from "node:assert/strict";
import test from "node:test";

import {
  DAEMON_MODE_LOCAL_SHELL_EVENTS,
  splitRuntimeEventSources,
} from "../src/runtimeEventRouting.ts";

test("daemon mode keeps runtime events on daemon feed and mirrors shell events locally", () => {
  const { daemonFeedEvents, localShellEvents } = splitRuntimeEventSources([
    "device_discovered",
    "system_error",
    "external_share_received",
    "pairing_link_received",
    "app_navigation_requested",
    "app_window_revealed",
  ]);

  assert.deepEqual(daemonFeedEvents, [
    "device_discovered",
    "system_error",
    "external_share_received",
    "pairing_link_received",
    "app_navigation_requested",
    "app_window_revealed",
  ]);
  assert.deepEqual(localShellEvents, [
    "system_error",
    "external_share_received",
    "pairing_link_received",
    "app_navigation_requested",
    "app_window_revealed",
  ]);
});

test("daemon mode shell-event routing stays deduplicated and intentionally narrow", () => {
  const { daemonFeedEvents, localShellEvents } = splitRuntimeEventSources([
    "system_error",
    "system_error",
    "transfer_progress",
    "external_share_received",
    "pairing_link_received",
    "app_navigation_requested",
  ]);

  assert.deepEqual(daemonFeedEvents, [
    "system_error",
    "transfer_progress",
    "external_share_received",
    "pairing_link_received",
    "app_navigation_requested",
  ]);
  assert.deepEqual(localShellEvents, [
    "system_error",
    "external_share_received",
    "pairing_link_received",
    "app_navigation_requested",
  ]);
  assert.deepEqual(DAEMON_MODE_LOCAL_SHELL_EVENTS, [
    "system_error",
    "external_share_received",
    "pairing_link_received",
    "app_navigation_requested",
    "app_window_revealed",
  ]);
});

test("daemon feed resync marker stays on daemon feed only", () => {
  const { daemonFeedEvents, localShellEvents } = splitRuntimeEventSources([
    "daemon_event_feed_resync_required",
    "system_error",
  ]);

  assert.deepEqual(daemonFeedEvents, [
    "daemon_event_feed_resync_required",
    "system_error",
  ]);
  assert.deepEqual(localShellEvents, ["system_error"]);
});
