import { call } from "./transport";
import type { Connection, OptionChange, Status } from "./types";

export const getStatus = () => call<Status>("get_status");

/**
 * Turning on without admin rights restarts NetAgg through the UAC (or pkexec) prompt; on macOS
 * it asks for the administrator password instead.
 */
export const setEnabled = (enabled: boolean) => call<Status>("set_enabled", { enabled });

export const setLinkEnabled = (id: string, enabled: boolean) =>
  call<Status>("set_link_enabled", { id, enabled });

export const setLinkWeight = (id: string, weight: number) =>
  call<Status>("set_link_weight", { id, weight });

/** An empty name goes back to the automatic one. */
export const setLinkLabel = (id: string, label: string) =>
  call<Status>("set_link_label", { id, label });

/** Lets Windows keep Wi-Fi next to a tethered phone; applies at the next sign-in. */
export const allowSimultaneousConnections = () => call<Status>("allow_simultaneous_connections");

/** `null` lets NetAgg pick: never a phone while another network is up. */
export const setDefaultLink = (id: string | null) => call<Status>("set_default_link", { id });

export const setOption = (change: OptionChange) => call<Status>("set_option", { change });

export const listConnections = () => call<Connection[]>("list_connections");

/** Commands reject with the message string the Rust side returned. */
/** Rust errors start in lower case; the popup shows them as sentences. */
export function errorText(err: unknown): string {
  const text = typeof err === "string" ? err : err instanceof Error ? err.message : String(err);
  return text.charAt(0).toUpperCase() + text.slice(1);
}
