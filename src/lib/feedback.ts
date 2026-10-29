import type { LinkKind, Status } from "../api/types";
import { diagnosisLine, plural } from "./format";

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

/** The bug report form, with the version, system and basic info filled in (.github/ISSUE_TEMPLATE/bug.yml). */
export function bugReportUrl(status: Status, inBrowser: boolean): string {
  const query = new URLSearchParams({
    template: "bug.yml",
    version: status.version,
    system: runsOn(status, inBrowser),
    basic: basicInfo(status, inBrowser),
  });
  return `${NEW_ISSUE}?${query}`;
}

const yesNo = (on: boolean) => (on ? "on" : "off");

/**
 * What every bug report carries: versions, counts and switches only. No network names, addresses, DNS
 * servers (a DNS-over-HTTPS address can hold a private ID) or error texts, since the report is public.
 */
export function basicInfo(status: Status, inBrowser: boolean): string {
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

/**
 * Added only when the user ticks "Include diagnostics", after a warning: each network by its name, how it
 * is doing, the DNS servers and the error showing. Still never an address (IP, gateway, provider hop).
 */
export function diagnostics(status: Status): string {
  const networks = status.links.map((l) => {
    const name = `- ${l.name} (${KIND[l.kind]})`;
    if (!l.present) return `${name}: not connected`;
    const shares = status.links.find((other) => other.id === l.sharesLineWith);
    return [
      `${name}: ${l.enabled ? "in use" : "switched off"}, internet ${l.health}`,
      l.speedBps ? `${Math.round(l.speedBps / 1e6)} Mbit/s link` : "",
      l.latencyMs != null ? `${l.latencyMs} ms` : "",
      `weight ${l.weight}/10, ${l.share}% of traffic, ${plural(l.connections, "connection")}`,
      l.diagnosis.checked ? diagnosisLine(l.diagnosis) : "",
      shares ? `same internet line as ${shares.name}` : "",
    ]
      .filter(Boolean)
      .join(", ");
  });
  const dns = status.settings.dns;
  return [
    "Networks:",
    ...networks,
    `DNS servers: ${dns.length > 0 ? dns.join(", ") : "none"}`,
    `Error showing: ${status.error ?? "none"}`,
  ].join("\n");
}
