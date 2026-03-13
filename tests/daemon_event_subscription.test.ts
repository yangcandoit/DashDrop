import assert from "node:assert/strict";
import test from "node:test";

import {
  __resetDaemonRuntimeEventLoopForTests,
  subscribeDaemonRuntimeEvents,
} from "../src/ipc.ts";

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
};

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function createStorage() {
  const values = new Map<string, string>();
  return {
    getItem(key: string) {
      return values.has(key) ? values.get(key)! : null;
    },
    setItem(key: string, value: string) {
      values.set(key, String(value));
    },
    removeItem(key: string) {
      values.delete(key);
    },
    clear() {
      values.clear();
    },
  };
}

async function waitFor(predicate: () => boolean, timeoutMs = 1_000): Promise<void> {
  const startedAt = Date.now();
  while (!predicate()) {
    if (Date.now() - startedAt > timeoutMs) {
      throw new Error(`condition was not met within ${timeoutMs}ms`);
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

test("shared daemon poller keeps running when a new subscriber replaces the previous one mid-poll", async () => {
  __resetDaemonRuntimeEventLoopForTests();

  const storage = createStorage();
  const firstPoll = deferred<{
    events: Array<{ seq: number; event: string; payload: Record<string, never>; emitted_at_unix_ms: number }>;
    generation: string;
    oldest_available_seq: number | null;
    latest_available_seq: number;
    resync_required: boolean;
  }>();
  const thirdPoll = deferred<{
    events: [];
    generation: string;
    oldest_available_seq: null;
    latest_available_seq: number;
    resync_required: boolean;
  }>();
  const pollStarts: number[] = [];
  let pollCount = 0;

  const originalWindow = globalThis.window;
  const originalLocalStorage = globalThis.localStorage;

  const mockWindow = {
    localStorage: storage,
    setTimeout: ((handler: TimerHandler, _timeout?: number) => {
      return globalThis.setTimeout(handler, 0);
    }) as typeof setTimeout,
    clearTimeout: globalThis.clearTimeout.bind(globalThis),
    __DASHDROP_TEST_MOCK__: {
      invoke: async (command: string) => {
        if (command === "get_runtime_event_checkpoint") {
          return null;
        }
        if (command === "set_runtime_event_checkpoint") {
          return null;
        }
        if (command === "get_runtime_events") {
          pollCount += 1;
          pollStarts.push(pollCount);
          if (pollCount === 1) {
            return firstPoll.promise;
          }
          if (pollCount === 2) {
            return {
              events: [
                {
                  seq: 1,
                  event: "device_discovered",
                  payload: {},
                  emitted_at_unix_ms: Date.now(),
                },
              ],
              generation: "gen-1",
              oldest_available_seq: 1,
              latest_available_seq: 1,
              resync_required: false,
            };
          }
          return thirdPoll.promise;
        }
        throw new Error(`unexpected command: ${command}`);
      },
      listen: async () => () => {},
    },
  } as unknown as Window;

  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: mockWindow,
  });
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: storage,
  });

  const received: string[] = [];

  try {
    const unsubscribeFirst = await subscribeDaemonRuntimeEvents(() => {
      received.push("first");
    });
    await waitFor(() => pollStarts.includes(1));

    unsubscribeFirst();

    const unsubscribeSecond = await subscribeDaemonRuntimeEvents((event) => {
      received.push(String(event.event));
    });

    firstPoll.resolve({
      events: [],
      generation: "gen-1",
      oldest_available_seq: null,
      latest_available_seq: 0,
      resync_required: false,
    });

    await waitFor(() => received.includes("device_discovered"));
    assert.ok(pollCount >= 2, "shared daemon poller should continue into the next poll cycle");

    unsubscribeSecond();
    thirdPoll.resolve({
      events: [],
      generation: "gen-1",
      oldest_available_seq: null,
      latest_available_seq: 1,
      resync_required: false,
    });
    await new Promise((resolve) => setTimeout(resolve, 10));
  } finally {
    firstPoll.reject(new Error("test cleanup"));
    thirdPoll.reject(new Error("test cleanup"));
    __resetDaemonRuntimeEventLoopForTests();
    if (originalWindow === undefined) {
      Reflect.deleteProperty(globalThis, "window");
    } else {
      Object.defineProperty(globalThis, "window", {
        configurable: true,
        value: originalWindow,
      });
    }
    if (originalLocalStorage === undefined) {
      Reflect.deleteProperty(globalThis, "localStorage");
    } else {
      Object.defineProperty(globalThis, "localStorage", {
        configurable: true,
        value: originalLocalStorage,
      });
    }
  }
});

test("shared daemon poller emits unavailable notice after repeated failures and recovery when feed resumes", async () => {
  __resetDaemonRuntimeEventLoopForTests();

  const storage = createStorage();
  const thirdFailure = deferred<never>();
  let pollCount = 0;

  const originalWindow = globalThis.window;
  const originalLocalStorage = globalThis.localStorage;
  const originalConsoleError = console.error;

  const mockWindow = {
    localStorage: storage,
    setTimeout: ((handler: TimerHandler, _timeout?: number) => {
      return globalThis.setTimeout(handler, 0);
    }) as typeof setTimeout,
    clearTimeout: globalThis.clearTimeout.bind(globalThis),
    __DASHDROP_TEST_MOCK__: {
      invoke: async (command: string) => {
        if (command === "get_runtime_event_checkpoint") {
          return null;
        }
        if (command === "set_runtime_event_checkpoint") {
          return null;
        }
        if (command === "get_runtime_events") {
          pollCount += 1;
          if (pollCount < 3) {
            throw new Error(`temporary daemon feed failure ${pollCount}`);
          }
          if (pollCount === 3) {
            return thirdFailure.promise;
          }
          return {
            events: [],
            generation: "gen-2",
            oldest_available_seq: null,
            latest_available_seq: 0,
            resync_required: false,
          };
        }
        throw new Error(`unexpected command: ${command}`);
      },
      listen: async () => () => {},
    },
  } as unknown as Window;

  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: mockWindow,
  });
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: storage,
  });

  const receivedEvents: string[] = [];
  const receivedPayloads: unknown[] = [];

  try {
    console.error = () => {};
    const unsubscribe = await subscribeDaemonRuntimeEvents((event) => {
      receivedEvents.push(String(event.event));
      receivedPayloads.push(event.payload);
    });

    await waitFor(() => pollCount === 3);
    assert.ok(
      !receivedEvents.includes("system_error"),
      "feed unavailability should not be reported before the third consecutive failure resolves",
    );

    thirdFailure.reject(new Error("third temporary daemon feed failure"));

    await waitFor(() => receivedEvents.includes("system_error"));
    await waitFor(() => receivedEvents.includes("daemon_control_plane_recovered"));

    const unavailableIndex = receivedEvents.indexOf("system_error");
    const recoveredIndex = receivedEvents.indexOf("daemon_control_plane_recovered");
    assert.ok(unavailableIndex >= 0, "unavailable notice should be emitted after repeated failures");
    assert.ok(
      recoveredIndex > unavailableIndex,
      "recovery event should arrive after the unavailable notice once polling succeeds again",
    );

    unsubscribe();
    await new Promise((resolve) => setTimeout(resolve, 10));
  } finally {
    thirdFailure.reject(new Error("test cleanup"));
    __resetDaemonRuntimeEventLoopForTests();
    if (originalWindow === undefined) {
      Reflect.deleteProperty(globalThis, "window");
    } else {
      Object.defineProperty(globalThis, "window", {
        configurable: true,
        value: originalWindow,
      });
    }
    if (originalLocalStorage === undefined) {
      Reflect.deleteProperty(globalThis, "localStorage");
    } else {
      Object.defineProperty(globalThis, "localStorage", {
        configurable: true,
        value: originalLocalStorage,
      });
    }
    console.error = originalConsoleError;
  }
});

test("shared daemon poller emits resync marker and persists latest cursor when generation changes", async () => {
  __resetDaemonRuntimeEventLoopForTests();

  const storage = createStorage();
  const savedCheckpoints: Array<{ consumerId: string; generation: string; seq: number }> = [];
  let pollCount = 0;

  const originalWindow = globalThis.window;
  const originalLocalStorage = globalThis.localStorage;

  const mockWindow = {
    localStorage: storage,
    setTimeout: ((handler: TimerHandler, _timeout?: number) => {
      return globalThis.setTimeout(handler, 0);
    }) as typeof setTimeout,
    clearTimeout: globalThis.clearTimeout.bind(globalThis),
    __DASHDROP_TEST_MOCK__: {
      invoke: async (command: string, args?: Record<string, unknown>) => {
        if (command === "get_runtime_event_checkpoint") {
          return null;
        }
        if (command === "set_runtime_event_checkpoint") {
          savedCheckpoints.push({
            consumerId: String(args?.consumerId ?? ""),
            generation: String(args?.generation ?? ""),
            seq: Number(args?.seq ?? 0),
          });
          return null;
        }
        if (command === "get_runtime_events") {
          pollCount += 1;
          if (pollCount === 1) {
            return {
              events: [
                {
                  seq: 4,
                  event: "device_discovered",
                  payload: {},
                  emitted_at_unix_ms: Date.now(),
                },
              ],
              generation: "gen-1",
              oldest_available_seq: 4,
              latest_available_seq: 4,
              resync_required: false,
            };
          }
          return {
            events: [],
            generation: "gen-2",
            oldest_available_seq: 8,
            latest_available_seq: 8,
            resync_required: false,
          };
        }
        throw new Error(`unexpected command: ${command}`);
      },
      listen: async () => () => {},
    },
  } as unknown as Window;

  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: mockWindow,
  });
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: storage,
  });

  const receivedEvents: string[] = [];
  const receivedPayloads: unknown[] = [];

  try {
    const unsubscribe = await subscribeDaemonRuntimeEvents((event) => {
      receivedEvents.push(String(event.event));
      receivedPayloads.push(event.payload);
    });

    await waitFor(() => receivedEvents.includes("daemon_event_feed_resync_required"));

    assert.deepEqual(receivedEvents.slice(0, 2), [
      "device_discovered",
      "daemon_event_feed_resync_required",
    ]);
    assert.deepEqual(receivedPayloads[1], {
      source: "daemon_event_feed",
      reason: "generation_changed",
      generation: "gen-2",
      oldest_available_seq: 8,
      latest_available_seq: 8,
      replay_source: "resync_required",
    });
    assert.ok(
      savedCheckpoints.some(
        (checkpoint) =>
          checkpoint.consumerId === "shared_ui_poller" &&
          checkpoint.generation === "gen-2" &&
          checkpoint.seq === 8,
      ),
      "generation change should persist the new daemon cursor",
    );
    assert.equal(
      storage.getItem("dashdrop_daemon_event_cursor_v1"),
      JSON.stringify({ afterSeq: 8, generation: "gen-2" }),
    );

    unsubscribe();
    await new Promise((resolve) => setTimeout(resolve, 10));
  } finally {
    __resetDaemonRuntimeEventLoopForTests();
    if (originalWindow === undefined) {
      Reflect.deleteProperty(globalThis, "window");
    } else {
      Object.defineProperty(globalThis, "window", {
        configurable: true,
        value: originalWindow,
      });
    }
    if (originalLocalStorage === undefined) {
      Reflect.deleteProperty(globalThis, "localStorage");
    } else {
      Object.defineProperty(globalThis, "localStorage", {
        configurable: true,
        value: originalLocalStorage,
      });
    }
  }
});

test("shared daemon poller emits resync marker when daemon feed explicitly requests resync", async () => {
  __resetDaemonRuntimeEventLoopForTests();

  const storage = createStorage();
  const savedCheckpoints: Array<{ consumerId: string; generation: string; seq: number }> = [];
  let pollCount = 0;

  const originalWindow = globalThis.window;
  const originalLocalStorage = globalThis.localStorage;

  const mockWindow = {
    localStorage: storage,
    setTimeout: ((handler: TimerHandler, _timeout?: number) => {
      return globalThis.setTimeout(handler, 0);
    }) as typeof setTimeout,
    clearTimeout: globalThis.clearTimeout.bind(globalThis),
    __DASHDROP_TEST_MOCK__: {
      invoke: async (command: string, args?: Record<string, unknown>) => {
        if (command === "get_runtime_event_checkpoint") {
          return null;
        }
        if (command === "set_runtime_event_checkpoint") {
          savedCheckpoints.push({
            consumerId: String(args?.consumerId ?? ""),
            generation: String(args?.generation ?? ""),
            seq: Number(args?.seq ?? 0),
          });
          return null;
        }
        if (command === "get_runtime_events") {
          pollCount += 1;
          if (pollCount === 1) {
            return {
              events: [
                {
                  seq: 2,
                  event: "device_discovered",
                  payload: {},
                  emitted_at_unix_ms: Date.now(),
                },
              ],
              generation: "gen-1",
              oldest_available_seq: 2,
              latest_available_seq: 2,
              resync_required: false,
            };
          }
          return {
            events: [],
            generation: "gen-1",
            oldest_available_seq: 2,
            latest_available_seq: 7,
            resync_required: true,
          };
        }
        throw new Error(`unexpected command: ${command}`);
      },
      listen: async () => () => {},
    },
  } as unknown as Window;

  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: mockWindow,
  });
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: storage,
  });

  const receivedEvents: string[] = [];
  const receivedPayloads: unknown[] = [];

  try {
    const unsubscribe = await subscribeDaemonRuntimeEvents((event) => {
      receivedEvents.push(String(event.event));
      receivedPayloads.push(event.payload);
    });

    await waitFor(() => receivedEvents.includes("daemon_event_feed_resync_required"));

    assert.deepEqual(receivedEvents.slice(0, 2), [
      "device_discovered",
      "daemon_event_feed_resync_required",
    ]);
    assert.deepEqual(receivedPayloads[1], {
      source: "daemon_event_feed",
      reason: "cursor_invalid",
      generation: "gen-1",
      oldest_available_seq: 2,
      latest_available_seq: 7,
      replay_source: "resync_required",
    });
    assert.ok(
      savedCheckpoints.some(
        (checkpoint) =>
          checkpoint.consumerId === "shared_ui_poller" &&
          checkpoint.generation === "gen-1" &&
          checkpoint.seq === 7,
      ),
      "explicit resync should persist the feed's latest seq",
    );
    assert.equal(
      storage.getItem("dashdrop_daemon_event_cursor_v1"),
      JSON.stringify({ afterSeq: 7, generation: "gen-1" }),
    );

    unsubscribe();
    await new Promise((resolve) => setTimeout(resolve, 10));
  } finally {
    __resetDaemonRuntimeEventLoopForTests();
    if (originalWindow === undefined) {
      Reflect.deleteProperty(globalThis, "window");
    } else {
      Object.defineProperty(globalThis, "window", {
        configurable: true,
        value: originalWindow,
      });
    }
    if (originalLocalStorage === undefined) {
      Reflect.deleteProperty(globalThis, "localStorage");
    } else {
      Object.defineProperty(globalThis, "localStorage", {
        configurable: true,
        value: originalLocalStorage,
      });
    }
  }
});

test("shared daemon poller prefers the newer backend checkpoint when persisted local cursor is stale", async () => {
  __resetDaemonRuntimeEventLoopForTests();

  const storage = createStorage();
  storage.setItem(
    "dashdrop_daemon_event_cursor_v1",
    JSON.stringify({ afterSeq: 3, generation: "gen-1" }),
  );
  let firstAfterSeq: number | null = null;
  let firstGeneration: string | null = null;

  const originalWindow = globalThis.window;
  const originalLocalStorage = globalThis.localStorage;

  const mockWindow = {
    localStorage: storage,
    setTimeout: ((handler: TimerHandler, _timeout?: number) => {
      return globalThis.setTimeout(handler, 0);
    }) as typeof setTimeout,
    clearTimeout: globalThis.clearTimeout.bind(globalThis),
    __DASHDROP_TEST_MOCK__: {
      invoke: async (command: string, args?: Record<string, unknown>) => {
        if (command === "get_runtime_event_checkpoint") {
          return {
            consumer_id: "shared_ui_poller",
            generation: "gen-1",
            seq: 9,
            updated_at_unix_ms: Date.now(),
          };
        }
        if (command === "set_runtime_event_checkpoint") {
          return null;
        }
        if (command === "get_runtime_events") {
          if (firstAfterSeq === null) {
            firstAfterSeq = Number(args?.afterSeq ?? -1);
            firstGeneration = JSON.parse(
              storage.getItem("dashdrop_daemon_event_cursor_v1") ?? "null",
            )?.generation ?? null;
          }
          return {
            events: [],
            generation: "gen-1",
            oldest_available_seq: 1,
            latest_available_seq: 9,
            resync_required: false,
          };
        }
        throw new Error(`unexpected command: ${command}`);
      },
      listen: async () => () => {},
    },
  } as unknown as Window;

  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: mockWindow,
  });
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: storage,
  });

  try {
    const unsubscribe = await subscribeDaemonRuntimeEvents(() => {});
    await waitFor(() => firstAfterSeq !== null);

    assert.equal(firstAfterSeq, 9, "daemon checkpoint should override an older local cursor");
    assert.equal(firstGeneration, "gen-1");
    assert.equal(
      storage.getItem("dashdrop_daemon_event_cursor_v1"),
      JSON.stringify({ afterSeq: 9, generation: "gen-1" }),
    );

    unsubscribe();
    await new Promise((resolve) => setTimeout(resolve, 10));
  } finally {
    __resetDaemonRuntimeEventLoopForTests();
    if (originalWindow === undefined) {
      Reflect.deleteProperty(globalThis, "window");
    } else {
      Object.defineProperty(globalThis, "window", {
        configurable: true,
        value: originalWindow,
      });
    }
    if (originalLocalStorage === undefined) {
      Reflect.deleteProperty(globalThis, "localStorage");
    } else {
      Object.defineProperty(globalThis, "localStorage", {
        configurable: true,
        value: originalLocalStorage,
      });
    }
  }
});

test("shared daemon poller avoids redundant checkpoint writes while cursor stays unchanged", async () => {
  __resetDaemonRuntimeEventLoopForTests();

  const storage = createStorage();
  const savedCheckpoints: Array<{ generation: string; seq: number }> = [];
  let pollCount = 0;

  const originalWindow = globalThis.window;
  const originalLocalStorage = globalThis.localStorage;

  const mockWindow = {
    localStorage: storage,
    setTimeout: ((handler: TimerHandler, _timeout?: number) => {
      return globalThis.setTimeout(handler, 0);
    }) as typeof setTimeout,
    clearTimeout: globalThis.clearTimeout.bind(globalThis),
    __DASHDROP_TEST_MOCK__: {
      invoke: async (command: string, args?: Record<string, unknown>) => {
        if (command === "get_runtime_event_checkpoint") {
          return null;
        }
        if (command === "set_runtime_event_checkpoint") {
          savedCheckpoints.push({
            generation: String(args?.generation ?? ""),
            seq: Number(args?.seq ?? 0),
          });
          return null;
        }
        if (command === "get_runtime_events") {
          pollCount += 1;
          if (pollCount === 1) {
            return {
              events: [
                {
                  seq: 5,
                  event: "device_discovered",
                  payload: {},
                  emitted_at_unix_ms: Date.now(),
                },
              ],
              generation: "gen-5",
              oldest_available_seq: 5,
              latest_available_seq: 5,
              resync_required: false,
            };
          }
          return {
            events: [],
            generation: "gen-5",
            oldest_available_seq: 5,
            latest_available_seq: 5,
            resync_required: false,
          };
        }
        throw new Error(`unexpected command: ${command}`);
      },
      listen: async () => () => {},
    },
  } as unknown as Window;

  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: mockWindow,
  });
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: storage,
  });

  try {
    const unsubscribe = await subscribeDaemonRuntimeEvents(() => {});
    await waitFor(() => pollCount >= 2);

    assert.deepEqual(savedCheckpoints, [{ generation: "gen-5", seq: 5 }]);
    assert.equal(
      storage.getItem("dashdrop_daemon_event_cursor_v1"),
      JSON.stringify({ afterSeq: 5, generation: "gen-5" }),
    );

    unsubscribe();
    await new Promise((resolve) => setTimeout(resolve, 10));
  } finally {
    __resetDaemonRuntimeEventLoopForTests();
    if (originalWindow === undefined) {
      Reflect.deleteProperty(globalThis, "window");
    } else {
      Object.defineProperty(globalThis, "window", {
        configurable: true,
        value: originalWindow,
      });
    }
    if (originalLocalStorage === undefined) {
      Reflect.deleteProperty(globalThis, "localStorage");
    } else {
      Object.defineProperty(globalThis, "localStorage", {
        configurable: true,
        value: originalLocalStorage,
      });
    }
  }
});
