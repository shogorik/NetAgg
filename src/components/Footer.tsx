import { Activity, List, Settings } from "lucide-react";
import { IconButton } from "./IconButton";

interface Props {
  info: string;
  onConnections: () => void;
  /** Left out where Settings is already on screen (a wide browser window). */
  onSettings?: () => void;
}

export function Footer({ info, onConnections, onSettings }: Props) {
  return (
    <footer className="footer">
      <span className="footer-info">
        <Activity size={14} />
        {info}
      </span>
      <div className="footer-actions">
        <IconButton icon={List} label="Connections" onClick={onConnections} />
        {onSettings && <IconButton icon={Settings} label="Settings" onClick={onSettings} />}
      </div>
    </footer>
  );
}
