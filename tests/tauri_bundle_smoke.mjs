import { spawn } from "node:child_process";
import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const OUTPUT_LIMIT = 160;
const outputLines = [];
const artifactDir = process.env.DASHDROP_SMOKE_ARTIFACT_DIR
  ? path.resolve(process.cwd(), process.env.DASHDROP_SMOKE_ARTIFACT_DIR)
  : null;

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
  return outputLines.length ? outputLines.join("\n") : "(no process output captured)";
}

async function pathExists(targetPath) {
  try {
    await fs.access(targetPath);
    return true;
  } catch {
    return false;
  }
}

async function writeFailureArtifacts(error) {
  if (!artifactDir) {
    return;
  }

  await fs.mkdir(artifactDir, { recursive: true });
  await fs.writeFile(path.join(artifactDir, "recent-output.log"), recentOutput() + "\n", "utf8");
  if (error instanceof Error) {
    await fs.writeFile(path.join(artifactDir, "failure-summary.log"), `${error.stack ?? error.message}\n`, "utf8");
  }

  const bundleRoot = path.join(process.cwd(), "src-tauri", "target", "release", "bundle");
  if (await pathExists(bundleRoot)) {
    const bundleListing = await runListing(bundleRoot);
    await fs.writeFile(path.join(artifactDir, "bundle-tree.log"), bundleListing, "utf8");
  }
}

function run(command, args, env = process.env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: process.cwd(),
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    child.stdout.on("data", (chunk) => {
      process.stdout.write(chunk);
      rememberOutput(chunk);
    });

    child.stderr.on("data", (chunk) => {
      process.stderr.write(chunk);
      rememberOutput(chunk);
    });

    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(
        new Error(
          `command failed (code=${code}, signal=${signal ?? "none"})\n${recentOutput()}`,
        ),
      );
    });
  });
}

async function findFirstMatch(root, matcher) {
  const entries = await fs.readdir(root, { withFileTypes: true });
  for (const entry of entries) {
    const candidate = path.join(root, entry.name);
    if (matcher(candidate, entry)) {
      return candidate;
    }
    if (entry.isDirectory()) {
      const nested = await findFirstMatch(candidate, matcher);
      if (nested) {
        return nested;
      }
    }
  }
  return null;
}

async function runListing(root) {
  const lines = [];

  async function walk(current, depth) {
    const entries = await fs.readdir(current, { withFileTypes: true });
    entries.sort((a, b) => a.name.localeCompare(b.name));
    for (const entry of entries) {
      const prefix = "  ".repeat(depth);
      lines.push(`${prefix}${entry.name}${entry.isDirectory() ? "/" : ""}`);
      if (entry.isDirectory()) {
        await walk(path.join(current, entry.name), depth + 1);
      }
    }
  }

  lines.push(path.basename(root) + "/");
  await walk(root, 1);
  return lines.join("\n") + "\n";
}

async function assertBundledSidecar() {
  const bundleRoot = path.join(process.cwd(), "src-tauri", "target", "release", "bundle");

  if (process.platform === "darwin") {
    const appBundle = await findFirstMatch(
      bundleRoot,
      (candidate, entry) => entry.isDirectory() && candidate.endsWith(".app"),
    );
    if (!appBundle) {
      throw new Error(`No .app bundle found under ${bundleRoot}`);
    }

    const mainBinary = path.join(appBundle, "Contents", "MacOS", "dashdrop");
    const sidecar = path.join(appBundle, "Contents", "MacOS", "dashdropd");
    const bleBridge = path.join(appBundle, "Contents", "MacOS", "dashdrop-ble-bridge");
    if (!(await pathExists(mainBinary))) {
      throw new Error(`Main app binary missing from bundle: ${mainBinary}`);
    }
    if (!(await pathExists(sidecar))) {
      throw new Error(`dashdropd sidecar missing from bundle: ${sidecar}`);
    }
    if (!(await pathExists(bleBridge))) {
      throw new Error(`dashdrop-ble-bridge sidecar missing from bundle: ${bleBridge}`);
    }

    console.log(`tauri bundle smoke passed: ${sidecar} + ${bleBridge}`);
    return;
  }

  const sidecar = await findFirstMatch(bundleRoot, (candidate, entry) => {
    if (!entry.isFile()) {
      return false;
    }
    const base = path.basename(candidate).toLowerCase();
    return base === "dashdropd" || base === "dashdropd.exe";
  });
  const bleBridge = await findFirstMatch(bundleRoot, (candidate, entry) => {
    if (!entry.isFile()) {
      return false;
    }
    const base = path.basename(candidate).toLowerCase();
    return base === "dashdrop-ble-bridge" || base === "dashdrop-ble-bridge.exe";
  });

  if (!sidecar) {
    throw new Error(`dashdropd sidecar was not found under ${bundleRoot}`);
  }
  if (process.platform === "win32" && !bleBridge) {
    throw new Error(`dashdrop-ble-bridge sidecar was not found under ${bundleRoot}`);
  }

  console.log(
    process.platform === "win32" && bleBridge
      ? `tauri bundle smoke passed: ${sidecar} + ${bleBridge}`
      : `tauri bundle smoke passed: ${sidecar}`,
  );
}

async function main() {
  try {
    await run("npm", ["run", "tauri", "build", "--", "--bundles", "app"]);
    await assertBundledSidecar();
  } catch (error) {
    try {
      await writeFailureArtifacts(error);
    } catch {}
    throw error;
  }
}

await main();
