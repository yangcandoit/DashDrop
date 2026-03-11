import { spawn } from "node:child_process";
import { once } from "node:events";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import process from "node:process";

const READY_TIMEOUT_MS = 120_000;
const STABLE_WINDOW_MS = 4_000;
const SHUTDOWN_TIMEOUT_MS = 15_000;
const OUTPUT_LIMIT = 120;

const readyMarkers = [
  "Running `target/debug/dashdrop`",
  "Running `target\\debug\\dashdrop.exe`",
];

const outputLines = [];
const configDir = path.join(
  os.tmpdir(),
  `dashdrop-tauri-smoke-${Date.now()}-${Math.random().toString(16).slice(2)}`,
);
const startupErrorLog = path.join(configDir, "startup-error.log");

let child;
let settled = false;
let ready = false;
let readyTimer = null;
let stableTimer = null;

function stripAnsi(value) {
  return value.replace(/\u001B\[[0-?]*[ -/]*[@-~]/g, "");
}

function rememberOutput(chunk) {
  const text = stripAnsi(String(chunk));
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim()) {
      continue;
    }
    outputLines.push(line);
    if (outputLines.length > OUTPUT_LIMIT) {
      outputLines.shift();
    }
  }
}

function recentOutput() {
  return outputLines.length
    ? outputLines.join("\n")
    : "(no process output captured)";
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function cleanupConfigDir() {
  await fs.rm(configDir, { recursive: true, force: true });
}

async function terminateChild() {
  if (!child || child.exitCode !== null) {
    return;
  }

  try {
    if (process.platform === "win32") {
      const killer = spawn("taskkill", ["/pid", String(child.pid), "/t", "/f"], {
        stdio: "ignore",
      });
      await once(killer, "exit");
    } else {
      process.kill(-child.pid, "SIGINT");
    }
  } catch {}

  const exited = await Promise.race([
    once(child, "exit").then(() => true),
    delay(SHUTDOWN_TIMEOUT_MS).then(() => false),
  ]);

  if (exited) {
    return;
  }

  try {
    if (process.platform === "win32") {
      const killer = spawn("taskkill", ["/pid", String(child.pid), "/t", "/f"], {
        stdio: "ignore",
      });
      await once(killer, "exit");
    } else {
      process.kill(-child.pid, "SIGKILL");
    }
  } catch {}

  await Promise.race([once(child, "exit"), delay(2_000)]);
}

async function finish(code, message) {
  if (settled) {
    return;
  }
  settled = true;

  if (readyTimer) {
    clearTimeout(readyTimer);
  }
  if (stableTimer) {
    clearTimeout(stableTimer);
  }

  await terminateChild();

  try {
    await cleanupConfigDir();
  } catch {}

  if (code === 0) {
    console.log(message);
    process.exit(0);
  }

  console.error(message);
  if (outputLines.length) {
    console.error("\nRecent output:\n" + recentOutput());
  }
  process.exit(code);
}

function checkReady(chunk) {
  if (ready) {
    return;
  }
  const text = stripAnsi(String(chunk));
  if (!readyMarkers.some((marker) => text.includes(marker))) {
    return;
  }

  ready = true;
  stableTimer = setTimeout(async () => {
    try {
      await fs.access(startupErrorLog);
      await finish(
        1,
        `tauri runtime smoke failed: startup error log was written to ${startupErrorLog}`,
      );
      return;
    } catch {}

    await finish(
      0,
      `tauri runtime smoke passed (stable for ${STABLE_WINDOW_MS}ms after launch)`,
    );
  }, STABLE_WINDOW_MS);
}

async function main() {
  await fs.mkdir(configDir, { recursive: true });

  child = spawn("npm", ["run", "tauri", "dev"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      DASHDROP_CONFIG_DIR: configDir,
      TAURI_DEV_HOST: "127.0.0.1",
    },
    detached: process.platform !== "win32",
    stdio: ["ignore", "pipe", "pipe"],
  });

  child.stdout.on("data", (chunk) => {
    rememberOutput(chunk);
    checkReady(chunk);
  });

  child.stderr.on("data", (chunk) => {
    rememberOutput(chunk);
    checkReady(chunk);
  });

  child.on("exit", async (code, signal) => {
    if (settled) {
      return;
    }
    await finish(
      1,
      `tauri runtime smoke failed: process exited before stabilization (code=${code}, signal=${signal ?? "none"})`,
    );
  });

  readyTimer = setTimeout(async () => {
    await finish(
      1,
      `tauri runtime smoke failed: app did not reach launch marker within ${READY_TIMEOUT_MS}ms`,
    );
  }, READY_TIMEOUT_MS);
}

await main();
