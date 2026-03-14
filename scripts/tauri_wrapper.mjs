import { execFileSync } from "node:child_process";
import { writeFileSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

function commandForCurrentPlatform(base) {
  return process.platform === "win32" ? `${base}.cmd` : base;
}

function mergeTauriConfig(extraConfig) {
  const existing = process.env.TAURI_CONFIG
    ? JSON.parse(process.env.TAURI_CONFIG)
    : {};

  return JSON.stringify({
    ...existing,
    build: {
      ...(existing.build ?? {}),
      ...(extraConfig.build ?? {}),
    },
    app: {
      ...(existing.app ?? {}),
      ...(extraConfig.app ?? {}),
    },
    bundle: {
      ...(existing.bundle ?? {}),
      ...(extraConfig.bundle ?? {}),
    },
  });
}

function run(command, args, env = process.env) {
  execFileSync(command, args, {
    stdio: "inherit",
    env,
    shell: process.platform === "win32",
  });
}

// Returns the --config argument value and a cleanup function.
// On Windows, cmd.exe mangles inline JSON through shell quoting, so we write
// the config to a temp file and pass its path instead (Tauri accepts both).
function resolveConfigArg(extraConfig) {
  const json = mergeTauriConfig(extraConfig);
  if (process.platform !== "win32") {
    return { configArg: json, cleanup: () => {} };
  }
  const tmpPath = join(tmpdir(), `tauri-config-${Date.now()}.json`);
  writeFileSync(tmpPath, json, "utf8");
  return {
    configArg: tmpPath,
    cleanup: () => { try { unlinkSync(tmpPath); } catch {} },
  };
}

function main() {
  const args = process.argv.slice(2);
  const primary = args[0];

  if (primary === "dev" && process.env.DASHDROP_TAURI_DEV_URL) {
    const { configArg, cleanup } = resolveConfigArg({
      build: { devUrl: process.env.DASHDROP_TAURI_DEV_URL },
    });
    try {
      run(commandForCurrentPlatform("npx"), ["tauri", ...args, "--config", configArg]);
    } finally {
      cleanup();
    }
    return;
  }

  if (primary === "build") {
    run(commandForCurrentPlatform("npm"), ["run", "tauri:prepare-sidecar", "--", "--release"]);
    const externalBin =
      process.platform === "darwin" || process.platform === "win32"
        ? ["binaries/dashdropd", "binaries/dashdrop-ble-bridge"]
        : ["binaries/dashdropd"];
    const { configArg, cleanup } = resolveConfigArg({ bundle: { externalBin } });
    try {
      run(commandForCurrentPlatform("npx"), ["tauri", ...args, "--config", configArg]);
    } finally {
      cleanup();
    }
    return;
  }

  run(commandForCurrentPlatform("npx"), ["tauri", ...args]);
}

main();
