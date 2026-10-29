import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { inBrowser } from "../api/transport";
import type { Status } from "../api/types";
import { bugReportUrl } from "../lib/feedback";

/** The Feedback row in Settings: a bug report on GitHub, filled in with basic info or, if asked, diagnostics. */
export function FeedbackPanel({ status }: { status: Status }) {
  const [note, setNote] = useState<string | null>(null);
  const [detailed, setDetailed] = useState(false);

  const report = () => {
    const url = bugReportUrl(status, inBrowser, detailed);
    setNote(null);
    if (inBrowser) {
      window.open(url, "_blank", "noopener");
      return;
    }
    openUrl(url).catch(() => setNote(`Couldn't open the browser. The form is at ${url}`));
  };

  return (
    <div className="setting-row stacked">
      <div className="setting-text">
        <span className="setting-label">Report a bug</span>
        <span className="setting-desc">
          {note ??
            "Opens a form on GitHub with your NetAgg version and basic info filled in: settings and how many networks of each kind, no names or addresses. You need a GitHub account, and the report is public."}
        </span>
      </div>
      <div className="notice-actions">
        <label className="notice-check">
          <input type="checkbox" checked={detailed} onChange={(e) => setDetailed(e.target.checked)} />
          Include diagnostics
        </label>
        <button type="button" className="secondary-button" onClick={report}>
          Report
        </button>
      </div>
      {detailed && (
        <div className="notice feedback-warning" role="alert">
          The form also gets each network&apos;s name, whether it&apos;s in use and reaches the internet, its link
          speed, delay, weight and share of traffic, the response times of its router, provider, Cloudflare and
          AdGuard, your DNS servers (a DNS-over-HTTPS address can hold a private ID) and the error NetAgg shows. No
          addresses. Once you submit the report, all of it is public in that issue.
        </div>
      )}
    </div>
  );
}
