// Mirrors the structs in src-tauri/src/app/status.rs and commands.rs.

export type LinkKind = "wifi" | "ethernet" | "phone" | "cellular" | "other";
export type Health = "up" | "down" | "unknown";

export interface LinkStatus {
  id: string;
  name: string;
  description: string;
  kind: LinkKind;
  address: string | null;
  gateway: string | null;
  speedBps: number | null;
  latencyMs: number | null;
  health: Health;
  present: boolean;
  isDefault: boolean;
  /** What the system calls it, shown when renaming. */
  osName: string;
  /** Bytes carried for NetAgg, down and up, since it started. */
  totalBytes: number;
  enabled: boolean;
  weight: number;
  /** Bytes per second. */
  rxRate: number;
  txRate: number;
  /** Percent of the current traffic. */
  share: number;
  connections: number;
}

export interface SettingsStatus {
  adaptive: boolean;
  sticky: boolean;
  captureIpv6: boolean;
  /** auto: each network's own DNS servers; manual: `dns`. */
  dnsMode: DnsMode;
  /** The servers for manual DNS, as typed. */
  dns: string[];
  /** With manual DNS, answer every app's lookups with `dns` and block the rest. */
  enforceDns: boolean;
  startOnLogin: boolean;
  /** Look for a new version when NetAgg starts (never installs on its own). */
  checkUpdates: boolean;
  /** Leave unplugged and unconnected networks out of the list. */
  hideUnavailable: boolean;
  /** The network Windows routes through by default; null is automatic. */
  defaultLink: string | null;
}

export type DnsMode = "auto" | "manual";

export type Platform = "windows" | "linux" | "macos";

export interface Status {
  platform: Platform;
  /** The running version, such as "1.2.0". */
  version: string;
  on: boolean;
  busy: boolean;
  elevated: boolean;
  canElevate: boolean;
  error: string | null;
  ipv6: boolean;
  rxRate: number;
  txRate: number;
  connections: number;
  todayBytes: number;
  links: LinkStatus[];
  settings: SettingsStatus;
  /** Windows drops Wi-Fi while a wired-looking network (a USB-tethered phone) is up. */
  dropsWifi: boolean;
}

export interface Connection {
  id: number;
  proto: "tcp" | "udp";
  app: string | null;
  host: string | null;
  destination: string;
  linkId: string | null;
  linkName: string | null;
  rx: number;
  tx: number;
  rxRate: number;
  txRate: number;
  seconds: number;
}

/** Settings a change can carry; fields left out stay as they are. */
export interface OptionChange {
  adaptive?: boolean;
  sticky?: boolean;
  captureIpv6?: boolean;
  dnsMode?: DnsMode;
  dns?: string[];
  enforceDns?: boolean;
  startOnLogin?: boolean;
  checkUpdates?: boolean;
  hideUnavailable?: boolean;
}
