import { invoke } from "@tauri-apps/api/core";

/**
 * The popup runs in two places: the desktop app's window (Tauri), and a browser, served by
 * netagg-server's WebUI on a headless Linux server.
 */
export const inBrowser = typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);

type Args = Record<string, unknown>;

/** The WebUI forwards the service's own requests (`control::Request`); the app's commands map to them. */
const requests: Record<string, (args: Args) => Args> = {
  get_status: () => ({ cmd: "status" }),
  list_connections: () => ({ cmd: "connections" }),
  set_enabled: (a) => ({ cmd: "setEnabled", on: a.enabled, remember: true }),
  set_link_enabled: (a) => ({ cmd: "setLinkEnabled", id: a.id, enabled: a.enabled }),
  set_link_weight: (a) => ({ cmd: "setLinkWeight", id: a.id, weight: a.weight }),
  set_link_label: (a) => ({ cmd: "setLinkLabel", id: a.id, label: a.label }),
  set_default_link: (a) => ({ cmd: "setDefaultLink", id: a.id }),
  set_option: (a) => ({ cmd: "setOption", change: a.change }),
  allow_simultaneous_connections: () => ({ cmd: "allowSimultaneousConnections" }),
};

/** One command; rejects with the message string the Rust side returned, as `invoke` does. */
export async function call<T>(command: string, args: Args = {}): Promise<T> {
  if (!inBrowser) return invoke<T>(command, args);
  const request = requests[command];
  if (!request) throw `${command} isn't available in the WebUI`;
  const response = await fetch("/api", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(request(args)),
  });
  if (response.status === 401) {
    window.location.assign("/login");
    throw "Signed out";
  }
  if (!response.ok) throw await response.text();
  const reply = (await response.json()) as { Ok: T } | { Err: string };
  if ("Err" in reply) throw reply.Err;
  return reply.Ok;
}
