import { useCallback, useEffect, useState } from "react";
import {
  Check,
  Copy,
  Download,
  KeyRound,
  Loader2,
  Terminal,
  X,
} from "lucide-react";

import { api, type LoginCheck, type SteamSetup as Setup } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

interface Props {
  /** Se llama cuando la cuenta queda verificada y guardada. */
  onReady: (user: string) => void;
  /** Cuenta ya en uso en el diálogo del taller. */
  user: string;
  onUserChange: (user: string) => void;
}

/**
 * Asistente de tres pasos para dejar steamcmd listo.
 *
 * Existe porque «instala steamcmd y autentícate» es una instrucción que
 * suena sencilla y no lo es: el paquete está en un componente que no viene
 * activado, el login es interactivo y obligatoriamente en una terminal, y
 * no hay ningún comando que diga si ya tienes sesión. El asistente resuelve
 * las tres: detecta la distro, da el comando exacto para copiar, y comprueba
 * la sesión de verdad ejecutando steamcmd.
 */
export function SteamSetup({ onReady, user, onUserChange }: Props) {
  const [setup, setSetup] = useState<Setup | null>(null);
  const [check, setCheck] = useState<LoginCheck | null>(null);
  const [checking, setChecking] = useState(false);

  const load = useCallback(async () => {
    try {
      const fresh = await api.steamSetup();
      setSetup(fresh);
      if (!user && fresh.steam_user) onUserChange(fresh.steam_user);
    } catch {
      // Un fallo aquí solo significa que no se pinta el asistente; el
      // diálogo del taller sigue siendo útil para explorar.
    }
  }, [user, onUserChange]);

  useEffect(() => {
    void load();
  }, [load]);

  const verify = async () => {
    setChecking(true);
    setCheck(null);
    try {
      const result = await api.steamCheckLogin(user);
      setCheck(result);
      if (result.logged_in) onReady(user.trim());
    } catch (err) {
      setCheck({
        logged_in: false,
        message: typeof err === "string" ? err : String(err),
      });
    } finally {
      setChecking(false);
    }
  };

  if (!setup) return null;

  const installed = Boolean(setup.steamcmd_path);
  const verified = check?.logged_in ?? false;

  return (
    <div className="space-y-3 rounded-xl border bg-card p-4">
      <Step
        index={1}
        done={installed}
        icon={<Download className="size-3.5" />}
        title="Instalar steamcmd"
        detail={
          installed
            ? `Encontrado en ${setup.steamcmd_path}`
            : "La herramienta oficial de Valve. wallflux la usa para descargar con tu cuenta."
        }
      >
        {!installed && (
          <div className="space-y-2 pt-2">
            {setup.install_options.map((option, index) => (
              <div key={option.label} className="space-y-1">
                <div className="flex items-center gap-2">
                  <span className="w-44 shrink-0 text-xs text-muted-foreground">
                    {option.label}
                  </span>
                  <CommandLine command={option.command} />
                  {index === 0 && (
                    <Badge variant="secondary" className="rounded-full text-[10px]">
                      tu sistema
                    </Badge>
                  )}
                </div>
                {option.note && (
                  <p className="pl-44 text-[11px] text-muted-foreground">
                    {option.note}
                  </p>
                )}
              </div>
            ))}
          </div>
        )}
      </Step>

      <Step
        index={2}
        done={verified}
        icon={<Terminal className="size-3.5" />}
        title="Iniciar sesión una vez, en una terminal"
        detail="steamcmd pedirá la contraseña y el código de Steam Guard. Guarda la sesión, así que esto se hace una sola vez. Tu contraseña no pasa por wallflux."
      >
        <div className="pt-2">
          <CommandLine command={`steamcmd +login ${user.trim() || "tu-cuenta"} +quit`} />
        </div>
      </Step>

      <Step
        index={3}
        done={verified}
        icon={<KeyRound className="size-3.5" />}
        title="Comprobar la sesión"
        detail="wallflux ejecuta steamcmd para ver si ya tiene sesión. No hay ningún comando de estado, así que esta es la única forma de saberlo."
      >
        <div className="flex items-center gap-2 pt-2">
          <Input
            value={user}
            onChange={(event) => onUserChange(event.target.value)}
            placeholder="Tu cuenta de Steam"
            className="flex-1"
          />
          <Button
            variant="secondary"
            onClick={() => void verify()}
            disabled={checking || !installed || !user.trim()}
          >
            {checking ? <Loader2 className="animate-spin" /> : <Check />}
            {checking ? "Comprobando…" : "Comprobar"}
          </Button>
        </div>
        {check && (
          <p
            className={cn(
              "flex items-start gap-1.5 pt-2 text-xs",
              check.logged_in ? "text-foreground" : "text-muted-foreground",
            )}
          >
            {check.logged_in ? (
              <Check className="mt-0.5 size-3.5 shrink-0" />
            ) : (
              <X className="mt-0.5 size-3.5 shrink-0" />
            )}
            {check.message}
          </p>
        )}
        {checking && (
          <p className="pt-1 text-[11px] text-muted-foreground">
            Puede tardar unos segundos: arranca steamcmd de verdad.
          </p>
        )}
      </Step>
    </div>
  );
}

function Step({
  index,
  done,
  icon,
  title,
  detail,
  children,
}: {
  index: number;
  done: boolean;
  icon: React.ReactNode;
  title: string;
  detail: string;
  children?: React.ReactNode;
}) {
  return (
    <div className="flex gap-3">
      <div
        className={cn(
          "grid size-7 shrink-0 place-items-center rounded-lg border text-xs font-medium",
          done && "border-foreground bg-foreground text-background",
        )}
      >
        {done ? <Check className="size-3.5" /> : index}
      </div>
      <div className="min-w-0 flex-1 space-y-0.5 pb-1">
        <div className="flex items-center gap-1.5 text-sm font-medium">
          {icon}
          {title}
        </div>
        <p className="text-xs text-muted-foreground">{detail}</p>
        {children}
      </div>
    </div>
  );
}

/** Un comando con botón de copiar: nadie debería transcribir esto a mano. */
function CommandLine({ command }: { command: string }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Sin portapapeles el comando sigue estando a la vista para
      // seleccionarlo; no merece un error en pantalla.
    }
  };

  return (
    <div className="flex min-w-0 flex-1 items-center gap-2 rounded-lg bg-muted px-2.5 py-1.5">
      <code className="min-w-0 flex-1 truncate font-mono text-[11px]">{command}</code>
      <Button
        variant="ghost"
        size="icon"
        className="size-6 shrink-0 rounded-md"
        onClick={() => void copy()}
        aria-label="Copiar"
      >
        {copied ? <Check className="size-3" /> : <Copy className="size-3" />}
      </Button>
    </div>
  );
}
