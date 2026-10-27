import { useEffect, useState, type FormEvent } from "react";
import { onPasswordNeeded } from "../api/transport";

/** The WebUI's stand-in for the desktop's polkit dialog: the password again, before a DNS change. */
export function PasswordPrompt() {
  const [pending, setPending] = useState<((password: string | null) => void) | null>(null);
  const [password, setPassword] = useState("");

  useEffect(() => {
    onPasswordNeeded(() => new Promise((resolve) => setPending(() => resolve)));
    return () => onPasswordNeeded(null);
  }, []);

  if (!pending) return null;

  const finish = (value: string | null) => {
    pending(value);
    setPending(null);
    setPassword("");
  };
  const submit = (e: FormEvent) => {
    e.preventDefault();
    finish(password);
  };

  return (
    <div className="password-prompt" role="dialog" aria-modal="true" aria-labelledby="password-prompt-title">
      <form className="password-prompt-card" onSubmit={submit}>
        <span id="password-prompt-title" className="setting-label">
          Confirm with your WebUI password
        </span>
        <span className="setting-desc">DNS settings decide where every lookup goes.</span>
        <input
          className="text-input"
          type="password"
          autoComplete="current-password"
          autoFocus
          aria-label="WebUI password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              // Cancel the prompt, not the page.
              e.stopPropagation();
              finish(null);
            }
          }}
        />
        <div className="password-prompt-buttons">
          <button type="button" className="link-button" onClick={() => finish(null)}>
            Cancel
          </button>
          <button type="submit" className="primary-button small" disabled={!password}>
            Confirm
          </button>
        </div>
      </form>
    </div>
  );
}
