import { useCallback, useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  Check,
  Download,
  FileQuestion,
  Loader2,
  RefreshCw,
  Search,
} from "lucide-react";

import {
  api,
  describeEngineKind,
  type EngineItem,
  type EngineLibrary as EngineLibraryData,
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
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Ids ya presentes en la biblioteca de wallflux. */
  alreadyImported: Set<string>;
  onImported: () => void;
  onError: (err: unknown) => void;
  onSuccess: (message: string) => void;
}

/**
 * Explorador de la biblioteca de Wallpaper Engine.
 *
 * Muestra todo lo que hay en el taller de Steam, no solo lo importable: ver
 * una escena en gris con el motivo al lado explica por qué faltan la mitad de
 * tus fondos mucho mejor que una lista que los omite en silencio.
 */
export function EngineLibrary({
  open,
  onOpenChange,
  alreadyImported,
  onImported,
  onError,
  onSuccess,
}: Props) {
  const [data, setData] = useState<EngineLibraryData | null>(null);
  const [loading, setLoading] = useState(false);
  const [importing, setImporting] = useState<string | null>(null);
  /** Lo importado en esta sesión, para marcarlo sin recargar la lista. */
  const [imported, setImported] = useState<Set<string>>(new Set());

  const scan = useCallback(async () => {
    setLoading(true);
    try {
      setData(await api.engineScan());
    } catch (err) {
      onError(err);
    } finally {
      setLoading(false);
    }
  }, [onError]);

  useEffect(() => {
    if (open && !data) void scan();
  }, [open, data, scan]);

  const importItem = async (item: EngineItem) => {
    setImporting(item.workshop_id);
    try {
      const id = await api.engineImport(item.workshop_id);
      setImported((current) => new Set(current).add(item.workshop_id));
      onSuccess(`«${item.title}» importado como ${id}`);
      onImported();
    } catch (err) {
      onError(err);
    } finally {
      setImporting(null);
    }
  };

  const items = data?.items ?? [];
  const importable = items.filter((item) => item.kind === "video");

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        // Con la variante `sm:`, porque la clase base del Dialog de shadcn
        // es `sm:max-w-lg` y tailwind-merge no la sustituye por un
        // `max-w-*` sin variante: ganaría la suya y el diálogo saldría
        // estrecho.
        className="flex h-[82vh] w-[94vw] flex-col gap-0 overflow-hidden p-0 sm:max-w-5xl"
        showCloseButton
      >
        <DialogHeader className="gap-1 px-6 pt-6 pb-4">
          <DialogTitle className="flex items-center gap-2">
            <Download className="size-4" />
            Biblioteca de Wallpaper Engine
          </DialogTitle>
          <DialogDescription>
            {items.length > 0
              ? `${importable.length} de ${items.length} elementos se pueden importar. Se copian a tu biblioteca; nada se descarga ni se modifica en Steam.`
              : "Fondos del taller de Steam que ya tienes en disco."}
          </DialogDescription>
        </DialogHeader>
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
              <NotFound searched={data?.searched ?? []} onRetry={() => void scan()} />
            ) : (
              <Grid>
                {items.map((item) => (
                  <EngineCard
                    key={item.workshop_id}
                    item={item}
                    done={
                      imported.has(item.workshop_id) ||
                      alreadyImported.has(slug(item.title))
                    }
                    busy={importing === item.workshop_id}
                    onImport={() => void importItem(item)}
                  />
                ))}
              </Grid>
            )}
          </div>
        </ScrollArea>

        <Separator />
        <div className="flex items-center justify-between gap-3 px-6 py-4">
          <span className="text-xs text-muted-foreground">
            {data?.searched[0] ?? "Buscado en las rutas habituales de Steam"}
          </span>
          <Button variant="secondary" onClick={() => void scan()} disabled={loading}>
            {loading ? <Loader2 className="animate-spin" /> : <RefreshCw />}
            Volver a buscar
          </Button>
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

function EngineCard({
  item,
  done,
  busy,
  onImport,
}: {
  item: EngineItem;
  done: boolean;
  busy: boolean;
  onImport: () => void;
}) {
  const importable = item.kind === "video";
  return (
    <div
      className={cn(
        "flex flex-col overflow-hidden rounded-xl border bg-card",
        !importable && "opacity-55",
      )}
    >
      <div className="relative aspect-video w-full bg-black">
        {item.preview ? (
          <img
            src={convertFileSrc(item.preview)}
            alt=""
            loading="lazy"
            className="size-full object-cover"
          />
        ) : (
          <div className="hatched grid size-full place-items-center">
            <FileQuestion className="size-5 text-muted-foreground" />
          </div>
        )}
        <Badge
          variant="secondary"
          className="absolute top-2 left-2 rounded-full backdrop-blur-sm"
        >
          {item.kind}
        </Badge>
      </div>

      <div className="flex flex-1 flex-col gap-2 p-3">
        <span className="truncate text-sm font-medium" title={item.title}>
          {item.title}
        </span>

        {importable ? (
          <Button
            size="sm"
            variant={done ? "secondary" : "default"}
            className="mt-auto w-full"
            onClick={onImport}
            disabled={busy || done}
          >
            {busy ? (
              <Loader2 className="animate-spin" />
            ) : done ? (
              <Check />
            ) : (
              <Download />
            )}
            {busy ? "Importando…" : done ? "Importado" : "Importar"}
          </Button>
        ) : (
          <Tooltip>
            <TooltipTrigger asChild>
              <div className="mt-auto cursor-help truncate text-xs text-muted-foreground">
                {describeEngineKind(item.kind)}
              </div>
            </TooltipTrigger>
            <TooltipContent className="max-w-xs">
              {describeEngineKind(item.kind)}
            </TooltipContent>
          </Tooltip>
        )}
      </div>
    </div>
  );
}

function NotFound({
  searched,
  onRetry,
}: {
  searched: string[];
  onRetry: () => void;
}) {
  return (
    <div className="grid place-items-center gap-4 py-20 text-center">
      <div className="grid size-16 place-items-center rounded-2xl border border-dashed">
        <Search className="size-5 text-muted-foreground" />
      </div>
      <div className="space-y-1.5">
        <h3 className="text-base font-medium">
          No se ha encontrado contenido de Wallpaper Engine
        </h3>
        <p className="mx-auto max-w-lg text-sm text-muted-foreground">
          {searched.length > 0
            ? "Hay carpeta del taller, pero está vacía."
            : "No aparece ninguna carpeta del taller de Steam. wallflux busca en las rutas habituales, incluida la de Flatpak."}
        </p>
        {searched.length > 0 && (
          <ul className="mx-auto max-w-lg space-y-0.5 pt-1 font-mono text-[11px] break-all text-muted-foreground">
            {searched.map((path) => (
              <li key={path}>{path}</li>
            ))}
          </ul>
        )}
        <p className="mx-auto max-w-lg pt-2 text-xs text-muted-foreground">
          Si tu Steam está en otro sitio, desde la terminal:{" "}
          <code className="rounded-md bg-muted px-1.5 py-0.5 font-mono">
            wallfluxctl import-engine --from &lt;carpeta&gt;
          </code>
        </p>
      </div>
      <Button variant="secondary" onClick={onRetry}>
        <RefreshCw />
        Volver a buscar
      </Button>
    </div>
  );
}

/** Mismo slug que usan el núcleo y el daemon, para detectar duplicados. */
function slug(name: string): string {
  let out = "";
  for (const ch of name) {
    if (/[a-zA-Z0-9_-]/.test(ch)) out += ch.toLowerCase();
    else if (!out.endsWith("-")) out += "-";
  }
  return out.replace(/^-+|-+$/g, "");
}
