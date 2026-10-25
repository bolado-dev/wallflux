// Typed bindings for the Tauri commands in `src-tauri/src/lib.rs`.
//
// These types mirror the Rust ones in wallflux-core. They are written by hand
// rather than generated: the surface is small, and a hand-written type is one
// fewer build step for anyone cloning the repo.

import { invoke } from "@tauri-apps/api/core";

export type ScaleMode = "cover" | "contain" | "stretch";

export type PauseReason =
  | "manual"
  | "fullscreen"
  | "occluded"
  | "battery"
  | "idle";

export interface OutputState {
  name: string;
  width: number;
  height: number;
  x: number;
  y: number;
  scale: number;
  wallpaper: string | null;
  paused: boolean;
  pause_reason: PauseReason | null;
  measured_fps: number;
  fps_cap: number;
}

export interface Status {
  version: string;
  backend: string;
  outputs: OutputState[];
  library_root: string;
  config_path: string;
  uptime_secs: number;
}

export interface Wallpaper {
  id: string;
  title: string;
  author: string | null;
  license: string | null;
  tags: string[];
  kind: "video";
  root: string;
  source: string;
  preview: string | null;
  fps_cap: number | null;
  scale_mode: ScaleMode | null;
  mute: boolean | null;
  implicit: boolean;
}

export interface LibraryView {
  wallpapers: Wallpaper[];
  errors: [string, string][];
  library_root: string;
}

export interface PlaybackConfig {
  fps_cap: number;
  mute: boolean;
  hwdec: string;
  scale_mode: ScaleMode;
  render_scale: number;
}

export interface PowerConfig {
  pause_on_fullscreen: boolean;
  pause_on_occlusion: boolean;
  pause_on_battery: boolean;
  battery_fps_cap: number;
  idle_timeout_secs: number;
  idle_fps_cap: number;
}

export interface OutputConfig {
  wallpaper: string | null;
  fps_cap: number | null;
  scale_mode: ScaleMode | null;
  render_scale: number | null;
  disabled: boolean;
}

export interface GalleryConfig {
  catalog_url: string;
  pexels_api_key: string;
}

export interface Config {
  version: number;
  playback: PlaybackConfig;
  power: PowerConfig;
  gallery: GalleryConfig;
  outputs: Record<string, OutputConfig>;
}

export type GallerySource = "curated" | "pexels";

export interface GalleryItem {
  id: string;
  title: string;
  author: string | null;
  license: string;
  source_url: string | null;
  video_url: string;
  preview_url: string | null;
  tags: string[];
  width: number | null;
  height: number | null;
  duration_secs: number | null;
  source: GallerySource;
}

export interface GalleryView {
  items: GalleryItem[];
  /** Cierto cuando hay clave de Pexels configurada. */
  pexels_enabled: boolean;
  /** Aviso no fatal: p. ej. Pexels falló pero el catálogo cargó. */
  warning: string | null;
}

export interface DaemonInfo {
  running: boolean;
  version: string | null;
  ipc_mismatch: boolean;
}

export const api = {
  daemonInfo: () => invoke<DaemonInfo>("daemon_info"),
  status: () => invoke<Status>("status"),
  listWallpapers: (rescan = false) =>
    invoke<LibraryView>("list_wallpapers", { rescan }),
  setWallpaper: (id: string, output?: string) =>
    invoke<void>("set_wallpaper", { id, output: output ?? null }),
  clearWallpaper: (output?: string) =>
    invoke<void>("clear_wallpaper", { output: output ?? null }),
  togglePause: (output?: string) =>
    invoke<void>("toggle_pause", { output: output ?? null }),
  getConfig: () => invoke<Config>("get_config"),
  setConfig: (config: Config) => invoke<void>("set_config", { config }),
  thumbnail: (id: string) => invoke<string>("thumbnail", { id }),
  importWallpaper: (path: string, title?: string) =>
    invoke<string>("import_wallpaper", { path, title: title ?? null }),
  libraryPath: () => invoke<string>("library_path"),
  startDaemon: () => invoke<void>("start_daemon"),
  galleryBrowse: (query: string) =>
    invoke<GalleryView>("gallery_browse", { query }),
  galleryDownload: (item: GalleryItem) =>
    invoke<string>("gallery_download", { item }),
};

/** Por qué un fondo no se está moviendo, en palabras accionables. */
export function describePause(reason: PauseReason | null): string {
  switch (reason) {
    case "manual":
      return "En pausa";
    case "fullscreen":
      return "Ventana a pantalla completa";
    case "occluded":
      return "Tapado por ventanas";
    case "battery":
      return "Con batería";
    case "idle":
      return "Sin fondo";
    default:
      return "En pausa";
  }
}
