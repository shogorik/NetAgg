import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { inBrowser } from "../api/transport";
import type { Status } from "../api/types";
import { bugReportUrl, diagnostics } from "../lib/feedback";

/** The Feedback rows in Settings: a bug report on GitHub, and diagnostics to paste into it. */
export function FeedbackPanel({ status }: { status: Status }) {
  const [reportNote, setReportNote] = useState<string | null>(null);
  const [copied, setCopied] = useState<"yes" | "failed" | null>(null);

  const report = () => {
    const url = bugReportUrl(status, inBrowser);
    setReportNote(null);
    if (inBrowser) {
      window.open(url, "_blank", "noopener");
      return;
    }
    openUrl(url).catch(() => setReportNote(`Couldn't open the browser. The form is at ${url}`));
  };

  const copy = () =>
    navigator.clipboard.writeText(diagnostics(status, inBrowser)).then(
      () => setCopied("yes"),
      () => setCopied("failed"),
    );

  return (
    <>
      <div className="setting-row">
        <div className="setting-text">
          <span className="setting-label">Report a bug</span>
          <span className="setting-desc">
            {reportNote ??
              "Opens a form on GitHub with your NetAgg version filled in. You need a GitHub account, and the report is public."}
          </span>
        </div>
        <button type="button" className="secondary-button" onClick={report}>
          Report
        </button>
      </div>
      <div className="setting-row">
        <div className="setting-text" role="status">
          <span className="setting-label">Copy diagnostics</span>
          <span className="setting-desc">
            {copied === "yes"
              ? "Copied. Paste it into the report if it helps; whatever you paste there is public."
              : copied === "failed"
                ? "Couldn't copy. Your browser or system didn't allow it."
                : "Optional, for a report: NetAgg's version, settings and how many networks of each kind. No names, addresses or DNS servers, but the report is public."}
          </span>
        </div>
        <button type="button" className="secondary-button" onClick={() => void copy()}>
          Copy
        </button>
      </div>
    </>
  );
}
