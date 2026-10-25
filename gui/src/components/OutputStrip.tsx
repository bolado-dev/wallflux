import { Monitor, MonitorPause, Pause, Play } from "lucide-react";

import { describePause, type OutputState } from "@/api";
import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

interface Props {
  outputs: OutputState[];
  /** `null` means "all outputs". */
  selected: string | null;
  onSelect: (output: string | null) => void;
  onToggle: (output: string) => void;
}

/**
 * Un chip por monitor: qué reproduce, por qué no lo hace, y a qué ritmo.
 *
 * Los fps van aquí y no enterrados en los ajustes porque son la única forma
 * de ver la política de pausa trabajando: tapa el fondo con una ventana y
 * mira cómo el número cae a cero.
 */
export function OutputStrip({ outputs, selected, onSelect, onToggle }: Props) {
  return (
    <div className="flex items-center gap-2 overflow-x-auto px-5 py-3">
      {outputs.length > 1 && (
        <Chip
          selected={selected === null}
          onClick={() => onSelect(null)}
          title="Todas las salidas"
          subtitle={`${outputs.length} monitores`}
          icon={<Monitor className="size-3.5" />}
        />
      )}

      {outputs.map((output) => (
        <Chip
          key={output.name}
          selected={selected === output.name}
          onClick={() => onSelect(output.name)}
          title={output.name}
          badge={`${output.width}×${output.height}`}
          subtitle={
            output.paused
              ? describePause(output.pause_reason)
              : (output.wallpaper ?? "—")
          }
          icon={
            output.paused ? (
              <MonitorPause className="size-3.5 text-muted-foreground" />
            ) : (
              <Play className="size-3 fill-current" />
            )
          }
          meter={
            output.paused
              ? "0 fps"
              : `${Math.round(output.measured_fps)}/${output.fps_cap}`
          }
          action={
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className="size-7 rounded-lg"
                  onClick={(event) => {
                    // The chip itself selects; only the icon toggles.
                    event.stopPropagation();
                    onToggle(output.name);
                  }}
                  aria-label={output.paused ? "Reanudar" : "Pausar"}
                >
                  {output.paused ? (
                    <Play className="size-3.5" />
                  ) : (
                    <Pause className="size-3.5" />
                  )}
                </Button>
              </TooltipTrigger>
              <TooltipContent>
                {output.paused ? "Reanudar esta salida" : "Pausar esta salida"}
              </TooltipContent>
            </Tooltip>
          }
        />
      ))}
    </div>
  );
}

function Chip({
  selected,
  onClick,
  title,
  badge,
  subtitle,
  icon,
  meter,
  action,
}: {
  selected: boolean;
  onClick: () => void;
  title: string;
  badge?: string;
  subtitle: string;
  icon: React.ReactNode;
  meter?: string;
  action?: React.ReactNode;
}) {
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onClick}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onClick();
        }
      }}
      className={cn(
        "flex shrink-0 cursor-pointer items-center gap-3 rounded-xl border bg-card py-2 pr-2 pl-4",
        "transition-colors hover:border-foreground/25 hover:bg-accent",
        "focus-visible:ring-ring/50 focus-visible:ring-[3px] focus-visible:outline-none",
        selected && "border-foreground/60 bg-accent",
      )}
    >
      <div className="flex min-w-0 flex-col">
        <span className="flex items-center gap-2 text-sm leading-tight font-medium">
          {title}
          {badge && (
            <span className="font-mono text-[10px] font-normal text-muted-foreground">
              {badge}
            </span>
          )}
        </span>
        <span className="flex items-center gap-1.5 truncate text-xs text-muted-foreground">
          {icon}
          {subtitle}
        </span>
      </div>
      {meter && (
        <span className="ml-1 font-mono text-[11px] text-muted-foreground tabular-nums">
          {meter}
        </span>
      )}
      {action}
    </div>
  );
}
