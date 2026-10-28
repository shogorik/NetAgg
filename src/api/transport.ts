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

/** `control::NEEDS_ADMIN`: the WebUI wants the password typed again before a DNS change. */
const NEEDS_ADMIN = "This change needs administrator approval.";

/** Asks for the WebUI password; `null` when cancelled. Set by `PasswordPrompt`. */
let askPassword: (() => Promise<string | null>) | null = null;

export function onPasswordNeeded(ask: typeof askPassword) {
  askPassword = ask;
}

let proxied = false;

/** Whether the WebUI said a reverse proxy or tunnel passed this page's requests on. */
export const viaProxy = () => proxied;

async function post(path: string, body: unknown): Promise<Response> {
  const response = await fetch(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (response.headers.get("x-netagg-proxied")) proxied = true;
  if (response.status === 401) {
    window.location.assign("/login");
    throw "Signed out";
  }
  return response;
}

async function send<T>(body: Args): Promise<T> {
  const response = await post("/api", body);
  if (!response.ok) throw await response.text();
  const reply = (await response.json()) as { Ok: T } | { Err: string };
  if ("Err" in reply) throw reply.Err;
  return reply.Ok;
}

/** One command; rejects with the message string the Rust side returned, as `invoke` does. */
export async function call<T>(command: string, args: Args = {}): Promise<T> {
  if (!inBrowser) return invoke<T>(command, args);
  const request = requests[command];
  if (!request) throw `${command} isn't available in the WebUI`;
  try {
    return await send<T>(request(args));
  } catch (err) {
    if (err !== NEEDS_ADMIN || !askPassword) throw err;
  }
  const password = await askPassword();
  if (password === null) throw "The change needs administrator approval, which wasn't given.";
  const approval = await post("/approve", { password });
  if (!approval.ok) throw await approval.text();
  return send<T>(request(args));
}
