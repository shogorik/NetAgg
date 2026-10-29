import type { LinkKind, Status } from "../api/types";
import { plural } from "./format";

/** GitHub's new-issue page; the desktop app may open nothing else (capabilities/default.json). */
export const NEW_ISSUE = "https://github.com/shogorik/NetAgg/issues/new";

const KIND: Record<LinkKind, string> = {
  wifi: "Wi-Fi",
  ethernet: "Ethernet",
  phone: "phone",
  cellular: "cellular",
  other: "other",
};

/** Where this popup runs, as a bug report names it. */
export function runsOn(status: Status, inBrowser: boolean): string {
  const os = { windows: "Windows", linux: "Linux", macos: "macOS" }[status.platform];
  return inBrowser ? `${os} (WebUI)` : os;
}

/** The bug report form, with the version and system filled in (.github/ISSUE_TEMPLATE/bug.yml). */
export function bugReportUrl(status: Status, inBrowser: boolean): string {
  const query = new URLSearchParams({
    template: "bug.yml",
    version: status.version,
    system: runsOn(status, inBrowser),
  });
  return `${NEW_ISSUE}?${query}`;
}

const yesNo = (on: boolean) => (on ? "on" : "off");

/**
 * What a bug report may be pasted: versions, counts and switches only. No network names, addresses, DNS
 * servers (a DNS-over-HTTPS address can hold a private ID) or error texts, since the report is public.
 */
export function diagnostics(status: Status, inBrowser: boolean): string {
  const s = status.settings;
  const present = status.links.filter((l) => l.present);
  const kinds = new Map<string, number>();
  for (const link of present) kinds.set(KIND[link.kind], (kinds.get(KIND[link.kind]) ?? 0) + 1);
  const mix = [...kinds].map(([kind, n]) => `${n} ${kind}`).join(", ");
  const count = (pick: (l: (typeof present)[number]) => boolean) => present.filter(pick).length;
  const notices = [
    status.error ? "an error is showing" : "",
    status.arpFlux.length > 0 ? "ARP settings notice" : "",
    status.dropsWifi ? "Windows drops Wi-Fi notice" : "",
  ].filter(Boolean);
  return [
    `NetAgg ${status.version} on ${runsOn(status, inBrowser)}`,
    `NetAgg: ${yesNo(status.on)}, ${plural(status.connections, "connection")}, IPv6 ${status.ipv6 ? "available" : "not available"}`,
    `Networks: ${present.length} connected${mix ? ` (${mix})` : ""}, ${count((l) => l.enabled)} in use, ` +
      `${count((l) => l.health === "down")} down, ${status.links.length - present.length} not connected`,
    `Settings: adaptive ${yesNo(s.adaptive)}, one network per site ${yesNo(s.sticky)}, IPv6 ${yesNo(s.captureIpv6)}, ` +
      `DNS ${s.dnsMode}${s.dnsMode === "manual" ? ` (${plural(s.dns.length, "server")}, block other DNS ${yesNo(s.enforceDns)})` : ""}`,
    `Notices: ${notices.length > 0 ? notices.join(", ") : "none"}`,
  ].join("\n");
}
