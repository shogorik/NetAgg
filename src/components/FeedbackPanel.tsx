import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { inBrowser } from "../api/transport";
import type { Status } from "../api/types";
import { basicInfo, bugReportUrl, diagnostics } from "../lib/feedback";

/** The Feedback rows in Settings: a bug report on GitHub, and what to paste into it. */
export function FeedbackPanel({ status }: { status: Status }) {
  const [reportNote, setReportNote] = useState<string | null>(null);
  const [copied, setCopied] = useState<"yes" | "failed" | null>(null);
  const [detailed, setDetailed] = useState(false);

  const report = () => {
    const url = bugReportUrl(status, inBrowser);
    setReportNote(null);
    if (inBrowser) {
      window.open(url, "_blank", "noopener");
      return;
    }
    openUrl(url).catch(() => setReportNote(`Couldn't open the browser. The form is at ${url}`));
  };

  const copy = () => {
    const text = basicInfo(status, inBrowser) + (detailed ? `\n\n${diagnostics(status)}` : "");
    navigator.clipboard.writeText(text).then(
      () => setCopied("yes"),
      () => setCopied("failed"),
    );
  };

  return (
    <>
      <div className="setting-row">
        <div className="setting-text">
          <span className="setting-label">Report a bug</span>
          <span className="setting-desc">
            {reportNote ??
              "Opens a form on GitHub with your NetAgg version and basic info filled in: settings and how many networks of each kind, no names or addresses. You need a GitHub account, and the report is public."}
          </span>
        </div>
        <button type="button" className="secondary-button" onClick={report}>
          Report
        </button>
      </div>
      <div className="setting-row stacked">
        <div className="setting-text" role="status">
          <span className="setting-label">Copy basic info</span>
          <span className="setting-desc">
            {copied === "yes"
              ? "Copied. Whatever you paste into the report is public."
              : copied === "failed"
                ? "Couldn't copy. Your browser or system didn't allow it."
                : "The same info the form starts with, to paste where you need it."}
          </span>
        </div>
        <label className="notice-check">
          <input
            type="checkbox"
            checked={detailed}
            onChange={(e) => {
              setDetailed(e.target.checked);
              setCopied(null);
            }}
          />
          Include diagnostics
        </label>
        {detailed && (
          <div className="notice feedback-warning" role="alert">
            Diagnostics add each network&apos;s name, whether it&apos;s in use and reaches the internet, its link
            speed, delay, weight and share of traffic, the response times of its router, provider, Cloudflare and
            AdGuard, your DNS servers (a DNS-over-HTTPS address can hold a private ID) and the error NetAgg shows.
            No addresses. Pasted into the report, all of it becomes public in that issue.
          </div>
        )}
        <div className="update-actions">
          <button type="button" className="secondary-button" onClick={copy}>
            {detailed ? "Copy with diagnostics" : "Copy"}
          </button>
        </div>
      </div>
    </>
  );
}
