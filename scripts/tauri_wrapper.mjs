import { execFileSync } from "node:child_process";

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
  });
}

function withMergedConfigArgs(args, extraConfig) {
  return [...args, "--config", mergeTauriConfig(extraConfig)];
}

function main() {
  const args = process.argv.slice(2);
  const primary = args[0];

  if (primary === "dev" && process.env.DASHDROP_TAURI_DEV_URL) {
    run(
      commandForCurrentPlatform("npx"),
      [
        "tauri",
        ...withMergedConfigArgs(args, {
          build: {
            devUrl: process.env.DASHDROP_TAURI_DEV_URL,
          },
        }),
      ],
    );
    return;
  }

  if (primary === "build") {
    run(commandForCurrentPlatform("npm"), ["run", "tauri:prepare-sidecar", "--", "--release"]);
    const externalBin =
      process.platform === "darwin" || process.platform === "win32"
        ? ["binaries/dashdropd", "binaries/dashdrop-ble-bridge"]
        : ["binaries/dashdropd"];
    run(
      commandForCurrentPlatform("npx"),
      [
        "tauri",
        ...withMergedConfigArgs(args, {
          bundle: {
            externalBin,
          },
        }),
      ],
    );
    return;
  }

  run(commandForCurrentPlatform("npx"), ["tauri", ...args]);
}

main();
