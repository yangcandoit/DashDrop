import { chmodSync, copyFileSync, existsSync, mkdirSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");
const srcTauriDir = path.join(repoRoot, "src-tauri");
const binariesDir = path.join(srcTauriDir, "binaries");

function currentTargetTriple() {
  const output = execFileSync("rustc", ["-vV"], {
    cwd: repoRoot,
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

function binaryExtensionForTarget(targetTriple) {
  return targetTriple.includes("windows") ? ".exe" : "";
}

function copyAliasIfNeeded(sourcePath, destPath, extension) {
  if (sourcePath === destPath) {
    return;
  }
  copyFileSync(sourcePath, destPath);
  if (extension === "") {
    chmodSync(destPath, 0o755);
  }
}

function buildMacOsBleBridge({ release, profile, targetTriple }) {
  if (!targetTriple.includes("apple-darwin")) {
    return null;
  }

  const sourceFile = path.join(srcTauriDir, "macos", "BleAssistBridge.swift");
  const outputBinary = path.join(srcTauriDir, "target", profile, "dashdrop-ble-bridge");
  const destinationBinary = path.join(
    binariesDir,
    `dashdrop-ble-bridge-${targetTriple}`,
  );
  const moduleCachePath = path.join(os.tmpdir(), "dashdrop-swift-module-cache");

  const swiftArgs = [
    "swiftc",
    "-module-cache-path",
    moduleCachePath,
  ];
  if (release) {
    swiftArgs.push("-O");
  }
  swiftArgs.push(sourceFile, "-o", outputBinary);

  execFileSync("xcrun", swiftArgs, {
    cwd: repoRoot,
    stdio: "inherit",
  });

  if (!existsSync(outputBinary)) {
    throw new Error(`built BLE bridge helper was not found at ${outputBinary}`);
  }

  mkdirSync(binariesDir, { recursive: true });
  copyFileSync(outputBinary, destinationBinary);
  chmodSync(destinationBinary, 0o755);
  const aliasBinary = path.join(binariesDir, `dashdrop-ble-bridge${binaryExtensionForTarget(targetTriple)}`);
  copyAliasIfNeeded(destinationBinary, aliasBinary, binaryExtensionForTarget(targetTriple));

  return path.relative(repoRoot, destinationBinary);
}

function buildWindowsBleBridge({ release, targetTriple }) {
  if (!targetTriple.includes("windows")) {
    return null;
  }

  const extension = binaryExtensionForTarget(targetTriple);
  const sourceBinary = path.join(
    srcTauriDir,
    "target",
    release ? "release" : "debug",
    `dashdrop-ble-bridge${extension}`,
  );
  const destinationBinary = path.join(
    binariesDir,
    `dashdrop-ble-bridge-${targetTriple}${extension}`,
  );

  const cargoArgs = [
    "build",
    "--manifest-path",
    path.join("src-tauri", "Cargo.toml"),
    "--bin",
    "dashdrop-ble-bridge",
    "--features",
    "sidecar",
  ];
  if (release) {
    cargoArgs.push("--release");
  }

  execFileSync("cargo", cargoArgs, {
    cwd: repoRoot,
    stdio: "inherit",
    env: { ...process.env, DASHDROP_BUILDING_SIDECAR: "1" },
  });

  if (!existsSync(sourceBinary)) {
    throw new Error(`built Windows BLE bridge helper was not found at ${sourceBinary}`);
  }

  mkdirSync(binariesDir, { recursive: true });
  copyFileSync(sourceBinary, destinationBinary);
  const aliasBinary = path.join(binariesDir, `dashdrop-ble-bridge${extension}`);
  copyAliasIfNeeded(destinationBinary, aliasBinary, extension);

  return path.relative(repoRoot, destinationBinary);
}

function buildLinuxBleBridge({ release, targetTriple }) {
  if (!targetTriple.includes("linux")) {
    return null;
  }

  const extension = binaryExtensionForTarget(targetTriple);
  // Source is the binary name produced by cargo
  const sourceBinary = path.join(
    srcTauriDir,
    "target",
    release ? "release" : "debug",
    `dashdrop-ble-bridge-linux${extension}`,
  );
  // Destination MUST match the name in tauri.conf.json + suffix
  // Note: tauri.conf.json says "binaries/dashdrop-ble-bridge"
  const destinationBinary = path.join(
    binariesDir,
    `dashdrop-ble-bridge-${targetTriple}${extension}`,
  );

  const cargoArgs = [
    "build",
    "--manifest-path",
    path.join("src-tauri", "Cargo.toml"),
    "--bin",
    "dashdrop-ble-bridge-linux",
    "--features",
    "sidecar",
  ];
  if (release) {
    cargoArgs.push("--release");
  }

  console.log(`🔨 Building Linux BLE Bridge...`);
  execFileSync("cargo", cargoArgs, {
    cwd: repoRoot,
    stdio: "inherit",
    env: { ...process.env, DASHDROP_BUILDING_SIDECAR: "1" },
  });

  if (!existsSync(sourceBinary)) {
    throw new Error(`built Linux BLE bridge helper was not found at ${sourceBinary}`);
  }

  mkdirSync(binariesDir, { recursive: true });
  copyFileSync(sourceBinary, destinationBinary);
  if (extension === "") {
    chmodSync(destinationBinary, 0o755);
  }
  const aliasBinary = path.join(binariesDir, `dashdrop-ble-bridge${extension}`);
  copyAliasIfNeeded(destinationBinary, aliasBinary, extension);

  return path.relative(repoRoot, destinationBinary);
}

function main() {
  const release = process.argv.includes("--release");
  const profile = release ? "release" : "debug";
  
  let targetTriple = process.env.TAURI_ENV_TARGET_TRIPLE || currentTargetTriple();
  console.log(`🎯 Target Triple: ${targetTriple}`);
  
  const extension = binaryExtensionForTarget(targetTriple);
  const sourceBinary = path.join(srcTauriDir, "target", profile, `dashdropd${extension}`);
  const destinationBinary = path.join(
    binariesDir,
    `dashdropd-${targetTriple}${extension}`,
  );

  const cargoArgs = [
    "build",
    "--manifest-path",
    path.join("src-tauri", "Cargo.toml"),
    "--bin",
    "dashdropd",
    "--features",
    "sidecar",
  ];
  if (release) {
    cargoArgs.push("--release");
  }

  console.log(`🔨 Building DashDrop Daemon...`);
  execFileSync("cargo", cargoArgs, {
    cwd: repoRoot,
    stdio: "inherit",
    env: { ...process.env, DASHDROP_BUILDING_SIDECAR: "1" },
  });

  if (!existsSync(sourceBinary)) {
    throw new Error(`built daemon binary was not found at ${sourceBinary}`);
  }

  mkdirSync(binariesDir, { recursive: true });
  copyFileSync(sourceBinary, destinationBinary);
  if (extension === "") {
    chmodSync(destinationBinary, 0o755);
  }
  const aliasBinary = path.join(binariesDir, `dashdropd${extension}`);
  copyAliasIfNeeded(destinationBinary, aliasBinary, extension);

  const bleBridgeBinary = buildMacOsBleBridge({
    release,
    profile,
    targetTriple,
  }) || buildWindowsBleBridge({ release, targetTriple }) || buildLinuxBleBridge({ release, targetTriple });

  console.log(
    `Prepared dashdropd sidecar for ${targetTriple}: ${path.relative(
      repoRoot,
      destinationBinary,
    )}`,
  );
  if (bleBridgeBinary) {
    console.log(
      `Prepared BLE bridge helper for ${targetTriple}: ${bleBridgeBinary}`,
    );
  }
}

main();
