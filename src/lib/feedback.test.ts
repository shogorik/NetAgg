import { describe, expect, it } from "vitest";
import type { LinkStatus, Status } from "../api/types";
import { basicInfo, bugReportUrl, diagnostics } from "./feedback";

const link = (over: Partial<LinkStatus>): LinkStatus => ({
  id: "1",
  name: "Home Wi-Fi 5G",
  description: "Intel Wi-Fi 6 AX201",
  kind: "wifi",
  address: "192.168.1.40",
  gateway: "192.168.1.1",
  speedBps: null,
  latencyMs: 18,
  health: "up",
  present: true,
  isDefault: true,
  osName: "Wi-Fi",
  totalBytes: 0,
  sharesLineWith: null,
  diagnosis: { router: 2, isp: 9, ispAddress: "203.0.113.7", cloudflare: 20, adguard: 30, checked: true },
  enabled: true,
  weight: 5,
  rxRate: 0,
  txRate: 0,
  share: 0,
  connections: 0,
  ...over,
});

const status: Status = {
  platform: "linux",
  version: "2.0.4",
  on: true,
  busy: false,
  elevated: false,
  canElevate: true,
  error: "Couldn't reach https://dns.example/dns-query/private-id",
  ipv6: false,
  rxRate: 0,
  txRate: 0,
  connections: 12,
  todayBytes: 0,
  links: [
    link({}),
    link({ id: "2", name: "Desk cable", kind: "ethernet", address: "192.168.1.41", health: "down" }),
    link({ id: "3", name: "Old phone", kind: "phone", present: false, enabled: false }),
  ],
  settings: {
    adaptive: true,
    sticky: false,
    captureIpv6: true,
    dnsMode: "manual",
    dns: ["https://dns.example/dns-query/private-id#198.51.100.53"],
    enforceDns: true,
    startOnLogin: false,
    checkUpdates: true,
    hideUnavailable: false,
    defaultLink: null,
  },
  dropsWifi: false,
  arpFlux: ["1", "2"],
};

describe("bug report", () => {
  it("fills in the version, system and basic info", () => {
    const url = new URL(bugReportUrl(status, true));
    expect(url.origin + url.pathname).toBe("https://github.com/shogorik/NetAgg/issues/new");
    expect(url.searchParams.get("template")).toBe("bug.yml");
    expect(url.searchParams.get("version")).toBe("2.0.4");
    expect(url.searchParams.get("system")).toBe("Linux (WebUI)");
    expect(url.searchParams.get("basic")).toBe(basicInfo(status, true));
  });

  it("describes the setup in counts and switches", () => {
    expect(basicInfo(status, false)).toBe(
      [
        "NetAgg 2.0.4 on Linux",
        "NetAgg: on, 12 connections, IPv6 not available",
        "Networks: 2 connected (1 Wi-Fi, 1 Ethernet), 2 in use, 1 down, 1 not connected",
        "Settings: adaptive on, one network per site off, IPv6 on, DNS manual (1 server, block other DNS on)",
        "Notices: an error is showing, ARP settings notice",
      ].join("\n"),
    );
  });

  it("never carries names, addresses, DNS servers or error texts by default", () => {
    const url = decodeURIComponent(bugReportUrl(status, true));
    for (const text of [basicInfo(status, true), url]) {
      for (const secret of ["Home Wi-Fi", "Desk cable", "Old phone", "Intel", "192.168", "203.0.113", "198.51.100", "dns.example", "private-id"]) {
        expect(text).not.toContain(secret);
      }
    }
  });

  it("adds names, health, DNS servers and the error only with diagnostics, never addresses", () => {
    const text = diagnostics(status);
    expect(text).toBe(
      [
        "Networks:",
        "- Home Wi-Fi 5G (Wi-Fi): in use, internet up, 18 ms, weight 5/10, 0% of traffic, 0 connections, Router 2 ms · ISP 9 ms · Cloudflare 20 ms · AdGuard 30 ms",
        "- Desk cable (Ethernet): in use, internet down, 18 ms, weight 5/10, 0% of traffic, 0 connections, Router 2 ms · ISP 9 ms · Cloudflare 20 ms · AdGuard 30 ms",
        "- Old phone (phone): not connected",
        "DNS servers: https://dns.example/dns-query/private-id#198.51.100.53",
        "Error showing: Couldn't reach https://dns.example/dns-query/private-id",
      ].join("\n"),
    );
    for (const address of ["192.168", "203.0.113"]) expect(text).not.toContain(address);
  });
});
