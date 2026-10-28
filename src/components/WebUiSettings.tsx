import { useEffect, useState, type FormEvent } from "react";
import { errorText, setWebUi, webUiState, type WebUiState } from "../api/commands";
import type { Status } from "../api/types";
import { Toggle } from "./Toggle";

const MIN_PASSWORD = 8;
const PORT = 8443;

/**
 * The Linux desktop app's WebUI: off until switched on here, which asks for the administrator
 * password (and, the first time, a WebUI password for the browsers).
 */
export function WebUiSettings({ status }: { status: Status }) {
  const [state, setState] = useState<WebUiState | null>(null);
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void webUiState()
      .then(setState)
      .catch(() => setState(null));
  }, []);
  if (!state) return null;

  const apply = (on: boolean, password: string | null) => {
    setBusy(true);
    setError(null);
    setWebUi(on, password)
      .then((next) => {
        setState(next);
        setAsking(false);
      })
      .catch((e) => setError(errorText(e)))
      .finally(() => setBusy(false));
  };

  const toggle = (on: boolean) => {
    if (on && !state.hasPassword) setAsking(true);
    // Only the password form was open: nothing to switch off.
    else if (!on && !state.on) setAsking(false);
    else apply(on, null);
  };

  const addresses = status.links
    .filter((l) => l.present && l.address && !l.address.includes(":"))
    .map((l) => `https://${l.address}:${PORT}`);

  return (
    <>
      <div className="setting-row">
        <div className="setting-text">
          <span className="setting-label">Open NetAgg in a browser</span>
          <span className="setting-desc" style={error ? { color: "var(--warn)" } : undefined}>
            {error ??
              (state.on
                ? `Other devices on your network: ${addresses.join(" or ") || `this computer's address, port ${PORT}`}. Sign in with the WebUI password.`
                : "Lets other devices on your network show this popup in a browser (HTTPS, password). Local networks only.")}
          </span>
        </div>
        <Toggle on={state.on || asking} label="Open NetAgg in a browser" onChange={toggle} />
      </div>
      {asking ? (
        <PasswordForm busy={busy} onCancel={() => setAsking(false)} onSave={(p) => apply(true, p)} />
      ) : (
        state.hasPassword && (
          <button type="button" className="setting-row" disabled={busy} onClick={() => setAsking(true)}>
            <div className="setting-text">
              <span className="setting-label">Change the WebUI password</span>
              <span className="setting-desc">Signs every browser out.</span>
            </div>
          </button>
        )
      )}
    </>
  );
}

interface PasswordFormProps {
  busy: boolean;
  onCancel: () => void;
  onSave: (password: string) => void;
}

/** A new WebUI password, typed twice. Saving also switches the WebUI on. */
function PasswordForm({ busy, onCancel, onSave }: PasswordFormProps) {
  const [first, setFirst] = useState("");
  const [second, setSecond] = useState("");
  const [error, setError] = useState<string | null>(null);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (first.length < MIN_PASSWORD) setError(`Use at least ${MIN_PASSWORD} characters.`);
    else if (first !== second) setError("The two don't match.");
    else onSave(first);
  };

  return (
    <form className="setting-row" onSubmit={submit}>
      <div className="setting-text">
        <span className="setting-label">WebUI password</span>
        <input
          className="text-input"
          type="password"
          autoFocus
          autoComplete="new-password"
          aria-label="New WebUI password"
          value={first}
          onChange={(e) => setFirst(e.target.value)}
        />
        <input
          className="text-input"
          type="password"
          autoComplete="new-password"
          aria-label="The same password again"
          placeholder="Again"
          value={second}
          onChange={(e) => setSecond(e.target.value)}
        />
        <span className="setting-desc" style={error ? { color: "var(--warn)" } : undefined}>
          {error ?? "For signing in from a browser. Saving switches the WebUI on and asks for the administrator password."}
        </span>
        <div className="update-actions">
          <button type="button" className="secondary-button" onClick={onCancel} disabled={busy}>
            Cancel
          </button>
          <button type="submit" className="secondary-button" disabled={busy}>
            Save
          </button>
        </div>
      </div>
    </form>
  );
}
