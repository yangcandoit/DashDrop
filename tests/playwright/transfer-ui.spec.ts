import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    type Handler = (payload: unknown) => void;
    const listeners = new Map<string, Set<Handler>>();
    const expiredIncomingIds = new Set<string>();
    const params = new URLSearchParams(window.location.search);
    const requestedControlPlaneMode = params.get("requestedControlPlaneMode") ?? "daemon";
    const controlPlaneMode = params.get("controlPlaneMode") ?? "in_process";
    const initialRuntimeFeedFailureCount = Number(params.get("runtimeFeedFailureCount") ?? "0");
    const state = {
      devices: {} as Record<string, any>,
      transfers: {} as Record<string, any>,
      incoming: {} as Record<string, any>,
      requestedControlPlaneMode,
      controlPlaneMode,
      runtimeEvents: [] as Array<{ seq: number; event: string; payload: unknown; emitted_at_unix_ms: number }>,
      runtimeEventSeq: 0,
      runtimeFeedFailureCount: Number.isFinite(initialRuntimeFeedFailureCount)
        ? Math.max(0, initialRuntimeFeedFailureCount)
        : 0,
      history: [] as any[],
      connectByAddressCalls: [] as string[],
      sendFileCalls: [] as Array<{ peerFp: string; paths: string[] }>,
      retryCalls: [] as string[],
      cancelAllCalls: 0,
      trustedPeers: [] as any[],
    };

    const recordRuntimeEvent = (event: string, payload: unknown) => {
      state.runtimeEventSeq += 1;
      state.runtimeEvents.push({
        seq: state.runtimeEventSeq,
        event,
        payload,
        emitted_at_unix_ms: Date.now(),
      });
    };

    const emit = (event: string, payload: unknown) => {
      recordRuntimeEvent(event, payload);
      const set = listeners.get(event);
      if (!set) return;
      for (const handler of set) handler(payload);
    };
    const emitLocalOnly = (event: string, payload: unknown) => {
      const set = listeners.get(event);
      if (!set) return;
      for (const handler of set) handler(payload);
    };
    const emitRuntimeOnly = (event: string, payload: unknown) => {
      recordRuntimeEvent(event, payload);
    };

    (window as any).__emitTestEvent = (event: string, payload: unknown) => emit(event, payload);
    (window as any).__emitLocalOnlyEvent = (event: string, payload: unknown) => emitLocalOnly(event, payload);
    (window as any).__emitRuntimeOnlyEvent = (event: string, payload: unknown) => emitRuntimeOnly(event, payload);
    (window as any).__getTestState = () => JSON.parse(JSON.stringify(state));
    (window as any).__setTrustedPeers = (peers: any[]) => {
      state.trustedPeers = peers.map((peer) => ({ ...peer }));
    };
    (window as any).__setControlPlaneModes = (requested: string, actual: string) => {
      state.requestedControlPlaneMode = requested;
      state.controlPlaneMode = actual;
    };
    (window as any).__setRuntimeFeedFailureCount = (count: number) => {
      state.runtimeFeedFailureCount = Number.isFinite(count) ? Math.max(0, count) : 0;
    };
    (window as any).__setMockDevices = (devices: any[]) => {
      state.devices = Object.fromEntries(devices.map((device) => [device.fingerprint, { ...device }]));
    };
    (window as any).__pushHistoryTransfer = (transfer: any) => {
      state.history.unshift(transfer);
    };

    const clipboardState = { text: "" };
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: async (text: string) => {
          clipboardState.text = String(text);
        },
      },
    });
    (window as any).__getClipboardText = () => clipboardState.text;

    (window as any).__DASHDROP_TEST_MOCK__ = {
      invoke(command: string, args?: Record<string, unknown>) {
        switch (command) {
          case "get_local_identity":
            return { fingerprint: "local-fp", device_name: "Test Device", port: 7000 };
          case "get_devices":
            return Object.values(state.devices);
          case "get_transfers":
            return Object.values(state.transfers);
          case "get_transfer":
            return state.transfers[String(args?.transferId)] ?? null;
          case "get_transfer_history":
            return state.history;
          case "get_app_config":
            return {
              device_name: "Test Device",
              auto_accept_trusted_only: false,
              download_dir: null,
              file_conflict_strategy: "rename",
              max_parallel_streams: 4,
            };
          case "set_app_config":
            return null;
          case "get_runtime_status":
            return {
              requested_control_plane_mode: state.requestedControlPlaneMode,
              control_plane_mode: state.controlPlaneMode,
              runtime_profile: "packaged",
              local_port: 7000,
              mdns_registered: true,
              discovered_devices: 2,
              trusted_devices: state.trustedPeers.length,
              daemon_status: "connected_after_retry",
              daemon_connect_attempts: 3,
              daemon_connect_strategy: "attach_existing",
              daemon_binary_path: "/Applications/DashDrop.app/Contents/MacOS/dashdropd",
            };
          case "get_transfer_metrics":
            return {
              completed: 0,
              partial: 0,
              failed: 0,
              cancelled_by_sender: 0,
              cancelled_by_receiver: 0,
              rejected: 0,
              bytes_sent: 0,
              bytes_received: 0,
              average_duration_ms: 0,
              failure_distribution: {},
            };
          case "get_discovery_diagnostics":
            return {
              runtime: {
                requested_control_plane_mode: state.requestedControlPlaneMode,
                control_plane_mode: state.controlPlaneMode,
                runtime_profile: "packaged",
                local_port: 7000,
                mdns_registered: true,
                discovered_devices: Object.keys(state.devices).length,
                trusted_devices: state.trustedPeers.length,
                daemon_status: "connected_after_retry",
                daemon_connect_attempts: 3,
                daemon_connect_strategy: "attach_existing",
                daemon_binary_path: "/Applications/DashDrop.app/Contents/MacOS/dashdropd",
              },
              service_type: "_dashdrop._udp.local.",
              own_fingerprint: "local-fp",
              own_platform: "Mac",
              mdns_daemon_initialized: true,
              mdns_service_fullname: "Test-Device._dashdrop._udp.local.",
              mdns_last_search_started: "if=0.0.0.0",
              local_instance_name: "Test-Device",
              listener_mode: "dual_stack",
              listener_addrs: ["[::]:7000", "0.0.0.0:7000"],
              network_interfaces: [
                { name: "en0", is_loopback: false, ipv4: ["192.168.1.8"], ipv6: ["fe80::1"] },
              ],
              browser_status: {
                active: true,
                restart_count: 0,
                last_disconnect_at: null,
                last_search_started: "if=0.0.0.0",
              },
              session_index_count: 1,
              discovery_event_counts: { search_started: 1, service_resolved: 1 },
              discovery_failure_counts: {},
              quick_hints: [],
              device_count: Object.keys(state.devices).length,
              devices: Object.values(state.devices),
            };
          case "get_trusted_peers":
            return state.trustedPeers;
          case "get_control_plane_mode":
            return state.controlPlaneMode;
          case "get_runtime_events":
            if (state.runtimeFeedFailureCount > 0) {
              state.runtimeFeedFailureCount -= 1;
              throw new Error("mock daemon event feed unavailable");
            }
            return {
              events: state.runtimeEvents
                .filter((entry) => entry.seq > Number(args?.afterSeq ?? 0))
                .slice(0, Number(args?.limit ?? 100)),
              generation: "test-feed",
              oldest_available_seq: state.runtimeEvents[0]?.seq ?? null,
              latest_available_seq: state.runtimeEvents[state.runtimeEvents.length - 1]?.seq ?? 0,
              resync_required: false,
            };
          case "set_trusted_alias": {
            const fp = String(args?.fp ?? "");
            const alias = args?.alias == null ? null : String(args.alias);
            const peer = state.trustedPeers.find((p) => p.fingerprint === fp);
            if (peer) peer.alias = alias;
            return null;
          }
          case "cancel_all_transfers": {
            state.cancelAllCalls += 1;
            Object.values(state.transfers).forEach((t: any) => {
              if (t.status === "PendingAccept" || t.status === "Transferring") {
                t.status = t.direction === "Send" ? "CancelledBySender" : "CancelledByReceiver";
                t.revision = (t.revision ?? 0) + 1;
                emit(
                  t.direction === "Send" ? "transfer_cancelled_by_sender" : "transfer_cancelled_by_receiver",
                  {
                    transfer_id: t.id,
                    reason_code: "E_CANCELLED_BY_USER",
                    terminal_cause: "UserCancelled",
                    revision: t.revision,
                  },
                );
              }
            });
            return 1;
          }
          case "send_files_cmd":
            state.sendFileCalls.push({
              peerFp: String(args?.peerFp ?? ""),
              paths: Array.isArray(args?.paths) ? (args?.paths as string[]) : [],
            });
            return null;
          case "retry_transfer":
            state.retryCalls.push(String(args?.transferId ?? ""));
            return null;
          case "get_security_posture":
            return { secure_store_available: true };
          case "get_security_events":
            return [];
          case "accept_transfer": {
            const transferId = String(args?.transferId);
            if (expiredIncomingIds.has(transferId)) {
              delete state.incoming[transferId];
              emit("transfer_error", {
                transfer_id: transferId,
                reason_code: "E_REQUEST_EXPIRED",
                terminal_cause: "NotificationExpired",
                phase: "notification_action",
                revision: 0,
              });
              throw new Error("E_REQUEST_EXPIRED");
            }
            let t = state.transfers[transferId];
            if (!t && state.incoming[transferId]) {
              const incoming = state.incoming[transferId];
              t = {
                id: transferId,
                direction: "Receive",
                peer_fingerprint: incoming.sender_fp,
                peer_name: incoming.sender_name,
                items: incoming.items,
                status: "PendingAccept",
                bytes_transferred: 0,
                total_bytes: incoming.total_size,
                revision: 0,
              };
              state.transfers[transferId] = t;
            }
            if (t) {
              t.status = "Transferring";
              t.revision = 1;
              t.bytes_transferred = t.total_bytes;
              t.status = "Completed";
              t.revision = 2;
              state.history.unshift({ ...t, ended_at_unix: 1 });
            }
            emit("transfer_accepted", { transfer_id: transferId, revision: 1 });
            emit("transfer_complete", { transfer_id: transferId, revision: 2 });
            return null;
          }
          case "reject_transfer": {
            const transferId = String(args?.transferId);
            if (expiredIncomingIds.has(transferId)) {
              delete state.incoming[transferId];
              emit("transfer_error", {
                transfer_id: transferId,
                reason_code: "E_REQUEST_EXPIRED",
                terminal_cause: "NotificationExpired",
                phase: "notification_action",
                revision: 0,
              });
              throw new Error("E_REQUEST_EXPIRED");
            }
            let t = state.transfers[transferId];
            if (!t && state.incoming[transferId]) {
              const incoming = state.incoming[transferId];
              t = {
                id: transferId,
                direction: "Receive",
                peer_fingerprint: incoming.sender_fp,
                peer_name: incoming.sender_name,
                items: incoming.items,
                status: "PendingAccept",
                bytes_transferred: 0,
                total_bytes: incoming.total_size,
                revision: 0,
              };
              state.transfers[transferId] = t;
            }
            if (t) {
              t.status = "Rejected";
              t.revision = 1;
              t.error = "E_REJECTED_BY_USER";
              state.history.unshift({ ...t, ended_at_unix: 1 });
            }
            emit("transfer_rejected", {
              transfer_id: transferId,
              reason_code: "E_REJECTED_BY_USER",
              terminal_cause: "RejectedByReceiver",
              revision: 1,
            });
            return null;
          }
          case "connect_by_address":
            state.connectByAddressCalls.push(String(args?.address ?? ""));
            return {
              fingerprint: "fp-connect",
              name: "Manual Peer",
              trusted: false,
              address: String(args?.address ?? ""),
            };
          default:
            return null;
        }
      },
      listen(event: string, cb: (payload: unknown) => void) {
        if (!listeners.has(event)) listeners.set(event, new Set());
        listeners.get(event)!.add(cb);
        return () => listeners.get(event)?.delete(cb);
      },
    };

    const seedIncoming = (transferId: string, senderName: string) => {
      const payload = {
        transfer_id: transferId,
        sender_name: senderName,
        sender_fp: `${transferId}-fp`,
        trusted: false,
        items: [{ file_id: 1, name: "a.txt", rel_path: "a.txt", size: 5 }],
        total_size: 5,
        revision: 0,
      };
      state.incoming[transferId] = payload;
      emit("transfer_incoming", payload);
    };

    (window as any).__seedIncomingTransfer = (transferId: string, senderName: string) => {
      seedIncoming(transferId, senderName);
    };
    (window as any).__expireIncomingTransfer = (transferId: string) => {
      expiredIncomingIds.add(transferId);
    };

    setTimeout(() => {
      seedIncoming("t-accept", "Peer Accept");
      seedIncoming("t-reject", "Peer Reject");
    }, 500);
  });

  await page.goto("/");
});

test("incoming -> accept -> history visible", async ({ page }) => {
  await page.getByRole("button", { name: "Transfers" }).click();
  await expect(page.getByText("Peer Accept")).toBeVisible();
  await page
    .locator("article.incoming-card", { hasText: "Peer Accept" })
    .getByRole("button", { name: "Accept", exact: true })
    .click();
  await page.getByRole("checkbox", { name: "I compared this shared code on both devices" }).check();
  await page.getByRole("button", { name: "Continue" }).click();

  await page.getByRole("button", { name: "History" }).click();
  await expect(page.getByText("Peer Accept")).toBeVisible();
  await expect(page.locator(".status-badge.completed").first()).toBeVisible();
});

test("incoming -> reject -> history visible", async ({ page }) => {
  await page.getByRole("button", { name: "Transfers" }).click();
  await expect(page.getByText("Peer Reject")).toBeVisible();
  await page.locator("article.incoming-card", { hasText: "Peer Reject" }).getByRole("button", { name: "Reject" }).click();

  await page.getByRole("button", { name: "History" }).click();
  await expect(page.getByText("Peer Reject")).toBeVisible();
  await expect(page.locator(".status-badge.rejected").first()).toBeVisible();
});

test("expired incoming action shows expiry error and does not create active transfer", async ({ page }) => {
  await page.getByRole("button", { name: "Transfers" }).click();
  await page.evaluate(() => {
    (window as any).__seedIncomingTransfer("t-expired", "Peer Expired");
    (window as any).__expireIncomingTransfer("t-expired");
  });

  const expiredCard = page.locator("article.incoming-card", { hasText: "Peer Expired" });
  await expect(expiredCard).toBeVisible();
  await expiredCard.getByRole("button", { name: "Accept", exact: true }).click();
  await page.getByRole("checkbox", { name: "I compared this shared code on both devices" }).check();
  await page.getByRole("button", { name: "Continue" }).click();

  await expect(page.getByText("This transfer request expired")).toBeVisible();
  await expect(expiredCard).toHaveCount(0);
  await expect(page.locator(".transfer-card", { hasText: "Peer Expired" })).toHaveCount(0);
});

test("connect by address dialog/confirm flow", async ({ page }) => {
  await page.getByRole("button", { name: "Transfers" }).click();
  await page.getByRole("button", { name: "Connect by Address" }).click();
  await page.getByLabel("Peer address").fill("127.0.0.1:7001");
  await page.locator(".dialog-actions").getByRole("button", { name: "Connect" }).click();
  await expect(page.getByText("Manual Peer")).toBeVisible();
  await expect(page.locator(".preview-row", { hasText: "Shared Verification Code" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Confirm Fingerprint" })).toBeDisabled();
  await page.getByRole("checkbox", { name: "I compared this shared code on both devices" }).check();
  await page.getByRole("button", { name: "Confirm Fingerprint" }).click();

  const state = await page.evaluate(() => (window as any).__getTestState());
  expect(state.connectByAddressCalls).toContain("127.0.0.1:7001");
});

test("history auto-refreshes on terminal event", async ({ page }) => {
  await page.getByRole("button", { name: "History" }).click();
  await expect(page.getByText("No past transfers.")).toBeVisible();

  await page.evaluate(() => {
    const transfer = {
      id: "t-auto",
      direction: "Receive",
      peer_fingerprint: "t-auto-fp",
      peer_name: "Peer Auto",
      items: [{ file_id: 1, name: "auto.txt", rel_path: "auto.txt", size: 1 }],
      status: "Completed",
      bytes_transferred: 1,
      total_bytes: 1,
      revision: 2,
      ended_at_unix: 1,
    };
    (window as any).__pushHistoryTransfer(transfer);
    (window as any).__emitTestEvent("transfer_complete", { transfer_id: "t-auto", revision: 2 });
  });

  await expect(page.getByText("Peer Auto")).toBeVisible();
});

test("requested daemon fallback still uses in-process runtime events", async ({ page }) => {
  await page.evaluate(() => {
    (window as any).__setControlPlaneModes("daemon", "in_process");
    (window as any).__seedIncomingTransfer("t-fallback", "Peer Fallback");
  });

  await page.getByRole("button", { name: "Transfers" }).click();
  await expect(page.locator("article.incoming-card", { hasText: "Peer Fallback" })).toBeVisible();
});

test("daemon mode ignores local-only listeners and consumes daemon event feed", async ({ page }) => {
  await page.goto("/?requestedControlPlaneMode=daemon&controlPlaneMode=daemon");
  await page.getByRole("button", { name: "Transfers" }).click();

  await page.evaluate(() => {
    (window as any).__emitLocalOnlyEvent("transfer_incoming", {
      transfer_id: "t-local-only",
      sender_name: "Peer Local Only",
      sender_fp: "peer-local-only",
      trusted: false,
      items: [{ file_id: 1, name: "local-only.txt", rel_path: "local-only.txt", size: 1 }],
      total_size: 1,
      revision: 0,
    });
  });

  await expect(page.locator("article.incoming-card", { hasText: "Peer Local Only" })).toHaveCount(0);

  await page.evaluate(() => {
    (window as any).__emitRuntimeOnlyEvent("transfer_incoming", {
      transfer_id: "t-daemon-feed",
      sender_name: "Peer Daemon Feed",
      sender_fp: "peer-daemon-feed",
      trusted: false,
      items: [{ file_id: 1, name: "daemon-feed.txt", rel_path: "daemon-feed.txt", size: 1 }],
      total_size: 1,
      revision: 0,
    });
  });

  await expect(page.locator("article.incoming-card", { hasText: "Peer Daemon Feed" })).toBeVisible();
});

test("identity mismatch warning is visible", async ({ page }) => {
  await page.getByRole("button", { name: "Nearby", exact: true }).click();
  await page.evaluate(() => {
    (window as any).__emitTestEvent("identity_mismatch", {
      expected_fp: "expected-fp",
      actual_fp: "actual-fp",
      phase: "connect",
    });
  });
  await expect(page.getByText("Security warning (connect): peer identity mismatch")).toBeVisible();
  await page.getByRole("button", { name: "Open Security Events" }).click();
  await expect(page.getByRole("heading", { name: "Security Events" })).toBeVisible();
});

test("daemon background hide notice links to transfers", async ({ page }) => {
  await page.evaluate(() => {
    (window as any).__emitTestEvent("system_error", {
      code: "DAEMON_UI_HIDDEN",
      subsystem: "ui_shell",
      message:
        "DashDrop is still running in the background. Active transfers and discovery stay attached to the daemon control plane.",
    });
  });

  await expect(page.getByText("DashDrop is still running in the background.")).toBeVisible();
  await page.getByRole("button", { name: "Open Transfers" }).click();
  await expect(page.getByRole("heading", { name: "Transfers" })).toBeVisible();
});

test("app window revealed clears daemon background hide notice", async ({ page }) => {
  await page.evaluate(() => {
    (window as any).__emitTestEvent("system_error", {
      code: "DAEMON_UI_HIDDEN",
      subsystem: "ui_shell",
      message:
        "DashDrop is still running in the background. Active transfers and discovery stay attached to the daemon control plane.",
    });
  });

  await expect(page.getByText("DashDrop is still running in the background.")).toBeVisible();

  await page.evaluate(() => {
    (window as any).__emitTestEvent("app_window_revealed", {
      source: "reopen",
    });
  });

  await expect(page.getByText("DashDrop is still running in the background.")).toHaveCount(0);
});

test("daemon event feed warning clears after recovery", async ({ page }) => {
  await page.evaluate(() => {
    (window as any).__emitTestEvent("system_error", {
      code: "DAEMON_EVENT_FEED_UNAVAILABLE",
      subsystem: "daemon_event_feed",
      message:
        "DashDrop temporarily lost contact with the daemon event feed. The UI will retry automatically.",
    });
  });

  await expect(page.getByText("DashDrop temporarily lost contact with the daemon event feed.")).toBeVisible();
  await page.getByRole("button", { name: "Open Settings" }).click();
  await expect(page.getByRole("heading", { name: "Settings" })).toBeVisible();

  await page.evaluate(() => {
    (window as any).__emitTestEvent("daemon_control_plane_recovered", {
      source: "daemon_event_feed",
    });
  });

  await expect(page.getByText("DashDrop temporarily lost contact with the daemon event feed.")).toHaveCount(0);
});

test("daemon event feed polling failure warns and auto-recovers", async ({ page }) => {
  await page.goto("/?requestedControlPlaneMode=daemon&controlPlaneMode=daemon&runtimeFeedFailureCount=3");

  const warning = page.getByText("DashDrop temporarily lost contact with the daemon event feed.");
  await expect(warning).toBeVisible({ timeout: 5000 });
  await expect(warning).toHaveCount(0, { timeout: 5000 });
});

test("settings runtime cards keep requested mode separate from actual control plane", async ({ page }) => {
  await page.goto("/?requestedControlPlaneMode=daemon&controlPlaneMode=in_process");
  await page.locator(".rail-nav").getByRole("button", { name: "Settings" }).click();

  await expect(page.locator(".runtime-card", { hasText: "Requested Mode" })).toContainText("daemon");
  await expect(page.locator(".runtime-card", { hasText: "Control Plane" })).toContainText("in_process");
  await expect(page.locator(".runtime-card", { hasText: "Daemon Status" })).toContainText("connected_after_retry");
});

test("history filters by peer and status", async ({ page }) => {
  await page.evaluate(() => {
    (window as any).__pushHistoryTransfer({
      id: "t-failed",
      direction: "Send",
      peer_fingerprint: "fp-failed",
      peer_name: "Alpha Peer",
      items: [{ file_id: 1, name: "a.txt", rel_path: "a.txt", size: 1 }],
      status: "Failed",
      bytes_transferred: 0,
      total_bytes: 1,
      revision: 1,
      ended_at_unix: 2,
      error: "E_NETWORK",
    });
    (window as any).__pushHistoryTransfer({
      id: "t-complete",
      direction: "Receive",
      peer_fingerprint: "fp-complete",
      peer_name: "Beta Peer",
      items: [{ file_id: 2, name: "b.txt", rel_path: "b.txt", size: 1 }],
      status: "Completed",
      bytes_transferred: 1,
      total_bytes: 1,
      revision: 1,
      ended_at_unix: 3,
    });
  });

  await page.getByRole("button", { name: "History" }).click();
  await expect(page.getByText("Alpha Peer")).toBeVisible();
  await expect(page.getByText("Beta Peer")).toBeVisible();

  await page.getByPlaceholder("Filter by peer name or fingerprint").fill("alpha");
  await expect(page.getByText("Alpha Peer")).toBeVisible();
  await expect(page.getByText("Beta Peer")).toHaveCount(0);

  await page.getByRole("combobox").nth(2).selectOption("Failed");
  await expect(page.locator(".status-badge.failed").first()).toBeVisible();
});

test("cancel all active transfers invokes command", async ({ page }) => {
  await page.evaluate(() => {
    (window as any).__emitTestEvent("transfer_started", {
      transfer_id: "t-running",
      peer_fp: "fp-run",
      peer_name: "Run Peer",
      items: [{ file_id: 1, name: "a.txt", rel_path: "a.txt", size: 1 }],
      total_size: 1,
      revision: 0,
    });
  });

  await page.getByRole("button", { name: "Transfers" }).click();
  await page.getByRole("button", { name: "Cancel All Active" }).click();
  const state = await page.evaluate(() => (window as any).__getTestState());
  expect(state.cancelAllCalls).toBe(1);
});

test("retry button triggers retry command for failed send", async ({ page }) => {
  await page.evaluate(() => {
    (window as any).__emitTestEvent("transfer_started", {
      transfer_id: "t-retry",
      peer_fp: "fp-retry",
      peer_name: "Retry Peer",
      items: [{ file_id: 1, name: "r.txt", rel_path: "r.txt", size: 1 }],
      total_size: 1,
      revision: 0,
    });
    (window as any).__emitTestEvent("transfer_failed", {
      transfer_id: "t-retry",
      reason_code: "E_PROTOCOL",
      terminal_cause: "TransferFailed",
      revision: 1,
    });
  });

  await page.getByRole("button", { name: "Transfers" }).click();
  await page.locator(".transfer-card", { hasText: "Retry Peer" }).getByRole("button", { name: "Retry" }).click();
  const state = await page.evaluate(() => (window as any).__getTestState());
  expect(state.retryCalls).toContain("t-retry");
});

test("nearby and trusted share online rule when session has no usable address", async ({ page }) => {
  await page.evaluate(() => {
    (window as any).__setTrustedPeers([
      {
        fingerprint: "fp-no-addr",
        name: "NoAddr Peer",
        paired_at: 1,
        alias: null,
        last_used_at: null,
      },
    ]);
    (window as any).__emitTestEvent("device_discovered", {
      fingerprint: "fp-no-addr",
      name: "NoAddr Peer",
      platform: "Windows",
      trusted: true,
      sessions: {
        "session-no-addr": {
          session_id: "session-no-addr",
          addrs: [],
          last_seen_unix: 1,
        },
      },
      last_seen: 1,
      reachability: "discovered",
      probe_fail_count: 0,
      last_probe_at: null,
    });
  });

  await page.getByRole("button", { name: "Nearby", exact: true }).click();
  await expect(page.getByText("Discovering (address pending)")).toBeVisible();
  await expect(page.locator(".device-card", { hasText: "NoAddr Peer" }).getByText("Unavailable")).toBeVisible();

  await page.getByRole("button", { name: "Trusted Devices" }).click();
  const trustedCard = page.locator(".trusted-card", { hasText: "NoAddr Peer" });
  await expect(trustedCard.getByText("Offline")).toBeVisible();
});

test("external share event queues files and nearby sends them to selected device", async ({ page }) => {
  await page.evaluate(() => {
    const trustedDevice = {
      fingerprint: "fp-trusted",
      name: "Trusted Peer",
      platform: "Mac",
      trusted: true,
      sessions: {
        "s-trusted": {
          session_id: "s-trusted",
          addrs: ["192.168.1.7:7000"],
          last_seen_unix: 1,
        },
      },
      last_seen: 1,
      reachability: "reachable",
      probe_fail_count: 0,
      last_probe_at: null,
    };
    (window as any).__setMockDevices([trustedDevice]);
    (window as any).__emitTestEvent("device_discovered", trustedDevice);
    (window as any).__emitTestEvent("external_share_received", {
      paths: ["/tmp/from-share/a.txt", "/tmp/from-share/b.txt"],
      source: "local_ipc",
    });
  });

  await page.getByRole("button", { name: "Nearby", exact: true }).click();
  await expect(page.locator(".share-banner")).toContainText("2 shared items");
  await page.locator(".device-card", { hasText: "Trusted Peer" }).click();

  const state = await page.evaluate(() => (window as any).__getTestState());
  expect(state.sendFileCalls).toContainEqual({
    peerFp: "fp-trusted",
    paths: ["/tmp/from-share/a.txt", "/tmp/from-share/b.txt"],
  });
});

test("settings diagnostics copy includes extended discovery fields", async ({ page }) => {
  await page.locator(".app-rail").getByRole("button", { name: "Settings", exact: true }).click();
  await expect(page.getByText("Verification Code")).toBeVisible();
  await expect(page.getByText("packaged")).toBeVisible();
  await expect(page.getByText("attach_existing")).toBeVisible();
  await expect(page.getByText("3", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Copy Discovery Diagnostics" }).click();

  const copied = await page.evaluate(() => (window as any).__getClipboardText());
  const diagnostics = JSON.parse(copied);

  expect(diagnostics.runtime.runtime_profile).toBe("packaged");
  expect(diagnostics.runtime.daemon_connect_attempts).toBe(3);
  expect(diagnostics.runtime.daemon_connect_strategy).toBe("attach_existing");
  expect(diagnostics.service_type).toBe("_dashdrop._udp.local.");
  expect(diagnostics.listener_mode).toBe("dual_stack");
  expect(Array.isArray(diagnostics.listener_addrs)).toBeTruthy();
  expect(Array.isArray(diagnostics.network_interfaces)).toBeTruthy();
  expect(diagnostics.browser_status?.active).toBeTruthy();
});
