import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    type Handler = (payload: unknown) => void;
    const listeners = new Map<string, Set<Handler>>();
    const state = {
      devices: {} as Record<string, any>,
      transfers: {} as Record<string, any>,
      incoming: {} as Record<string, any>,
      history: [] as any[],
      connectByAddressCalls: [] as string[],
      retryCalls: [] as string[],
      cancelAllCalls: 0,
      trustedPeers: [] as any[],
    };

    const emit = (event: string, payload: unknown) => {
      const set = listeners.get(event);
      if (!set) return;
      for (const handler of set) handler(payload);
    };

    (window as any).__emitTestEvent = (event: string, payload: unknown) => emit(event, payload);
    (window as any).__getTestState = () => JSON.parse(JSON.stringify(state));
    (window as any).__setTrustedPeers = (peers: any[]) => {
      state.trustedPeers = peers.map((peer) => ({ ...peer }));
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
              local_port: 7000,
              mdns_registered: true,
              discovered_devices: 2,
              trusted_devices: state.trustedPeers.length,
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
                local_port: 7000,
                mdns_registered: true,
                discovered_devices: Object.keys(state.devices).length,
                trusted_devices: state.trustedPeers.length,
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
          case "retry_transfer":
            state.retryCalls.push(String(args?.transferId ?? ""));
            return null;
          case "get_security_posture":
            return { secure_store_available: true };
          case "get_security_events":
            return [];
          case "accept_transfer": {
            const transferId = String(args?.transferId);
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

test("connect by address dialog/confirm flow", async ({ page }) => {
  await page.getByRole("button", { name: "Transfers" }).click();
  await page.getByRole("button", { name: "Connect by Address" }).click();
  await page.getByLabel("Peer address").fill("127.0.0.1:7001");
  await page.locator(".dialog-actions").getByRole("button", { name: "Connect" }).click();
  await expect(page.getByText("Manual Peer")).toBeVisible();
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

test("identity mismatch warning is visible", async ({ page }) => {
  await page.getByRole("button", { name: "Nearby" }).click();
  await page.evaluate(() => {
    (window as any).__emitTestEvent("identity_mismatch", {
      expected_fp: "expected-fp",
      actual_fp: "actual-fp",
      phase: "connect",
    });
  });
  await expect(page.getByText("Security warning (connect): peer identity mismatch")).toBeVisible();
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

  await page.getByRole("button", { name: "Nearby" }).click();
  await expect(page.getByText("Discovering (address pending)")).toBeVisible();

  await page.getByRole("button", { name: "Trusted Devices" }).click();
  const trustedCard = page.locator(".trusted-card", { hasText: "NoAddr Peer" });
  await expect(trustedCard.getByText("Offline")).toBeVisible();
});

test("settings diagnostics copy includes extended discovery fields", async ({ page }) => {
  await page.locator(".app-rail").getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("button", { name: "Copy Discovery Diagnostics" }).click();

  const copied = await page.evaluate(() => (window as any).__getClipboardText());
  const diagnostics = JSON.parse(copied);

  expect(diagnostics.service_type).toBe("_dashdrop._udp.local.");
  expect(diagnostics.listener_mode).toBe("dual_stack");
  expect(Array.isArray(diagnostics.listener_addrs)).toBeTruthy();
  expect(Array.isArray(diagnostics.network_interfaces)).toBeTruthy();
  expect(diagnostics.browser_status?.active).toBeTruthy();
});
