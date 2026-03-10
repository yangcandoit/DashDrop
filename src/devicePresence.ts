import type { DeviceView, SessionView } from "./types";

function sortedSessions(device: DeviceView): SessionView[] {
  return Object.values(device.sessions || {}).sort((a, b) => b.last_seen_unix - a.last_seen_unix);
}

export function hasAnySession(device: DeviceView): boolean {
  return sortedSessions(device).length > 0;
}

export function hasUsableAddress(device: DeviceView): boolean {
  return sortedSessions(device).some(
    (session) =>
      Array.isArray(session.addrs) &&
      session.addrs.some((addr) => {
        const lower = addr.toLowerCase();
        // Scope-less link-local IPv6 addresses (fe80::) are frequently non-routable candidates.
        return !(lower.startsWith("[fe80:") && !lower.includes("%"));
      }),
  );
}

export function firstUsableAddress(device: DeviceView): string | null {
  for (const session of sortedSessions(device)) {
    if (!Array.isArray(session.addrs) || session.addrs.length === 0) {
      continue;
    }
    for (const addr of session.addrs) {
      const lower = addr.toLowerCase();
      if (!(lower.startsWith("[fe80:") && !lower.includes("%"))) {
        return addr;
      }
    }
  }
  return null;
}

export function isDeviceOnline(device: DeviceView): boolean {
  return hasUsableAddress(device) && device.reachability !== "offline";
}
