import {
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { ChevronRight } from "lucide-react";
import { errorText, setDefaultLink, setLinkLabel, setLinkWeight, setOption } from "../api/commands";
import { inBrowser } from "../api/transport";
import type { LinkStatus, OptionChange, Status } from "../api/types";
import { hidePopup } from "../api/window";
import { SubHeader } from "../components/Header";
import { Toggle } from "../components/Toggle";
import { UpdatePanel } from "../components/UpdatePanel";
import { WebUiSettings } from "../components/WebUiSettings";
import { diagnosisLine, networkColors } from "../lib/format";
import { startAtLoginLabel } from "../lib/summary";
import type { UpdateControls } from "../lib/useUpdater";

interface Props {
  status: Status;
  onStatus: (status: Status) => void;
  /** Left out where Settings has its own columns (a wide browser window). */
  onBack?: () => void;
  update: UpdateControls;
  /** Two columns side by side: networks on the left, the app, DNS and updates on the right. */
  columns?: boolean;
}

const WEIGHT_DELAY_MS = 250;

export function Settings({ status, onStatus, onBack, update, columns = false }: Props) {
  const [error, setError] = useState<string | null>(null);
  const settings = status.settings;
  const present = status.links.filter((l) => l.present);
  const colors = networkColors(status.links);

  const change = (option: OptionChange) => {
    setError(null);
    setOption(option)
      .then(onStatus)
      .catch((e) => setError(errorText(e)));
  };

  return (
    <>
      <SubHeader title="Settings" onBack={onBack} onMinimize={hidePopup} />
      {error && (
        <div className="notice" role="alert">
          {error}
        </div>
      )}

      <div className="divider" />
      <div className={columns ? "settings-body settings-columns" : "settings-body"}>
        <div className="settings-col">
          <Section title="Balancing">
            <SwitchRow
              label="Adaptive weighting"
              desc="Faster networks get more new connections, based on what they actually deliver."
              on={settings.adaptive}
              onChange={(adaptive) => change({ adaptive })}
            />
            <SwitchRow
              label="Keep sites on one network"
              desc="Fixes sign-outs on banking sites, but one site no longer gets the combined speed."
              on={settings.sticky}
              onChange={(sticky) => change({ sticky })}
            />
            <SwitchRow
              label="Include IPv6"
              desc="Balance IPv6 traffic on networks that have it."
              on={settings.captureIpv6}
              onChange={(captureIpv6) => change({ captureIpv6 })}
            />
          </Section>

          {status.links.length > 0 && (
            <>
              <div className="divider" />
              <Section title="Networks">
                {status.platform === "windows" && (
                  <DefaultRow
                    links={status.links}
                    chosen={settings.defaultLink}
                    onChange={(id) =>
                      setDefaultLink(id)
                        .then(onStatus)
                        .catch((e) => setError(errorText(e)))
                    }
                  />
                )}
                {status.links.map((link) => (
                  <NameRow
                    key={link.id}
                    link={link}
                    onSave={(label) => setLinkLabel(link.id, label).then(onStatus)}
                  />
                ))}
              </Section>
            </>
          )}

          {present.length > 0 && (
            <>
              <div className="divider" />
              <Section title="Share of new connections">
                {present.map((link) => (
                  <WeightRow
                    key={link.id}
                    name={link.name}
                    color={colors.get(link.id) ?? "var(--text-3)"}
                    weight={link.weight}
                    onCommit={(weight) =>
                      setLinkWeight(link.id, weight)
                        .then(onStatus)
                        .catch((e) => setError(errorText(e)))
                    }
                  />
                ))}
              </Section>
            </>
          )}

        </div>
        <div className="settings-col">
          <div className="divider" />
          <Section title="App">
            {!inBrowser && (
              <SwitchRow
                label={startAtLoginLabel(status)}
                on={settings.startOnLogin}
                onChange={(startOnLogin) => change({ startOnLogin })}
              />
            )}
            <SwitchRow
              label="Hide unavailable networks"
              desc="Leaves unplugged cables and networks that aren't connected out of the list."
              on={settings.hideUnavailable}
              onChange={(hideUnavailable) => change({ hideUnavailable })}
            />
            {!inBrowser && status.platform === "linux" && <WebUiSettings status={status} />}
          </Section>

          <div className="divider" />
          <Section title="DNS">
            <SwitchRow
              label={`Use each network's own DNS (${settings.dnsMode === "auto" ? "Auto" : "Manual"})`}
              desc="Lookups go to the servers each network hands out (usually its router), through that network."
              on={settings.dnsMode === "auto"}
              onChange={(auto) => change({ dnsMode: auto ? "auto" : "manual" })}
            />
            <DnsRow dns={settings.dns} onSave={(dns) => setOption({ dns }).then(onStatus)} />
            {settings.dnsMode === "manual" && (
              <SwitchRow
                label="Block other DNS"
                desc="Every app's lookups go to your servers, even ones that name their own. Turn off only if an app breaks."
                on={settings.enforceDns}
                onChange={(enforceDns) => change({ enforceDns })}
              />
            )}
          </Section>

          <div className="divider" />
          <Section title="Updates">
            <UpdatePanel status={status} update={update} />
            <SwitchRow
              label="Check for updates at start"
              desc={
                inBrowser
                  ? "The server asks GitHub for a newer version when this page opens. It never installs anything."
                  : "Asks GitHub for a newer version when NetAgg opens. Never installs without asking."
              }
              on={settings.checkUpdates}
              onChange={(checkUpdates) => change({ checkUpdates })}
            />
          </Section>
          {inBrowser && (
            <>
              <div className="divider" />
              <Section title="WebUI">
                <div className="setting-row">
                  <div className="setting-text">
                    <span className="setting-label">Signed in</span>
                  </div>
                  <button type="button" className="secondary-button" onClick={signOut}>
                    Sign out
                  </button>
                </div>
              </Section>
            </>
          )}
        </div>
        <div className="bottom-spacer" />
      </div>
    </>
  );
}

/** Ends this browser's WebUI session. */
function signOut() {
  void fetch("/logout", { method: "POST" }).finally(() => window.location.assign("/login"));
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="settings-section" aria-label={title}>
      <span className="section-title">{title}</span>
      <div className="settings-group">{children}</div>
    </section>
  );
}

interface SwitchRowProps {
  label: string;
  desc?: string;
  on: boolean;
  onChange: (on: boolean) => void;
}

function SwitchRow({ label, desc, on, onChange }: SwitchRowProps) {
  return (
    <div className="setting-row">
      <div className="setting-text">
        <span className="setting-label">{label}</span>
        {desc && <span className="setting-desc">{desc}</span>}
      </div>
      <Toggle on={on} label={label} onChange={onChange} />
    </div>
  );
}

interface WeightRowProps {
  name: string;
  color: string;
  weight: number;
  onCommit: (weight: number) => Promise<unknown>;
}

/** A 1–10 slider that only tells the backend once the user pauses. */
function WeightRow({ name, color, weight, onCommit }: WeightRowProps) {
  const [draft, setDraft] = useState<number | null>(null);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const value = draft ?? weight;
  const slide = (next: number) => {
    setDraft(next);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      void onCommit(next).finally(() => setDraft(null));
    }, WEIGHT_DELAY_MS);
  };

  return (
    <div className="weight-row">
      <div className="weight-top">
        <span className="weight-label">
          <span className="dot" style={{ background: color }} />
          {name}
        </span>
        <span className="weight-value">{value} / 10</span>
      </div>
      <input
        type="range"
        className="slider"
        min={1}
        max={10}
        step={1}
        value={value}
        aria-label={`Share for ${name}`}
        style={{ "--fill": `${((value - 1) / 9) * 100}%` } as CSSProperties}
        onChange={(e) => slide(Number(e.target.value))}
      />
    </div>
  );
}

interface DefaultRowProps {
  links: LinkStatus[];
  chosen: string | null;
  onChange: (id: string | null) => void;
}

/** Which network Windows routes through by default, NetAgg on or off. */
function DefaultRow({ links, chosen, onChange }: DefaultRowProps) {
  // A chosen network that's unplugged stays listed, so the choice is visible.
  const options = links.filter((l) => l.present || l.id === chosen);
  return (
    <div className="setting-row">
      <div className="setting-text">
        <span className="setting-label">Default network</span>
        <select
          className="text-input"
          aria-label="Default network"
          value={chosen ?? ""}
          onChange={(e) => onChange(e.target.value || null)}
        >
          <option value="">Automatic (avoids phones while another network is up)</option>
          {options.map((l) => (
            <option key={l.id} value={l.id}>
              {l.present ? l.name : `${l.name} (not connected)`}
            </option>
          ))}
        </select>
        <span className="setting-desc">
          What Windows uses while NetAgg is off, and for traffic it doesn't carry. Applied while NetAgg has
          administrator rights.
        </span>
      </div>
    </div>
  );
}

interface NameRowProps {
  link: LinkStatus;
  onSave: (label: string) => Promise<unknown>;
}

/** One network's name, edited in place; empty goes back to the automatic name. */
function NameRow({ link, onSave }: NameRowProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  const save = () =>
    onSave(draft)
      .then(() => setEditing(false))
      .catch((e) => setError(errorText(e)));

  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") void save();
    if (e.key === "Escape") {
      // Close the editor, not the page.
      e.stopPropagation();
      setEditing(false);
    }
  };

  if (!editing) {
    return (
      <button
        type="button"
        className="setting-row"
        onClick={() => {
          setDraft(link.name);
          setError(null);
          setEditing(true);
        }}
      >
        <div className="setting-text">
          <span className="setting-label">{link.name}</span>
          <span className="setting-desc">
            {link.osName !== link.name ? `${link.osName} · ` : ""}
            {link.description || link.kind} · Rename
          </span>
          {link.present && link.diagnosis.checked && (
            <span className="setting-desc" title={link.diagnosis.ispAddress ? `Provider hop: ${link.diagnosis.ispAddress}` : undefined}>
              {diagnosisLine(link.diagnosis)}
            </span>
          )}
        </div>
        <ChevronRight size={16} />
      </button>
    );
  }

  return (
    <div className="setting-row">
      <div className="setting-text">
        <span className="setting-label">Name for {link.osName}</span>
        <input
          className="text-input"
          value={draft}
          autoFocus
          maxLength={40}
          spellCheck={false}
          aria-label={`Name for ${link.osName}`}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKey}
          onBlur={() => setEditing(false)}
        />
        <span className="setting-desc" style={error ? { color: "var(--warn)" } : undefined}>
          {error ?? "Enter saves. Leave it empty for the automatic name."}
        </span>
      </div>
    </div>
  );
}

interface DnsRowProps {
  dns: string[];
  onSave: (dns: string[]) => Promise<unknown>;
}

/** The servers for manual DNS, edited in place. */
function DnsRow({ dns, onSave }: DnsRowProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  const open = () => {
    setDraft(dns.join(", "));
    setError(null);
    setEditing(true);
  };

  const save = () => {
    const servers = draft.split(/[\s,]+/).filter(Boolean);
    if (servers.length === 0) {
      setError("Add at least one DNS server.");
      return;
    }
    onSave(servers)
      .then(() => setEditing(false))
      .catch((e) => setError(errorText(e)));
  };

  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") save();
    if (e.key === "Escape") {
      // Close the editor, not the page.
      e.stopPropagation();
      setEditing(false);
    }
  };

  if (!editing) {
    return (
      <button type="button" className="setting-row" onClick={open}>
        <div className="setting-text">
          <span className="setting-label">Your DNS servers</span>
          <span className="setting-desc">{dns.length > 0 ? dns.join(", ") : "None yet. Add some to stop using each network's own."}</span>
        </div>
        <ChevronRight size={16} />
      </button>
    );
  }

  return (
    <div className="setting-row">
      <div className="setting-text">
        <span className="setting-label">Your DNS servers</span>
        <input
          className="text-input"
          value={draft}
          autoFocus
          spellCheck={false}
          aria-label="DNS servers"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKey}
          onBlur={() => setEditing(false)}
        />
        <span className="setting-desc" style={error ? { color: "var(--warn)" } : undefined}>
          {error ??
            "9.9.9.9, tls://1.1.1.1 or https://dns.google/dns-query#8.8.8.8 (a name needs its IP after #). Separate with commas; Enter saves."}
        </span>
      </div>
    </div>
  );
}
