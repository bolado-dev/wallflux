import { useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Check, ImageOff, Loader2 } from "lucide-react";

import { api, type Wallpaper } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

interface Props {
  wallpaper: Wallpaper;
  /** Output names currently showing this wallpaper. */
  activeOn: string[];
  onApply: (id: string) => void;
}

/**
 * A card in the library grid.
 *
 * Hovering plays the actual video rather than a canned preview — the one
 * interaction that makes a wallpaper picker feel like it knows what it is
 * showing you. The `<video>` only mounts on hover, so a library of fifty clips
 * does not open fifty decoders.
 */
export function WallpaperCard({ wallpaper, activeOn, onApply }: Props) {
  const [thumb, setThumb] = useState<string | null>(null);
  const [thumbError, setThumbError] = useState<string | null>(null);
  const [hovered, setHovered] = useState(false);
  const [videoReady, setVideoReady] = useState(false);
  const video = useRef<HTMLVideoElement>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .thumbnail(wallpaper.id)
      .then((path) => {
        if (!cancelled) setThumb(convertFileSrc(path));
      })
      .catch((err: unknown) => {
        // No ffmpeg, or a file it cannot read. A hatched placeholder beats an
        // error toast per card, but the reason belongs on the card rather
        // than nowhere: a silent "no preview" is impossible to debug.
        if (!cancelled) setThumbError(typeof err === "string" ? err : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [wallpaper.id]);

  useEffect(() => {
    if (!hovered) setVideoReady(false);
  }, [hovered]);

  const active = activeOn.length > 0;

  return (
    <Card
      // A div rather than a <button>: the card contains a tooltip trigger,
      // and nesting interactive elements inside a button is invalid HTML.
      role="button"
      tabIndex={0}
      aria-label={`Aplicar ${wallpaper.title}`}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      onFocus={() => setHovered(true)}
      onBlur={() => setHovered(false)}
      onClick={() => onApply(wallpaper.id)}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onApply(wallpaper.id);
        }
      }}
      className={cn(
        "group cursor-pointer gap-0 overflow-hidden rounded-xl p-0 transition-all duration-200",
        "hover:-translate-y-0.5 hover:border-foreground/25",
        "focus-visible:ring-ring/50 focus-visible:ring-[3px] focus-visible:outline-none",
        active && "border-transparent ring-2 ring-foreground",
      )}
    >
        <div className="relative aspect-video w-full overflow-hidden bg-black">
          {thumb ? (
            <img
              src={thumb}
              alt=""
              loading="lazy"
              className="size-full object-cover"
            />
          ) : (
            <Placeholder error={thumbError} />
          )}

          {hovered && (
            <video
              ref={video}
              src={convertFileSrc(wallpaper.source)}
              data-ready={videoReady}
              muted
              loop
              autoPlay
              playsInline
              preload="none"
              onCanPlay={() => setVideoReady(true)}
              className="preview-video absolute inset-0 size-full object-cover"
            />
          )}

          {active && (
            <Badge className="absolute top-3 left-3 gap-1.5 rounded-full px-2.5 backdrop-blur-sm">
              <Check className="size-3" />
              {activeOn.length === 1 ? activeOn[0] : `${activeOn.length} salidas`}
            </Badge>
          )}
        </div>

        <div className="flex w-full flex-col items-start gap-1 px-4 py-3.5 text-left">
          <span className="w-full truncate font-medium">{wallpaper.title}</span>
          <span className="w-full truncate text-xs text-muted-foreground">
            {wallpaper.author ?? (wallpaper.implicit ? "sin manifiesto" : "—")}
          </span>
          {wallpaper.tags.length > 0 && (
            <div className="mt-1.5 flex flex-wrap gap-1.5">
              {wallpaper.tags.slice(0, 3).map((tag) => (
                <Badge key={tag} variant="outline" className="rounded-full font-normal">
                  {tag}
                </Badge>
              ))}
            </div>
          )}
        </div>
    </Card>
  );
}

/** Shown until a thumbnail arrives, or instead of one that cannot be made. */
function Placeholder({ error }: { error: string | null }) {
  if (!error) {
    return (
      <div className="hatched grid size-full place-items-center">
        <Loader2 className="size-5 animate-spin text-muted-foreground" />
      </div>
    );
  }
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <div className="hatched grid size-full place-items-center gap-1.5 text-muted-foreground">
          <ImageOff className="size-5 justify-self-center" />
          <span className="px-4 text-center text-[11px]">sin vista previa</span>
        </div>
      </TooltipTrigger>
      <TooltipContent className="max-w-sm">{error}</TooltipContent>
    </Tooltip>
  );
}
