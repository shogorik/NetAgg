import { invoke } from "@tauri-apps/api/core";
import type { Connection, OptionChange, Status } from "./types";

export const getStatus = () => invoke<Status>("get_status");

/**
 * Turning on without admin rights restarts NetAgg through the UAC (or pkexec) prompt; on macOS
 * it asks for the administrator password instead.
 */
export const setEnabled = (enabled: boolean) => invoke<Status>("set_enabled", { enabled });

export const setLinkEnabled = (id: string, enabled: boolean) =>
  invoke<Status>("set_link_enabled", { id, enabled });

export const setLinkWeight = (id: string, weight: number) =>
  invoke<Status>("set_link_weight", { id, weight });

/** An empty name goes back to the automatic one. */
export const setLinkLabel = (id: string, label: string) =>
  invoke<Status>("set_link_label", { id, label });

/** `null` lets NetAgg pick: never a phone while another network is up. */
export const setDefaultLink = (id: string | null) => invoke<Status>("set_default_link", { id });

export const setOption = (change: OptionChange) => invoke<Status>("set_option", { change });

export const listConnections = () => invoke<Connection[]>("list_connections");

/** Stops the engine before an update installs where the installer won't let NetAgg exit normally. */
export const prepareUpdateInstall = () => invoke<void>("prepare_update_install");

/** Commands reject with the message string the Rust side returned. */
/** Rust errors start in lower case; the popup shows them as sentences. */
export function errorText(err: unknown): string {
  const text = typeof err === "string" ? err : err instanceof Error ? err.message : String(err);
  return text.charAt(0).toUpperCase() + text.slice(1);
}
