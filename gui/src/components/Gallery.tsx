import { useCallback, useEffect, useRef, useState } from "react";
import {
  Check,
  Download,
  ExternalLink,
  ImageOff,
  Loader2,
  Search,
  Sparkles,
} from "lucide-react";

import { api, type GalleryItem, type GalleryView } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDownloaded: () => void;
  onError: (err: unknown) => void;
  onSuccess: (message: string) => void;
}

/** Espera antes de buscar mientras se escribe. */
const DEBOUNCE_MS = 400;

/**
 * Galería en línea: fondos de libre distribución, listos para descargar.
 *
 * El catálogo curado vive en el repositorio y no necesita cuenta ni clave,
 * así que esto funciona desde el primer arranque. Pexels se suma solo si el
 * usuario ha puesto la suya.
 */
export function Gallery({
  open,
  onOpenChange,
  onDownloaded,
  onError,
  onSuccess,
}: Props) {
  const [query, setQuery] = useState("");
  const [data, setData] = useState<GalleryView | null>(null);
  const [loading, setLoading] = useState(false);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [done, setDone] = useState<Set<string>>(new Set());
  /** Descarta respuestas de búsquedas que ya no son la última. */
  const latest = useRef(0);

  const browse = useCallback(
    async (text: string) => {
      const ticket = ++latest.current;
      setLoading(true);
      try {
        const view = await api.galleryBrowse(text);
        if (ticket === latest.current) setData(view);
      } catch (err) {
        if (ticket === latest.current) onError(err);
      } finally {
        if (ticket === latest.current) setLoading(false);
      }
    },
    [onError],
  );

  useEffect(() => {
    if (!open) return;
    const timer = window.setTimeout(() => void browse(query), DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [open, query, browse]);

  const download = async (item: GalleryItem) => {
    setDownloading(item.id);
    try {
      const id = await api.galleryDownload(item);
      setDone((current) => new Set(current).add(item.id));
      onSuccess(`«${item.title}» descargado como ${id}`);
      onDownloaded();
    } catch (err) {
      onError(err);
    } finally {
      setDownloading(null);
    }
  };

  const items = data?.items ?? [];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        // Con la variante `sm:`: la clase base del Dialog de shadcn es
        // `sm:max-w-lg` y tailwind-merge no la sustituye por un `max-w-*`
        // sin variante, así que ganaría la suya y saldría estrecho.
        className="flex h-[82vh] w-[94vw] flex-col gap-0 overflow-hidden p-0 sm:max-w-5xl"
      >
        <DialogHeader className="gap-1 px-6 pt-6 pb-4">
          <DialogTitle className="flex items-center gap-2">
            <Sparkles className="size-4" />
            Galería
          </DialogTitle>
          <DialogDescription>
            Fondos de libre distribución, listos para descargar. Sin cuenta y
            sin instalar nada.
          </DialogDescription>
        </DialogHeader>

        <div className="px-6 pb-4">
          <div className="relative">
            <Search className="absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Buscar: aurora, noche, olas…"
              className="pl-9"
            />
            {loading && (
              <Loader2 className="absolute top-1/2 right-3 size-4 -translate-y-1/2 animate-spin text-muted-foreground" />
            )}
          </div>
          {data && !data.pexels_enabled && (
            <p className="pt-2 text-xs text-muted-foreground">
              Estás viendo el catálogo curado. Para buscar además en Pexels,
              pon tu clave gratuita en{" "}
              <code className="rounded bg-muted px-1 py-0.5 font-mono">
                gallery.pexels_api_key
              </code>{" "}
              del config.toml.
            </p>
          )}
          {data?.warning && (
            <p className="pt-2 text-xs text-muted-foreground">{data.warning}</p>
          )}
        </div>
        <Separator />

        <ScrollArea className="min-h-0 flex-1">
          <div className="p-6">
            {loading && !data ? (
              <Grid>
                {Array.from({ length: 8 }, (_, index) => (
                  <div key={index} className="space-y-3">
                    <Skeleton className="aspect-video w-full rounded-xl" />
                    <Skeleton className="h-4 w-2/3 rounded-md" />
                  </div>
                ))}
              </Grid>
            ) : items.length === 0 ? (
              <Empty query={query} />
            ) : (
              <Grid>
                {items.map((item) => (
                  <GalleryCard
                    key={`${item.source}-${item.id}`}
                    item={item}
                    done={done.has(item.id)}
                    busy={downloading === item.id}
                    onDownload={() => void download(item)}
                  />
                ))}
              </Grid>
            )}
          </div>
        </ScrollArea>

        <Separator />
        <div className="flex items-center justify-between gap-3 px-6 py-4 text-xs text-muted-foreground">
          <span>
            {items.length > 0
              ? `${items.length} fondo(s). Respeta la licencia de cada uno: va escrita en su manifiesto.`
              : "El catálogo se descarga del repositorio de wallflux."}
          </span>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function Grid({ children }: { children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[repeat(auto-fill,minmax(210px,1fr))] gap-4">
      {children}
    </div>
  );
}

function GalleryCard({
  item,
  done,
  busy,
  onDownload,
}: {
  item: GalleryItem;
  done: boolean;
  busy: boolean;
  onDownload: () => void;
}) {
  const [broken, setBroken] = useState(false);
  return (
    <div className="flex flex-col overflow-hidden rounded-xl border bg-card">
      <div className="relative aspect-video w-full bg-black">
        {item.preview_url && !broken ? (
          <img
            src={item.preview_url}
            alt=""
            loading="lazy"
            onError={() => setBroken(true)}
            className="size-full object-cover"
          />
        ) : (
          <div className="hatched grid size-full place-items-center">
            <ImageOff className="size-5 text-muted-foreground" />
          </div>
        )}
        {item.width && item.height && (
          <Badge
            variant="secondary"
            className="absolute top-2 left-2 rounded-full font-mono text-[10px] backdrop-blur-sm"
          >
            {item.height}p
          </Badge>
        )}
      </div>

      <div className="flex flex-1 flex-col gap-2 p-3">
        <span className="truncate text-sm font-medium" title={item.title}>
          {item.title}
        </span>
        <span className="flex items-center gap-1.5 truncate text-xs text-muted-foreground">
          {item.license}
          {item.source_url && (
            <Tooltip>
              <TooltipTrigger asChild>
                <a
                  href={item.source_url}
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex hover:text-foreground"
                  onClick={(event) => event.stopPropagation()}
                >
                  <ExternalLink className="size-3" />
                </a>
              </TooltipTrigger>
              <TooltipContent>
                Ver la ficha original{item.author ? ` — ${item.author}` : ""}
              </TooltipContent>
            </Tooltip>
          )}
        </span>

        <Button
          size="sm"
          variant={done ? "secondary" : "default"}
          className="mt-auto w-full"
          onClick={onDownload}
          disabled={busy || done}
        >
          {busy ? (
            <Loader2 className="animate-spin" />
          ) : done ? (
            <Check />
          ) : (
            <Download />
          )}
          {busy ? "Descargando…" : done ? "Descargado" : "Descargar"}
        </Button>
      </div>
    </div>
  );
}

function Empty({ query }: { query: string }) {
  return (
    <div className="grid place-items-center gap-4 py-20 text-center">
      <div className="grid size-16 place-items-center rounded-2xl border border-dashed">
        <Search className="size-5 text-muted-foreground" />
      </div>
      <div className="space-y-1.5">
        <h3 className="text-base font-medium">
          {query ? `Nada para «${query}»` : "La galería está vacía"}
        </h3>
        <p className="mx-auto max-w-lg text-sm text-muted-foreground">
          {query
            ? "Prueba con otra palabra, o deja el campo vacío para ver el catálogo entero."
            : "No se ha podido leer el catálogo. Comprueba tu conexión; se descarga del repositorio de wallflux."}
        </p>
      </div>
    </div>
  );
}
