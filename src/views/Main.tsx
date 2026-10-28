import { useState } from "react";
import { CircleArrowUp, Gauge, ShieldCheck } from "lucide-react";
import { allowSimultaneousConnections, errorText, setEnabled, setLinkEnabled } from "../api/commands";
import type { Status } from "../api/types";
import { hidePopup } from "../api/window";
import { Footer } from "../components/Footer";
import { Header } from "../components/Header";
import { NetworkRow } from "../components/NetworkRow";
import { ProxyNotice } from "../components/ProxyNotice";
import { ShareRing } from "../components/ShareRing";
import { networkColors } from "../lib/format";
import { elevationNote, footerInfo, headerStatus, inUse, type Pending } from "../lib/summary";
import type { UpdateState } from "../lib/update";

/** Makes each adapter answer ARP only for its own address, and name it in its own requests. */
const ARP_FIX =
  "printf 'net.ipv4.conf.all.arp_ignore = 1\\nnet.ipv4.conf.all.arp_announce = 2\\n' | sudo tee /etc/sysctl.d/60-netagg-arp.conf && sudo sysctl --system";

interface Props {
  status: Status;
  onStatus: (status: Status) => void;
  /** Left out where Settings is already on screen (a wide browser window): no gear then. */
  onSettings?: () => void;
  onConnections: () => void;
  update: UpdateState;
  /** Off where the page shows the proxy warning below the card instead. */
  proxyNotice?: boolean;
}

/** The popup's first page: live traffic when on, an explanation when off. */
export function Main({ status, onStatus, onSettings, onConnections, update, proxyNotice = true }: Props) {
  const [pending, setPending] = useState<Pending>(null);
  const [error, setError] = useState<string | null>(null);
  const [signInNeeded, setSignInNeeded] = useState(false);

  const allowBoth = () => {
    setError(null);
    allowSimultaneousConnections()
      .then((s) => {
        onStatus(s);
        setSignInNeeded(true);
      })
      .catch((e) => setError(errorText(e)));
  };

  const turn = (on: boolean) => {
    setPending(on ? "on" : "off");
    setError(null);
    // Without admin rights this restarts NetAgg through the UAC (or pkexec) prompt; macOS asks
    // for the administrator password instead.
    setEnabled(on)
      .then(onStatus)
      .catch((e) => setError(errorText(e)))
      .finally(() => setPending(null));
  };

  const chooseLink = (id: string, enabled: boolean) => {
    setError(null);
    setLinkEnabled(id, enabled)
      .then(onStatus)
      .catch((e) => setError(errorText(e)));
  };

  const header = headerStatus(status, pending);
  const busy = pending !== null || status.busy;
  const notice = error ?? (status.error && errorText(status.error));
  const colors = networkColors(status.links);
  const shown = status.settings.hideUnavailable
    ? status.links.filter((l) => l.present)
    : status.links;

  return (
    <>
      <Header
        on={status.on}
        status={header.text}
        dot={header.dot}
        disabled={busy}
        onToggle={turn}
        onMinimize={hidePopup}
      />
      {notice && (
        <div className="notice" role="alert">
          {notice}
        </div>
      )}
      {proxyNotice && <ProxyNotice />}
      {status.dropsWifi && (
        <div className="notice" role="alert">
          Windows drops Wi-Fi while the tethered phone is connected, because it treats the phone as a cable
          connection.{" "}
          <button type="button" className="link-button" onClick={allowBoth}>
            Keep both connected
          </button>
        </div>
      )}
      {status.arpFlux.length > 0 && (
        <div className="notice" role="alert">
          {status.arpFlux.join(" and ")} are on the same router, and with Linux&apos;s default ARP settings
          replies for one can arrive on the other, so NetAgg sees a network dropping out. To fix it, run this
          once, then reconnect both networks:
          <code className="notice-command">{ARP_FIX}</code>
          <button type="button" className="link-button" onClick={() => void navigator.clipboard?.writeText(ARP_FIX)}>
            Copy
          </button>
        </div>
      )}
      {status.links
        .filter((l) => l.present && l.sharesLineWith)
        .map((l) => {
          const other = status.links.find((o) => o.id === l.sharesLineWith);
          return (
            <div key={l.id} className="notice" role="status">
              {l.name} reaches the internet through the same router as {other?.name ?? "another network"}, so
              both share one line: it adds speed only if that line is faster than {other?.name ?? "it"} alone.
              {l.kind === "phone" ? " Turn off Wi-Fi on the phone to use its mobile data instead." : ""}
            </div>
          );
        })}
      {signInNeeded && (
        <div className="notice" role="status">
          Done. Sign out and back in (or restart) for Windows to keep Wi-Fi connected.
        </div>
      )}
      {update.step === "available" && (
        <div className="update-banner" role="status">
          <CircleArrowUp size={14} />
          <span className="update-banner-text">NetAgg {update.version} is available.</span>
          <button type="button" className="link-button" onClick={onSettings}>
            Review
          </button>
        </div>
      )}

      {status.on ? (
        <section className="throughput" aria-label="Throughput">
          <ShareRing links={inUse(status.links)} colors={colors} rxRate={status.rxRate} txRate={status.txRate} />
        </section>
      ) : (
        <section className="off-callout">
          <div className="callout">
            <span className="callout-title">Add your networks together</span>
            <p className="callout-body">
              Each new connection goes out on whichever network has the most room. Apps that open
              many connections, like browsers, Steam and torrents, get the combined speed.
            </p>
            <button
              type="button"
              className="primary-button"
              disabled={busy}
              onClick={() => turn(true)}
            >
              <ShieldCheck size={14} />
              Turn on
            </button>
            {!status.elevated && (
              <span className="muted">{elevationNote(status)}</span>
            )}
          </div>
        </section>
      )}

      <div className="divider" />
      <section className="networks" aria-label="Networks">
        <div className="section-head">
          <span className="section-title">Networks</span>
          {status.settings.adaptive && (
            <button type="button" className="chip" onClick={onSettings}>
              <Gauge size={12} />
              Adaptive
            </button>
          )}
        </div>
        {shown.map((link) => (
          <NetworkRow
            key={link.id}
            link={link}
            color={colors.get(link.id) ?? "var(--text-3)"}
            live={status.on}
            onEnabled={(enabled) => chooseLink(link.id, enabled)}
          />
        ))}
        {shown.length === 0 && (
          <div className="empty">
            {status.links.length === 0 ? "No networks found." : "No networks are connected."}
          </div>
        )}
      </section>

      <Footer info={footerInfo(status)} onConnections={onConnections} onSettings={onSettings} />
    </>
  );
}
