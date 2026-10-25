import React, { useEffect, useState } from "react";
import {
  BatteryLow,
  Clapperboard,
  Cpu,
  Expand,
  Gauge,
  Proportions,
  Scaling,
  KeyRound,
  Settings2,
  Sparkles,
  SquareStack,
  Volume2,
  VolumeX,
  Zap,
} from "lucide-react";

import type { Config, ScaleMode } from "@/api";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Slider } from "@/components/ui/slider";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { cn } from "@/lib/utils";

interface Props {
  config: Config;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSave: (config: Config) => void;
}

const SCALE_MODES: ScaleMode[] = ["cover", "contain", "stretch"];
const HWDEC_MODES = ["auto-safe", "auto", "no"];

/**
 * Settings panel.
 *
 * Edits a local copy and sends the whole document on save, which is what the
 * daemon's `SetConfig` expects — the write is then atomic and the GUI can
 * never leave the config half-applied.
 */
export function Settings({ config, open, onOpenChange, onSave }: Props) {
  const [draft, setDraft] = useState<Config>(config);

  useEffect(() => setDraft(config), [config]);

  const dirty = JSON.stringify(draft) !== JSON.stringify(config);

  const playback = (patch: Partial<Config["playback"]>) =>
    setDraft({ ...draft, playback: { ...draft.playback, ...patch } });
  const power = (patch: Partial<Config["power"]>) =>
    setDraft({ ...draft, power: { ...draft.power, ...patch } });

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        // Detached from the window edge and rounded on every corner: an
        // edge-to-edge panel with square corners is the one thing in the
        // window that would not match everything else.
        className="data-[side=right]:inset-y-3 data-[side=right]:right-3 data-[side=right]:h-auto
                   flex w-full flex-col gap-0 overflow-hidden rounded-2xl border p-0
                   data-[side=right]:sm:max-w-md"
      >
        <SheetHeader className="gap-1 px-6 pt-6 pb-4">
          <SheetTitle className="flex items-center gap-2">
            <Settings2 className="size-4" />
            Ajustes
          </SheetTitle>
          <SheetDescription>
            Valores por defecto para todas las salidas. El manifiesto de un
            fondo puede sobrescribirlos.
          </SheetDescription>
        </SheetHeader>
        <Separator />

        <ScrollArea className="min-h-0 flex-1">
          <div className="space-y-8 px-6 py-6">
            <Section title="Reproducción" icon={<Clapperboard />}>
              <SliderField
                icon={<Gauge />}
                label="Límite de fotogramas"
                hint="La palanca más efectiva para el consumo de CPU"
                value={draft.playback.fps_cap}
                min={10}
                max={120}
                step={1}
                format={(value) => `${value} fps`}
                onChange={(fps_cap) => playback({ fps_cap })}
              />
              <SliderField
                icon={<Scaling />}
                label="Escala de render"
                hint="Dibuja más pequeño y escala; al 50% cuesta la cuarta parte"
                value={Math.round(draft.playback.render_scale * 100)}
                min={30}
                max={100}
                step={5}
                format={(value) => `${value}%`}
                onChange={(value) => playback({ render_scale: value / 100 })}
              />
              <ChoiceField
                icon={<Proportions />}
                label="Ajuste"
                hint="Cómo encaja un vídeo en una salida de otra proporción"
                value={draft.playback.scale_mode}
                options={SCALE_MODES}
                onChange={(scale_mode) => playback({ scale_mode })}
              />
              <SwitchField
                icon={draft.playback.mute ? <VolumeX /> : <Volume2 />}
                label="Silenciar audio"
                hint="Un fondo que hace ruido es un bug"
                checked={draft.playback.mute}
                onChange={(mute) => playback({ mute })}
              />
            </Section>

            <Section
              title="Energía"
              icon={<Zap />}
              description="Cuándo dejar de dibujar. Esto es lo que separa a wallflux de un script de shell."
            >
              <SwitchField
                icon={<Expand />}
                label="Pausar en pantalla completa"
                hint="Una ventana a pantalla completa tapa el fondo por completo"
                checked={draft.power.pause_on_fullscreen}
                onChange={(pause_on_fullscreen) => power({ pause_on_fullscreen })}
              />
              <SwitchField
                icon={<SquareStack />}
                label="Pausar al estar tapado"
                hint="Una ventana opaca cubriendo toda la salida"
                checked={draft.power.pause_on_occlusion}
                onChange={(pause_on_occlusion) => power({ pause_on_occlusion })}
              />
              <SwitchField
                icon={<BatteryLow />}
                label="Pausar con batería"
                hint="Desactivado por defecto; se usa el límite de abajo"
                checked={draft.power.pause_on_battery}
                onChange={(pause_on_battery) => power({ pause_on_battery })}
              />
              <SliderField
                icon={<BatteryLow />}
                label="Límite con batería"
                hint="Se aplica con batería cuando no se pausa del todo"
                value={draft.power.battery_fps_cap}
                min={5}
                max={60}
                step={1}
                disabled={draft.power.pause_on_battery}
                format={(value) => `${value} fps`}
                onChange={(battery_fps_cap) => power({ battery_fps_cap })}
              />
            </Section>

            <Section
              title="Galería"
              icon={<Sparkles />}
              description="El catálogo curado funciona sin configurar nada. Pexels es opcional y añade miles de vídeos."
            >
              <Field
                label="Clave de la API de Pexels"
                hint="Gratuita en pexels.com/api. Se guarda en tu config.toml."
                icon={<KeyRound />}
                stacked
                control={
                  <Input
                    type="password"
                    value={draft.gallery.pexels_api_key}
                    placeholder="sin clave — solo catálogo curado"
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        gallery: {
                          ...draft.gallery,
                          pexels_api_key: event.target.value,
                        },
                      })
                    }
                  />
                }
              />
            </Section>

            <Section title="Decodificación" icon={<Cpu />}>
              <ChoiceField
                icon={<Cpu />}
                label="Decodificación por hardware"
                hint="El hwdec de mpv; auto-safe cae a software si no es fiable"
                value={draft.playback.hwdec}
                options={HWDEC_MODES}
                onChange={(hwdec) => playback({ hwdec })}
              />
            </Section>
          </div>
        </ScrollArea>

        <Separator />
        <SheetFooter className="flex-row justify-end gap-2 px-6 py-4">
          <Button
            variant="ghost"
            onClick={() => setDraft(config)}
            disabled={!dirty}
          >
            Restablecer
          </Button>
          <Button
            onClick={() => onSave(draft)}
            disabled={!dirty}
          >
            Aplicar
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}

function Section({
  title,
  icon,
  description,
  children,
}: {
  title: string;
  icon: React.ReactNode;
  description?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="space-y-1">
      <h3 className="flex items-center gap-2 text-xs font-medium tracking-widest text-muted-foreground uppercase">
        <Icon size="size-3.5">{icon}</Icon>
        {title}
      </h3>
      {description && (
        <p className="pb-2 text-xs text-muted-foreground">{description}</p>
      )}
      <div className="divide-y divide-border">{children}</div>
    </section>
  );
}

function Field({
  label,
  hint,
  icon,
  control,
  stacked,
}: {
  label: string;
  hint?: string;
  icon: React.ReactNode;
  control: React.ReactNode;
  stacked?: boolean;
}) {
  return (
    <div
      className={
        stacked
          ? "space-y-2.5 py-4"
          : "flex items-center justify-between gap-6 py-4"
      }
    >
      <div className="flex items-start gap-2.5">
        <Icon className="mt-px text-muted-foreground">{icon}</Icon>
        <div className="space-y-0.5">
          <div className="text-sm leading-none font-medium">{label}</div>
          {hint && <div className="text-xs text-muted-foreground">{hint}</div>}
        </div>
      </div>
      {control}
    </div>
  );
}

/**
 * Sizes an icon passed in as an element.
 *
 * Taking `<Gauge />` rather than the `Gauge` component keeps the call sites
 * readable and lets one of them swap icon by state (mute does), while the
 * size stays decided in one place instead of at every usage.
 */
function Icon({
  children,
  size = "size-4",
  className,
}: {
  children: React.ReactNode;
  size?: string;
  className?: string;
}) {
  if (!React.isValidElement<{ className?: string }>(children)) return null;
  return React.cloneElement(children, {
    className: cn(size, "shrink-0", className),
  });
}

function SliderField({
  label,
  hint,
  icon,
  value,
  min,
  max,
  step,
  disabled,
  format,
  onChange,
}: {
  label: string;
  hint?: string;
  icon: React.ReactNode;
  value: number;
  min: number;
  max: number;
  step: number;
  disabled?: boolean;
  format: (value: number) => string;
  onChange: (value: number) => void;
}) {
  return (
    <Field
      label={label}
      hint={hint}
      icon={icon}
      stacked
      control={
        <div className="flex items-center gap-4">
          <Slider
            value={[value]}
            min={min}
            max={max}
            step={step}
            disabled={disabled}
            onValueChange={([next]) => next !== undefined && onChange(next)}
            className="flex-1"
          />
          <span className="w-16 text-right font-mono text-xs text-muted-foreground tabular-nums">
            {format(value)}
          </span>
        </div>
      }
    />
  );
}

function SwitchField({
  label,
  hint,
  icon,
  checked,
  onChange,
}: {
  label: string;
  hint?: string;
  icon: React.ReactNode;
  checked: boolean;
  onChange: (value: boolean) => void;
}) {
  return (
    <Field
      label={label}
      hint={hint}
      icon={icon}
      control={
        <Switch checked={checked} onCheckedChange={onChange} aria-label={label} />
      }
    />
  );
}

function ChoiceField<T extends string>({
  label,
  hint,
  icon,
  value,
  options,
  onChange,
}: {
  label: string;
  hint?: string;
  icon: React.ReactNode;
  value: T;
  options: readonly T[];
  onChange: (value: T) => void;
}) {
  return (
    <Field
      label={label}
      hint={hint}
      icon={icon}
      stacked
      control={
        <ToggleGroup
          type="single"
          value={value}
          // A toggle group can be emptied by clicking the active item; a
          // setting with three valid values has no "none", so that is ignored.
          onValueChange={(next) => next && onChange(next as T)}
          variant="outline"
          className="w-full rounded-lg"
        >
          {options.map((option) => (
            <ToggleGroupItem
              key={option}
              value={option}
              // Styled here rather than in the generated component, so a
              // `shadcn add --overwrite` cannot quietly undo it.
              className="flex-1 text-xs data-[state=on]:bg-foreground data-[state=on]:text-background"
            >
              {option}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      }
    />
  );
}
