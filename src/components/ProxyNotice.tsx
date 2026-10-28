import { useState } from "react";
import { inBrowser, viaProxy } from "../api/transport";

const HIDDEN_KEY = "netagg.proxyNoticeHidden";

function hiddenForGood(): boolean {
  try {
    return localStorage.getItem(HIDDEN_KEY) === "1";
  } catch {
    return false;
  }
}

/**
 * The WebUI answers only local and private addresses, but behind a reverse proxy or tunnel every
 * visitor arrives from the proxy's address, so the password is all that's left.
 */
export function ProxyNotice() {
  const [hidden, setHidden] = useState(hiddenForGood);
  const [forGood, setForGood] = useState(false);
  if (!inBrowser || !viaProxy() || hidden) return null;

  const dismiss = () => {
    if (forGood) {
      try {
        localStorage.setItem(HIDDEN_KEY, "1");
      } catch {
        // Then it's dismissed until the page reloads.
      }
    }
    setHidden(true);
  };

  return (
    <div className="notice" role="alert">
      This page reaches NetAgg through a proxy or tunnel, so NetAgg can&apos;t see who is asking: anyone who
      can reach that proxy gets as far as the password. The WebUI is meant for your own network. From
      elsewhere, use Tailscale or another VPN instead; if you keep the proxy, put its own sign-in and HTTPS in
      front, and never open it to the whole internet.
      <div className="notice-actions">
        <label className="notice-check">
          <input type="checkbox" checked={forGood} onChange={(e) => setForGood(e.target.checked)} />
          Don&apos;t show again
        </label>
        <button type="button" className="link-button" onClick={dismiss}>
          Dismiss
        </button>
      </div>
    </div>
  );
}
