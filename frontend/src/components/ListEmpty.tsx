import type { ReactNode } from "react";
import { Plus } from "lucide-react";

interface Props {
  icon: ReactNode;
  title: string;
  hint: string;
  cta?: string;
  onCta?: () => void;
}

export function ListEmpty({ icon, title, hint, cta, onCta }: Props) {
  return (
    <div className="empty">
      <div className="empty-icon">{icon}</div>
      <h3>{title}</h3>
      <p>{hint}</p>
      {cta && onCta && (
        <button className="primary" onClick={onCta}>
          <Plus size={14} /> {cta}
        </button>
      )}
    </div>
  );
}
