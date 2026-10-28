import { useEffect, useState } from "react";
import { inBrowser } from "./api/transport";
import { updater } from "./api/updater";
import { PasswordPrompt } from "./components/PasswordPrompt";
import { ProxyNotice } from "./components/ProxyNotice";
import { useStatus } from "./api/useStatus";
import { hidePopup, useFitWindow } from "./api/window";
import { useUpdater } from "./lib/useUpdater";
import { Connections } from "./views/Connections";
import { Main } from "./views/Main";
import { Settings } from "./views/Settings";

type View = "main" | "settings" | "connections";

/** In a browser wide enough for three popup-wide columns: the main page, then Settings across two. */
const WIDE = "(min-width: 1160px)";

function useWide(): boolean {
  const [wide, setWide] = useState(() => inBrowser && window.matchMedia(WIDE).matches);
  useEffect(() => {
    if (!inBrowser) return;
    const query = window.matchMedia(WIDE);
    const onChange = () => setWide(query.matches);
    query.addEventListener("change", onChange);
    return () => query.removeEventListener("change", onChange);
  }, []);
  return wide;
}

export default function App() {
  const ref = useFitWindow<HTMLDivElement>();
  const [status, setStatus] = useStatus();
  const [view, setView] = useState<View>("main");
  const wide = useWide();
  // Settings is taller than the main page; it keeps the main page's height and scrolls instead.
  const [homeHeight, setHomeHeight] = useState<number | null>(null);
  // A server's WebUI is updated with its package, not from the page.
  const update = useUpdater(updater, status ? status.settings.checkUpdates : null);

  // Esc goes back a page, and from the first page hides the popup.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (view === "main") void hidePopup?.();
      else setView("main");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [view]);

  const back = () => setView("main");
  const openSettings = () => {
    setHomeHeight(ref.current ? Math.ceil(ref.current.getBoundingClientRect().height) : null);
    setView("settings");
  };
  // In a browser, Settings fills the window instead, minus the page's margin above and below (#root's 24px).
  const fixedHeight =
    view !== "settings"
      ? undefined
      : inBrowser
        ? { height: "calc(100dvh - 48px)" }
        : homeHeight
          ? { height: homeHeight }
          : undefined;

  // A wide browser window shows Settings beside the main page, so there's no gear to open it.
  if (wide && status) {
    return (
      <div className="web-columns">
        <PasswordPrompt />
        <div className="web-column">
          <div className="popup">
            {view === "connections" ? (
              <Connections status={status} onBack={back} />
            ) : (
              <Main
                status={status}
                onStatus={setStatus}
                onConnections={() => setView("connections")}
                update={update.state}
                proxyNotice={false}
              />
            )}
          </div>
          <ProxyNotice />
        </div>
        <div className="popup web-settings">
          <Settings status={status} onStatus={setStatus} update={update} columns />
        </div>
      </div>
    );
  }

  return (
    <div ref={ref} className="popup" style={fixedHeight}>
      {inBrowser && <PasswordPrompt />}
      {!status ? (
        <div className="empty">Loading…</div>
      ) : view === "settings" ? (
        <Settings status={status} onStatus={setStatus} onBack={back} update={update} />
      ) : view === "connections" ? (
        <Connections status={status} onBack={back} />
      ) : (
        <Main
          status={status}
          onStatus={setStatus}
          onSettings={openSettings}
          onConnections={() => setView("connections")}
          update={update.state}
        />
      )}
    </div>
  );
}
