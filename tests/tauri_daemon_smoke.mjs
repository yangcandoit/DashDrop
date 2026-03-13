import { spawn, execFileSync } from "node:child_process";
import { once } from "node:events";
import fs from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import process from "node:process";

const READY_TIMEOUT_MS = 120_000;
const STABLE_WINDOW_MS = 4_000;
const SHUTDOWN_TIMEOUT_MS = 15_000;
const OUTPUT_LIMIT = 160;
const artifactDir = process.env.DASHDROP_SMOKE_ARTIFACT_DIR
  ? path.resolve(process.cwd(), process.env.DASHDROP_SMOKE_ARTIFACT_DIR)
  : null;

const uiReadyMarkers = [
  "Running `target/debug/dashdrop`",
  "Running `target\\debug\\dashdrop.exe`",
  "Running DevCommand (`cargo  run --no-default-features --color always --`)",
  "Finished `dev` profile",
];
const outputLines = [];
const configDir = path.join(
  os.tmpdir(),
  `dashdrop-tauri-daemon-smoke-${Date.now()}-${Math.random().toString(16).slice(2)}`,
);
const startupErrorLog = path.join(configDir, "startup-error.log");

let daemonChild;
let uiChild;
let settled = false;
let daemonReady = false;
let uiReady = false;
let readyTimer = null;
let stableTimer = null;

function stripAnsi(value) {
  return value.replace(/\u001B\[[0-?]*[ -/]*[@-~]/g, "");
}

function rememberOutput(chunk, prefix = "") {
  const text = stripAnsi(String(chunk));
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim()) {
      continue;
    }
    outputLines.push(prefix ? `[${prefix}] ${line}` : line);
    if (outputLines.length > OUTPUT_LIMIT) {
      outputLines.shift();
    }
  }
}

function recentOutput() {
  return outputLines.length ? outputLines.join("\n") : "(no process output captured)";
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function currentTargetTriple() {
  const output = execFileSync("rustc", ["-vV"], {
    cwd: process.cwd(),
    encoding: "utf8",
  });
  const line = output
    .split("\n")
    .find((entry) => entry.toLowerCase().startsWith("host:"));
  if (!line) {
    throw new Error("failed to resolve Rust host target triple");
  }
  return line.slice(line.indexOf(":") + 1).trim();
}

function sidecarBinaryPath() {
  const targetTriple = currentTargetTriple();
  const ext = targetTriple.includes("windows") ? ".exe" : "";
  return path.join(
    process.cwd(),
    "src-tauri",
    "binaries",
    `dashdropd-${targetTriple}${ext}`,
  );
}

function getFreePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close(() => reject(new Error("failed to resolve free TCP port")));
        return;
      }
      const { port } = address;
      server.close((closeError) => {
        if (closeError) {
          reject(closeError);
          return;
        }
        resolve(port);
      });
    });
  });
}

async function cleanupConfigDir() {
  await fs.rm(configDir, { recursive: true, force: true });
}

async function writeFailureArtifacts() {
  if (!artifactDir) {
    return;
  }

  await fs.mkdir(artifactDir, { recursive: true });
  await fs.writeFile(path.join(artifactDir, "recent-output.log"), recentOutput() + "\n", "utf8");
  try {
    const startupError = await fs.readFile(startupErrorLog, "utf8");
    await fs.writeFile(path.join(artifactDir, "startup-error.log"), startupError, "utf8");
  } catch {}
}

async function terminateProcess(child) {
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

  await Promise.all([terminateProcess(uiChild), terminateProcess(daemonChild)]);

  if (code !== 0) {
    try {
      await writeFailureArtifacts();
    } catch {}
  }

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

function maybeArmStableWindow() {
  if (!daemonReady || !uiReady || stableTimer) {
    return;
  }

  stableTimer = setTimeout(async () => {
    try {
      await fs.access(startupErrorLog);
      await finish(
        1,
        `tauri daemon smoke failed: startup error log was written to ${startupErrorLog}`,
      );
      return;
    } catch {}

    await finish(
      0,
      `tauri daemon smoke passed (daemon + UI stable for ${STABLE_WINDOW_MS}ms)`,
    );
  }, STABLE_WINDOW_MS);
}

function watchDaemonOutput(chunk) {
  rememberOutput(chunk, "daemon");
  if (!daemonReady) {
    daemonReady = true;
  }
  maybeArmStableWindow();
}

function watchUiOutput(chunk) {
  rememberOutput(chunk, "ui");
  const text = stripAnsi(String(chunk));
  if (!uiReady && uiReadyMarkers.some((marker) => text.includes(marker))) {
    uiReady = true;
  }
  maybeArmStableWindow();
}

async function runPrepareSidecar() {
  await new Promise((resolve, reject) => {
    const child = spawn("npm", ["run", "tauri:prepare-sidecar"], {
      cwd: process.cwd(),
      stdio: ["ignore", "pipe", "pipe"],
    });

    child.stdout.on("data", (chunk) => {
      process.stdout.write(chunk);
      rememberOutput(chunk, "prepare");
    });
    child.stderr.on("data", (chunk) => {
      process.stderr.write(chunk);
      rememberOutput(chunk, "prepare");
    });

    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(
        new Error(
          `prepare sidecar failed (code=${code}, signal=${signal ?? "none"})\n${recentOutput()}`,
        ),
      );
    });
  });
}

async function main() {
  await fs.mkdir(configDir, { recursive: true });
  await runPrepareSidecar();

  const daemonBinary = sidecarBinaryPath();
  const devPort = await getFreePort();
  const hmrPort = await getFreePort();

  daemonChild = spawn(daemonBinary, [], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      DASHDROP_CONFIG_DIR: configDir,
    },
    detached: process.platform !== "win32",
    stdio: ["ignore", "pipe", "pipe"],
  });

  daemonChild.stdout.on("data", watchDaemonOutput);
  daemonChild.stderr.on("data", watchDaemonOutput);
  daemonReady = true;
  maybeArmStableWindow();
  daemonChild.on("exit", async (code, signal) => {
    if (settled) {
      return;
    }
    await finish(
      1,
      `tauri daemon smoke failed: daemon exited early (code=${code}, signal=${signal ?? "none"})`,
    );
  });

  uiChild = spawn("npm", ["run", "tauri", "dev"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      DASHDROP_CONFIG_DIR: configDir,
      DASHDROP_CONTROL_PLANE_MODE: "daemon",
      TAURI_DEV_HOST: "127.0.0.1",
      DASHDROP_TAURI_DEV_PORT: String(devPort),
      DASHDROP_TAURI_HMR_PORT: String(hmrPort),
      DASHDROP_TAURI_DEV_URL: `http://127.0.0.1:${devPort}`,
    },
    detached: process.platform !== "win32",
    stdio: ["ignore", "pipe", "pipe"],
  });

  uiChild.stdout.on("data", watchUiOutput);
  uiChild.stderr.on("data", watchUiOutput);
  uiChild.on("exit", async (code, signal) => {
    if (settled) {
      return;
    }
    await finish(
      1,
      `tauri daemon smoke failed: UI process exited early (code=${code}, signal=${signal ?? "none"})`,
    );
  });

  readyTimer = setTimeout(async () => {
    await finish(
      1,
      `tauri daemon smoke failed: daemon-backed runtime did not reach readiness within ${READY_TIMEOUT_MS}ms`,
    );
  }, READY_TIMEOUT_MS);
}

await main();
