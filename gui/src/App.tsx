import { useCallback, useEffect, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  AlertTriangle,
  FolderOpen,
  ImagePlus,
  Play,
  RefreshCw,
  Settings2,
  Sparkles,
  Store,
} from "lucide-react";
import { Toaster, toast } from "sonner";

import mark from "@/assets/wallflux-mark.png";

import {
  api,
  type Config,
  type DaemonInfo,
  type LibraryView,
  type Status,
} from "@/api";
import { Gallery } from "@/components/Gallery";
import { Workshop } from "@/components/Workshop";
import { OutputStrip } from "@/components/OutputStrip";
import { Settings } from "@/components/Settings";
import { WallpaperCard } from "@/components/WallpaperCard";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";

/** How often the output strip refreshes its frame-rate readout. */
const STATUS_INTERVAL = 1000;

export function App() {
  const [daemon, setDaemon] = useState<DaemonInfo | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [library, setLibrary] = useState<LibraryView | null>(null);
  const [config, setConfig] = useState<Config | null>(null);
  const [target, setTarget] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [showGallery, setShowGallery] = useState(false);
  const [showWorkshop, setShowWorkshop] = useState(false);
  const [busy, setBusy] = useState(false);

  const fail = useCallback((err: unknown) => {
    const message = typeof err === "string" ? err : String(err);
    // Errors stay until dismissed: they are usually a sentence worth reading.
    toast.error(message, { duration: Infinity, closeButton: true });
  }, []);

  const refreshStatus = useCallback(async () => {
    try {
      setStatus(await api.status());
    } catch {
      // A daemon that went away mid-session: fall back to the gate rather
      // than a toast every second.
      setStatus(null);
      setDaemon(await api.daemonInfo());
    }
  }, []);

  const load = useCallback(
    async (rescan = false) => {
      try {
        const info = await api.daemonInfo();
        setDaemon(info);
        if (!info.running) return;
        const [status, library, config] = await Promise.all([
          api.status(),
          api.listWallpapers(rescan),
          api.getConfig(),
        ]);
        setStatus(status);
        setLibrary(library);
        setConfig(config);
      } catch (err) {
        fail(err);
      }
    },
    [fail],
  );

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!daemon?.running) return;
    const timer = window.setInterval(() => void refreshStatus(), STATUS_INTERVAL);
    return () => window.clearInterval(timer);
  }, [daemon?.running, refreshStatus]);

  /** Qué salidas muestran cada fondo. */
  const activeByWallpaper = useMemo(() => {
    const map = new Map<string, string[]>();
    for (const output of status?.outputs ?? []) {
      if (!output.wallpaper) continue;
      map.set(output.wallpaper, [...(map.get(output.wallpaper) ?? []), output.name]);
    }
    return map;
  }, [status]);

  const apply = async (id: string) => {
    setBusy(true);
    try {
      await api.setWallpaper(id, target ?? undefined);
      await refreshStatus();
      toast.success(target ? `Aplicado en ${target}` : "Aplicado");
    } catch (err) {
      fail(err);
    } finally {
      setBusy(false);
    }
  };

  const importFile = async () => {
    const picked = await open({
      multiple: false,
      filters: [
        {
          name: "Vídeo",
          extensions: ["mp4", "mkv", "webm", "mov", "m4v", "avi", "gif"],
        },
      ],
    });
    if (typeof picked !== "string") return;
    setBusy(true);
    try {
      const id = await api.importWallpaper(picked);
      await load(true);
      toast.success(`Importado como ${id}`);
    } catch (err) {
      fail(err);
    } finally {
      setBusy(false);
    }
  };

  const saveConfig = async (next: Config) => {
    try {
      await api.setConfig(next);
      setConfig(next);
      await refreshStatus();
      setShowSettings(false);
      toast.success("Ajustes aplicados");
    } catch (err) {
      fail(err);
    }
  };

  return (
    <TooltipProvider delayDuration={300}>
      <Toaster theme="dark" position="bottom-center" richColors={false} />
      {daemon && !daemon.running ? (
        <Gate onReady={() => void load()} onError={fail} />
      ) : (
        <div className="flex h-full flex-col">
          <header className="flex shrink-0 items-center gap-2 px-5 py-3.5">
            <div className="flex items-center gap-2.5">
              <img src={mark} alt="" className="h-5 w-auto" />
              <span className="font-semibold tracking-tight">wallflux</span>
              {status && (
                <span className="font-mono text-[11px] text-muted-foreground">
                  {status.version} · {status.backend}
                </span>
              )}
            </div>

            <div className="flex-1" />

            <Button
              variant="secondary"
              onClick={importFile}
              disabled={busy}
            >
              <ImagePlus />
              Importar vídeo
            </Button>
            <Button variant="secondary" onClick={() => setShowGallery(true)}>
              <Sparkles />
              Galería
            </Button>
            <IconButton
              label="Taller de Wallpaper Engine"
              onClick={() => setShowWorkshop(true)}
            >
              <Store />
            </IconButton>
            <IconButton
              label="Volver a escanear la biblioteca"
              onClick={() => void load(true)}
              disabled={busy}
            >
              <RefreshCw />
            </IconButton>
            <IconButton
              label="Abrir la carpeta de la biblioteca"
              onClick={() => library && void revealItemInDir(library.library_root)}
              disabled={!library}
            >
              <FolderOpen />
            </IconButton>
            <IconButton
              label="Ajustes"
              onClick={() => setShowSettings(true)}
              disabled={!config}
            >
              <Settings2 />
            </IconButton>
          </header>

          <Separator />

          {status && (
            <>
              <OutputStrip
                outputs={status.outputs}
                selected={target}
                onSelect={setTarget}
                onToggle={(output) =>
                  api.togglePause(output).then(refreshStatus).catch(fail)
                }
              />
              <Separator />
            </>
          )}

          <ScrollArea className="min-h-0 flex-1">
            <main className="p-5">
              {library && library.errors.length > 0 && (
                <div className="mb-5 flex gap-3 rounded-xl border border-foreground/20 bg-card p-4">
                  <AlertTriangle className="mt-0.5 size-4 shrink-0" />
                  <div className="min-w-0 space-y-1.5">
                    <p className="text-sm font-medium">
                      No se {library.errors.length > 1 ? "han" : "ha"} podido cargar{" "}
                      {library.errors.length} paquete{library.errors.length > 1 ? "s" : ""}
                    </p>
                    <ul className="space-y-1 font-mono text-[11px] break-all text-muted-foreground">
                      {library.errors.map(([path, reason]) => (
                        <li key={path}>
                          {path} — {reason}
                        </li>
                      ))}
                    </ul>
                  </div>
                </div>
              )}

              {!library ? (
                <LoadingGrid />
              ) : library.wallpapers.length === 0 ? (
                <EmptyLibrary root={library.library_root} onImport={importFile} />
              ) : (
                <div className="grid grid-cols-[repeat(auto-fill,minmax(230px,1fr))] gap-4">
                  {library.wallpapers.map((wallpaper) => (
                    <WallpaperCard
                      key={wallpaper.id}
                      wallpaper={wallpaper}
                      activeOn={activeByWallpaper.get(wallpaper.id) ?? []}
                      onApply={apply}
                    />
                  ))}
                </div>
              )}
            </main>
          </ScrollArea>

          <Gallery
            open={showGallery}
            onOpenChange={setShowGallery}
            onDownloaded={() => void load(true)}
            onError={fail}
            onSuccess={(message) => toast.success(message)}
          />

          <Workshop
            open={showWorkshop}
            onOpenChange={setShowWorkshop}
            onImported={() => void load(true)}
            onError={fail}
            onSuccess={(message) => toast.success(message)}
          />

          {config && (
            <Settings
              config={config}
              open={showSettings}
              onOpenChange={setShowSettings}
              onSave={saveConfig}
            />
          )}
        </div>
      )}
    </TooltipProvider>
  );
}

function IconButton({
  label,
  children,
  ...props
}: React.ComponentProps<typeof Button> & { label: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button variant="ghost" size="icon" className="rounded-lg" aria-label={label} {...props}>
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  );
}

function LoadingGrid() {
  return (
    <div className="grid grid-cols-[repeat(auto-fill,minmax(230px,1fr))] gap-4">
      {Array.from({ length: 6 }, (_, index) => (
        <div key={index} className="space-y-3">
          <Skeleton className="aspect-video w-full rounded-xl" />
          <Skeleton className="h-4 w-2/3 rounded-md" />
          <Skeleton className="h-3 w-1/3 rounded-md" />
        </div>
      ))}
    </div>
  );
}

function EmptyLibrary({ root, onImport }: { root: string; onImport: () => void }) {
  return (
    <div className="grid place-items-center gap-4 py-24 text-center">
      <div className="grid size-16 place-items-center rounded-2xl border border-dashed">
        <img src={mark} alt="" className="h-5 w-auto opacity-35" />
      </div>
      <div className="space-y-1.5">
        <h2 className="text-base font-medium">Todavía no hay fondos</h2>
        <p className="mx-auto max-w-md text-sm text-muted-foreground">
          Importa un vídeo, o suelta uno en{" "}
          <code className="rounded-md bg-muted px-1.5 py-0.5 font-mono text-xs">
            {root}
          </code>{" "}
          y pulsa volver a escanear. Un archivo suelto ya es un fondo: no hace
          falta manifiesto.
        </p>
      </div>
      <Button onClick={onImport}>
        <ImagePlus />
        Importar un vídeo
      </Button>
    </div>
  );
}

/** Shown when no daemon is listening: the GUI is a client, not a host. */
function Gate({
  onReady,
  onError,
}: {
  onReady: () => void;
  onError: (err: unknown) => void;
}) {
  const [starting, setStarting] = useState(false);

  const start = async () => {
    setStarting(true);
    try {
      await api.startDaemon();
      onReady();
    } catch (err) {
      onError(err);
    } finally {
      setStarting(false);
    }
  };

  return (
    <div className="grid h-full place-content-center gap-5 px-10 text-center">
      <img src={mark} alt="" className="h-9 w-auto justify-self-center" />
      <div className="space-y-2">
        <h1 className="text-lg font-medium">El daemon de wallflux no está en marcha</h1>
        <p className="mx-auto max-w-md text-sm text-muted-foreground">
          Esta ventana solo habla con el daemon por su socket de control.
          Arráncalo aquí, o añádelo al autoarranque de tu gestor de ventanas
          para que el fondo sobreviva a un reinicio.
        </p>
      </div>
      <div className="flex justify-center gap-2">
        <Button onClick={() => void start()} disabled={starting}>
          {starting ? (
            <RefreshCw className="animate-spin" />
          ) : (
            <Play className="fill-current" />
          )}
          {starting ? "Arrancando…" : "Arrancar el daemon"}
        </Button>
        <Button
          variant="secondary"
          onClick={onReady}
          disabled={starting}
        >
          <RefreshCw />
          Comprobar otra vez
        </Button>
      </div>
      <pre className="mx-auto overflow-x-auto rounded-xl border bg-card p-4 text-left font-mono text-xs text-muted-foreground">
{`# para arrancarlo con la sesión, en bspwmrc o en la config de sway
wallflux-daemon &

# o como servicio de usuario
systemctl --user enable --now wallflux`}
      </pre>
    </div>
  );
}
