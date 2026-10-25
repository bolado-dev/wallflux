import { useCallback, useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  Check,
  Download,
  ExternalLink,
  ImageOff,
  Loader2,
  Search,
  Store,
} from "lucide-react";

import {
  api,
  describeWorkshopKind,
  humanSize,
  workshopMayWork,
  type WorkshopItem,
  type WorkshopView,
} from "@/api";
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
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onImported: () => void;
  onError: (err: unknown) => void;
  onSuccess: (message: string) => void;
}

const DEBOUNCE_MS = 500;

const SORTS = [
  { value: "trend", label: "Tendencia" },
  { value: "recent", label: "Recientes" },
  { value: "subscribed", label: "Populares" },
] as const;

/**
 * Explorador del taller de Wallpaper Engine en Steam.
 *
 * El listado es público: títulos, miniaturas, etiquetas y tamaños salen de
 * la API abierta de Steam, sin clave y sin cuenta. Descargar es otra cosa —
 * Steam no publica URL directa de este contenido — y para eso hace falta
 * steamcmd y una cuenta que posea la aplicación. La interfaz dice cuál de
 * las dos cosas falta en vez de limitarse a deshabilitar un botón.
 */
export function Workshop({
  open,
  onOpenChange,
  onImported,
  onError,
  onSuccess,
}: Props) {
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<string>("trend");
  const [data, setData] = useState<WorkshopView | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [done, setDone] = useState<Set<string>>(new Set());
  const [steamUser, setSteamUser] = useState("");
  const latest = useRef(0);

  const browse = useCallback(
    async (text: string, order: string) => {
      const ticket = ++latest.current;
      setLoading(true);
      try {
        const view = await api.workshopBrowse(text, order, 1);
        if (ticket !== latest.current) return;
        setData(view);
        // Solo la primera vez: no pisar lo que esté escribiendo el usuario.
        setSteamUser((current) => current || view.steam_user);
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
    const timer = window.setTimeout(() => void browse(query, sort), DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [open, query, sort, browse]);

  const importItem = async (item: WorkshopItem) => {
    setBusy(item.id);
    try {
      const id = await api.workshopImport(item.id, steamUser.trim());
      setDone((current) => new Set(current).add(item.id));
      onSuccess(`«${item.title}» importado como ${id}`);
      onImported();
    } catch (err) {
      onError(err);
    } finally {
      setBusy(null);
    }
  };

  const items = data?.items ?? [];
  const importable = items.filter((item) => workshopMayWork(item.kind)).length;
  const ready = Boolean(data?.steamcmd) && steamUser.trim().length > 0;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex h-[82vh] w-[94vw] flex-col gap-0 overflow-hidden p-0 sm:max-w-5xl">
        <DialogHeader className="gap-1 px-6 pt-6 pb-4">
          <DialogTitle className="flex items-center gap-2">
            <Store className="size-4" />
            Taller de Wallpaper Engine
          </DialogTitle>
          <DialogDescription>
            {items.length > 0
              ? `${importable} de ${items.length} se pueden traer. Descargar usa tu propia cuenta de Steam.`
              : "El catálogo del taller de Steam. Explorar es libre; descargar requiere tener la aplicación."}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3 px-6 pb-4">
          <div className="flex gap-2">
            <div className="relative flex-1">
              <Search className="absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="Buscar en el taller…"
                className="pl-9"
              />
              {loading && (
                <Loader2 className="absolute top-1/2 right-3 size-4 -translate-y-1/2 animate-spin text-muted-foreground" />
              )}
            </div>
            <ToggleGroup
              type="single"
              value={sort}
              onValueChange={(next) => next && setSort(next)}
              variant="outline"
              className="rounded-lg"
            >
              {SORTS.map((option) => (
                <ToggleGroupItem
                  key={option.value}
                  value={option.value}
                  className="text-xs data-[state=on]:bg-foreground data-[state=on]:text-background"
                >
                  {option.label}
                </ToggleGroupItem>
              ))}
            </ToggleGroup>
          </div>

          <Requirements
            view={data}
            steamUser={steamUser}
            onSteamUser={setSteamUser}
          />
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
              <div className="grid place-items-center gap-3 py-20 text-center">
                <div className="grid size-16 place-items-center rounded-2xl border border-dashed">
                  <Search className="size-5 text-muted-foreground" />
                </div>
                <p className="text-sm text-muted-foreground">
                  {query
                    ? `El taller no ha devuelto nada para «${query}».`
                    : "No se ha podido leer el taller. Comprueba tu conexión."}
                </p>
              </div>
            ) : (
              <Grid>
                {items.map((item) => (
                  <WorkshopCard
                    key={item.id}
                    item={item}
                    ready={ready}
                    done={done.has(item.id)}
                    busy={busy === item.id}
                    onImport={() => void importItem(item)}
                  />
                ))}
              </Grid>
            )}
          </div>
        </ScrollArea>

        <Separator />
        <p className="px-6 py-4 text-xs text-muted-foreground">
          Steam no publica descarga directa de este contenido: solo está
          disponible para cuentas que poseen Wallpaper Engine. wallflux usa
          steamcmd, la herramienta oficial, con tu cuenta.
        </p>
      </DialogContent>
    </Dialog>
  );
}

/** Explica qué falta para poder descargar, y recoge la cuenta de Steam. */
function Requirements({
  view,
  steamUser,
  onSteamUser,
}: {
  view: WorkshopView | null;
  steamUser: string;
  onSteamUser: (value: string) => void;
}) {
  if (!view) return null;

  if (!view.steamcmd) {
    return (
      <div className="flex gap-3 rounded-xl border border-foreground/20 bg-card p-3">
        <AlertTriangle className="mt-0.5 size-4 shrink-0" />
        <div className="min-w-0 space-y-1">
          <p className="text-sm font-medium">Falta steamcmd</p>
          <pre className="overflow-x-auto font-mono text-[11px] whitespace-pre-wrap text-muted-foreground">
            {view.steamcmd_help}
          </pre>
        </div>
      </div>
    );
  }

  return (
    <div className="flex items-center gap-3">
      <Input
        value={steamUser}
        onChange={(event) => onSteamUser(event.target.value)}
        placeholder="Tu cuenta de Steam (la que tiene Wallpaper Engine)"
        className="flex-1"
      />
      <Tooltip>
        <TooltipTrigger asChild>
          <span className="text-xs whitespace-nowrap text-muted-foreground">
            ¿Por qué?
          </span>
        </TooltipTrigger>
        <TooltipContent className="max-w-sm">
          La contraseña no pasa por wallflux. Ejecuta{" "}
          <code>steamcmd +login tu-cuenta +quit</code> una vez en una terminal
          para autenticarte, y a partir de ahí las descargas van solas.
        </TooltipContent>
      </Tooltip>
    </div>
  );
}

function Grid({ children }: { children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[repeat(auto-fill,minmax(210px,1fr))] items-start gap-4">
      {children}
    </div>
  );
}

function WorkshopCard({
  item,
  ready,
  done,
  busy,
  onImport,
}: {
  item: WorkshopItem;
  ready: boolean;
  done: boolean;
  busy: boolean;
  onImport: () => void;
}) {
  const [broken, setBroken] = useState(false);
  const certain = item.kind === "video";
  const mayWork = workshopMayWork(item.kind);

  return (
    <div
      className={cn(
        "flex flex-col overflow-hidden rounded-xl border bg-card",
        !mayWork && "opacity-55",
      )}
    >
      <div className="relative aspect-video w-full shrink-0 overflow-hidden bg-black">
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
        <Badge
          variant="secondary"
          className="absolute top-2 left-2 rounded-full backdrop-blur-sm"
        >
          {item.kind}
        </Badge>
        {item.file_size > 0 && (
          <Badge
            variant="secondary"
            className="absolute top-2 right-2 rounded-full font-mono text-[10px] backdrop-blur-sm"
          >
            {humanSize(item.file_size)}
          </Badge>
        )}
      </div>

      <div className="flex flex-1 flex-col gap-2 p-3">
        <span className="truncate text-sm font-medium" title={item.title}>
          {item.title}
        </span>
        <span className="flex items-center gap-1.5 truncate text-xs text-muted-foreground">
          {item.resolution ?? "resolución desconocida"}
          <a
            href={item.url}
            target="_blank"
            rel="noreferrer"
            className="inline-flex hover:text-foreground"
          >
            <ExternalLink className="size-3" />
          </a>
        </span>

        {mayWork ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <span className="mt-auto">
                <Button
                  size="sm"
                  variant={done ? "secondary" : certain ? "default" : "secondary"}
                  className="w-full"
                  onClick={onImport}
                  disabled={busy || done || !ready}
                >
                  {busy ? (
                    <Loader2 className="animate-spin" />
                  ) : done ? (
                    <Check />
                  ) : (
                    <Download />
                  )}
                  {busy
                    ? "Descargando…"
                    : done
                      ? "Importado"
                      : certain
                        ? "Importar"
                        : "Intentar"}
                </Button>
              </span>
            </TooltipTrigger>
            <TooltipContent className="max-w-xs">
              {!ready
                ? "Hace falta steamcmd y tu cuenta de Steam"
                : certain
                  ? "Se importará como fondo de vídeo"
                  : "Las escenas solo funcionan si llevan un vídeo dentro del .pkg; wallflux lo extraerá si lo encuentra"}
            </TooltipContent>
          </Tooltip>
        ) : (
          <span className="mt-auto truncate text-xs text-muted-foreground">
            {describeWorkshopKind(item.kind)}
          </span>
        )}
      </div>
    </div>
  );
}
