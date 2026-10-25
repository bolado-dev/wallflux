#!/usr/bin/env bash
#
# wallflux installer.
#
# Does four things, each of them skippable and each of them idempotent:
# installs the runtime libraries, installs a Rust toolchain if there is none,
# builds and installs the binaries, and makes both persistent by putting the
# install directory on your PATH in your shell's startup file.
#
#   ./install.sh                  # daemon + CLI
#   ./install.sh --with-gui       # ...and the Tauri GUI
#   ./install.sh --service        # ...and a systemd user service
#   ./install.sh --uninstall      # remove binaries and service, keep your data
#
# Run it again after pulling: it rebuilds and replaces, it does not duplicate.

set -euo pipefail

readonly REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly MARKER="# added by wallflux installer"

PREFIX="${WALLFLUX_PREFIX:-$HOME/.local/bin}"
INSTALL_DEPS=1
WITH_GUI=0
WITH_SERVICE=0
ASSUME_YES=0
MODE="install"

# --- Output ------------------------------------------------------------------

if [[ -t 1 ]]; then
  readonly C_RESET=$'\033[0m' C_BOLD=$'\033[1m' C_DIM=$'\033[2m'
  readonly C_BLUE=$'\033[34m' C_GREEN=$'\033[32m' C_YELLOW=$'\033[33m' C_RED=$'\033[31m'
else
  readonly C_RESET='' C_BOLD='' C_DIM='' C_BLUE='' C_GREEN='' C_YELLOW='' C_RED=''
fi

step()  { printf '%s==>%s %s%s%s\n' "$C_BLUE" "$C_RESET" "$C_BOLD" "$*" "$C_RESET"; }
info()  { printf '    %s\n' "$*"; }
muted() { printf '    %s%s%s\n' "$C_DIM" "$*" "$C_RESET"; }
ok()    { printf '    %s✓%s %s\n' "$C_GREEN" "$C_RESET" "$*"; }
warn()  { printf '    %s!%s %s\n' "$C_YELLOW" "$C_RESET" "$*" >&2; }
die()   { printf '%serror:%s %s\n' "$C_RED" "$C_RESET" "$*" >&2; exit 1; }

confirm() {
  [[ $ASSUME_YES -eq 1 ]] && return 0
  local reply
  printf '    %s [S/n] ' "$1"
  read -r reply || reply=n
  [[ -z $reply || $reply =~ ^[SsYy] ]]
}

usage() {
  sed -n '3,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  cat <<'USAGE'

Options:
  --prefix DIR     install binaries into DIR (default: ~/.local/bin)
  --no-deps        do not touch system packages
  --with-gui       also build and install the GUI (needs Node and pnpm)
  --service        install and enable the systemd user service
  --uninstall      remove binaries and the service; keep config and library
  --purge          --uninstall, and also delete config, library and cache
  -y, --yes        do not ask for confirmation
  -h, --help       this text
USAGE
}

# --- Argument parsing --------------------------------------------------------

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix)     PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
    --prefix=*)   PREFIX="${1#*=}"; shift ;;
    --no-deps)    INSTALL_DEPS=0; shift ;;
    --with-gui)   WITH_GUI=1; shift ;;
    --service)    WITH_SERVICE=1; shift ;;
    --uninstall)  MODE="uninstall"; shift ;;
    --purge)      MODE="purge"; shift ;;
    -y|--yes)     ASSUME_YES=1; shift ;;
    -h|--help)    usage; exit 0 ;;
    *)            die "unknown option $1 (try --help)" ;;
  esac
done

# --- Shell startup file ------------------------------------------------------

# The file to append PATH changes to, based on the login shell rather than the
# shell running this script: you want the change where your terminals read it.
shell_rc() {
  case "$(basename "${SHELL:-/bin/bash}")" in
    zsh)  printf '%s/.zshrc' "$HOME" ;;
    bash) printf '%s/.bashrc' "$HOME" ;;
    fish) printf '%s/.config/fish/config.fish' "$HOME" ;;
    *)    printf '%s/.profile' "$HOME" ;;
  esac
}

# Appends a line to the shell startup file unless an equivalent one is there.
persist_line() {
  local line="$1" rc
  rc="$(shell_rc)"
  mkdir -p "$(dirname "$rc")"
  touch "$rc"
  if grep -Fq "$line" "$rc"; then
    muted "ya está en $(basename "$rc")"
    return 0
  fi
  printf '\n%s\n%s\n' "$MARKER" "$line" >> "$rc"
  ok "añadido a $(basename "$rc")"
}

persist_path() {
  local dir="$1" rc line
  rc="$(shell_rc)"
  if [[ "$(basename "$rc")" == "config.fish" ]]; then
    line="fish_add_path $dir"
  else
    line="export PATH=\"$dir:\$PATH\""
  fi
  # Already effective and already persisted? Nothing to do.
  if grep -Fq "$dir" "$rc" 2>/dev/null; then
    muted "$dir ya está en el PATH de $(basename "$rc")"
    return 0
  fi
  persist_line "$line"
}

# --- System packages ---------------------------------------------------------

# Checks for a shared library the way the daemon does — by asking the dynamic
# loader, not the package database. A library installed from source still
# counts, and a package renamed between releases does not break this.
have_lib() {
  ldconfig -p 2>/dev/null | grep -q "[[:space:]]$1"
}

detect_pm() {
  for pm in apt-get pacman dnf zypper apk; do
    if command -v "$pm" >/dev/null 2>&1; then
      printf '%s' "$pm"
      return 0
    fi
  done
  return 1
}

# Package names per manager, for the libraries we actually found missing.
package_for() {
  local pm="$1" want="$2"
  case "$pm:$want" in
    apt-get:mpv)        printf 'libmpv2' ;;
    apt-get:egl)        printf 'libegl1' ;;
    apt-get:wlegl)      printf 'libwayland-egl1' ;;
    apt-get:ffmpeg)     printf 'ffmpeg' ;;
    apt-get:guidev)     printf 'libwebkit2gtk-4.1-dev libxdo-dev libssl-dev librsvg2-dev build-essential pkg-config' ;;

    pacman:mpv)         printf 'mpv' ;;
    pacman:egl)         printf 'libglvnd' ;;
    pacman:wlegl)       printf 'wayland' ;;
    pacman:ffmpeg)      printf 'ffmpeg' ;;
    pacman:guidev)      printf 'webkit2gtk-4.1 xdotool openssl librsvg base-devel' ;;

    dnf:mpv)            printf 'mpv-libs' ;;
    dnf:egl)            printf 'mesa-libEGL' ;;
    dnf:wlegl)          printf 'mesa-libwayland-egl' ;;
    dnf:ffmpeg)         printf 'ffmpeg-free' ;;
    dnf:guidev)         printf 'webkit2gtk4.1-devel libxdo-devel openssl-devel librsvg2-devel' ;;

    zypper:mpv)         printf 'libmpv2' ;;
    zypper:egl)         printf 'Mesa-libEGL1' ;;
    zypper:wlegl)       printf 'libwayland-egl1' ;;
    zypper:ffmpeg)      printf 'ffmpeg' ;;
    zypper:guidev)      printf 'webkit2gtk3-devel libxdo-devel libopenssl-devel librsvg-devel' ;;

    *)                  return 1 ;;
  esac
}

install_packages() {
  local pm="$1"; shift
  local -a packages=("$@")
  [[ ${#packages[@]} -eq 0 ]] && return 0

  info "hacen falta: ${packages[*]}"
  confirm "¿Instalarlos con $pm?" || { warn "me salto los paquetes del sistema"; return 0; }

  case "$pm" in
    apt-get) sudo apt-get install -y "${packages[@]}" ;;
    pacman)  sudo pacman -S --needed --noconfirm "${packages[@]}" ;;
    dnf)     sudo dnf install -y "${packages[@]}" ;;
    zypper)  sudo zypper install -y "${packages[@]}" ;;
    apk)     sudo apk add "${packages[@]}" ;;
  esac
}

do_deps() {
  step "Comprobando librerías de ejecución"
  local pm
  if ! pm="$(detect_pm)"; then
    warn "gestor de paquetes desconocido; instala esto a mano si falta:"
    warn "  libmpv (soname libmpv.so.2), libEGL, ffmpeg"
    return 0
  fi
  muted "gestor de paquetes: $pm"

  local -a wanted=()
  have_lib libmpv.so.2 || have_lib libmpv.so.1 || wanted+=(mpv)
  have_lib libEGL.so.1 || wanted+=(egl)
  # Only needed for the Wayland backend, but it is a tiny library and asking
  # "do you plan to use Wayland later" is a worse experience than installing it.
  have_lib libwayland-egl.so.1 || wanted+=(wlegl)
  command -v ffmpeg >/dev/null 2>&1 || wanted+=(ffmpeg)
  [[ $WITH_GUI -eq 1 ]] && wanted+=(guidev)

  if [[ ${#wanted[@]} -eq 0 ]]; then
    ok "ya está todo presente"
    return 0
  fi

  local -a packages=()
  local want name
  for want in "${wanted[@]}"; do
    if name="$(package_for "$pm" "$want")"; then
      # shellcheck disable=SC2206  # intentional word splitting: one entry can
      # name several packages (the GUI build dependencies).
      packages+=($name)
    else
      warn "no sé el nombre del paquete de '$want' en $pm — instálalo a mano"
    fi
  done
  install_packages "$pm" "${packages[@]}"
}

# --- Rust --------------------------------------------------------------------

do_rust() {
  step "Comprobando el toolchain de Rust"
  # shellcheck source=/dev/null
  [[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"

  if command -v cargo >/dev/null 2>&1; then
    ok "$(cargo --version)"
  else
    info "no hay cargo; instalando rustup"
    confirm "¿Instalar el toolchain de Rust desde https://sh.rustup.rs?" ||
      die "sin toolchain de Rust no se puede compilar"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
      sh -s -- -y --profile default --no-modify-path
    # shellcheck source=/dev/null
    . "$HOME/.cargo/env"
    ok "$(cargo --version)"
  fi

  # rustup is installed with --no-modify-path above so that *this* script owns
  # the persistence: one marker, one place to look when PATH goes wrong.
  persist_line '. "$HOME/.cargo/env"'
}

# --- Build and install -------------------------------------------------------

do_build() {
  step "Compilando wallflux (release)"
  ( cd "$REPO" && cargo build --release --workspace --locked ) ||
    die "la compilación ha fallado; arriba está el motivo"
  ok "compilado"
}

do_install_binaries() {
  step "Instalando en $PREFIX"
  mkdir -p "$PREFIX"
  local binary
  for binary in wallflux-daemon wallfluxctl; do
    install -m 0755 "$REPO/target/release/$binary" "$PREFIX/$binary"
    ok "$binary"
  done
  persist_path "$PREFIX"
}

do_data_dirs() {
  step "Preparando tu biblioteca y tu configuración"
  local library="${XDG_DATA_HOME:-$HOME/.local/share}/wallflux/wallpapers"
  local config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/wallflux"
  mkdir -p "$library" "$config_dir"
  ok "biblioteca: $library"

  if [[ -f "$config_dir/config.toml" ]]; then
    muted "config: $config_dir/config.toml (se deja como está)"
  else
    # Every value here is also the built-in default; the file exists to be a
    # discoverable, commented starting point.
    cat > "$config_dir/config.toml" <<'TOML'
version = 1

[playback]
# La palanca más efectiva para el consumo de CPU. A 24-30 parece vídeo;
# a 60 parece vídeo y cuesta el doble.
fps_cap = 30
mute = true
hwdec = "auto-safe"
scale_mode = "cover"      # cover | contain | stretch
# Dibuja por debajo de la resolución nativa y deja que la GPU escale. A 0.5
# cuesta la cuarta parte y apenas se nota en un fondo.
render_scale = 1.0

[power]
pause_on_fullscreen = true
pause_on_occlusion = true
pause_on_battery = false
battery_fps_cap = 15

# Ajustes por salida, con el nombre del conector como clave
# (lo ves con `wallfluxctl status`):
#
# [outputs."eDP-1"]
# wallpaper = "neon-city"
# fps_cap = 24
TOML
    ok "config: $config_dir/config.toml (escritos los valores por defecto)"
  fi
}

do_gui() {
  step "Compilando la interfaz"
  command -v node >/dev/null 2>&1 || die "la interfaz necesita Node.js"
  if command -v pnpm >/dev/null 2>&1; then
    muted "usando pnpm"
    # pnpm 10+ refuses to run a dependency's build scripts unless told to.
    # esbuild's postinstall fetches vite's native bundler binary, so without
    # this the install "succeeds" with an unusable node_modules.
    ( cd "$REPO/gui" && pnpm install --allow-build=esbuild && pnpm run tauri build --no-bundle ) ||
      die "ha fallado la compilación de la interfaz"
  else
    muted "usando npm"
    ( cd "$REPO/gui" && npm install && npm run tauri build -- --no-bundle ) ||
      die "ha fallado la compilación de la interfaz"
  fi

  # Tauri's output path moved between versions, so find it rather than guess.
  local binary
  binary="$(find "$REPO/gui/src-tauri/target/release" -maxdepth 1 -type f -name 'wallflux-gui' -perm -u+x 2>/dev/null | head -1)"
  if [[ -n $binary ]]; then
    install -m 0755 "$binary" "$PREFIX/wallflux-gui"
    ok "wallflux-gui"
  else
    warn "no encuentro el binario compilado de la interfaz; mira en gui/src-tauri/target/release"
    return 0
  fi

  do_desktop_entry
}

# Makes wallflux findable in rofi, wofi, the GNOME overview, anything that
# reads the XDG menu — which is how most people expect to open a GUI app.
do_desktop_entry() {
  step "Añadiendo wallflux al menú de aplicaciones"
  local apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  local icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
  mkdir -p "$apps"

  # Exec must be an absolute path unless PREFIX is on the PATH of whatever
  # launches it, and a launcher's environment is not your shell's.
  sed -e "s|^Exec=wallflux-gui$|Exec=$PREFIX/wallflux-gui|" \
      -e "s|^Exec=wallfluxctl |Exec=$PREFIX/wallfluxctl |" \
      "$REPO/packaging/wallflux.desktop" > "$apps/wallflux.desktop"
  chmod 0644 "$apps/wallflux.desktop"
  ok "$apps/wallflux.desktop"

  local source_icon="$REPO/gui/src-tauri/icons/icon.png"
  if [[ -f $source_icon ]]; then
    local size
    for size in 32 48 64 128 256 512; do
      mkdir -p "$icons/${size}x${size}/apps"
      if command -v magick >/dev/null 2>&1; then
        magick "$source_icon" -resize "${size}x${size}" "$icons/${size}x${size}/apps/wallflux.png"
      elif command -v convert >/dev/null 2>&1; then
        convert "$source_icon" -resize "${size}x${size}" "$icons/${size}x${size}/apps/wallflux.png"
      else
        # No ImageMagick: one unscaled copy still beats a missing icon.
        cp "$source_icon" "$icons/${size}x${size}/apps/wallflux.png"
      fi
    done
    ok "iconos instalados"
    command -v gtk-update-icon-cache >/dev/null 2>&1 &&
      gtk-update-icon-cache -q -t -f "$icons" 2>/dev/null || true
  fi

  command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database -q "$apps" 2>/dev/null || true
  command -v desktop-file-validate >/dev/null 2>&1 &&
    desktop-file-validate "$apps/wallflux.desktop" || true
}

do_service() {
  step "Instalando el servicio de usuario de systemd"
  command -v systemctl >/dev/null 2>&1 || { warn "aquí no hay systemd; me lo salto"; return 0; }
  local unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
  mkdir -p "$unit_dir"
  # Point the unit at wherever we actually installed the binaries.
  sed -e "s|%h/.cargo/bin/wallflux-daemon|$PREFIX/wallflux-daemon|" \
      -e "s|%h/.cargo/bin/wallfluxctl|$PREFIX/wallfluxctl|" \
      "$REPO/packaging/wallflux.service" > "$unit_dir/wallflux.service"
  systemctl --user daemon-reload
  ok "instalado en $unit_dir/wallflux.service"
  if confirm "¿Activarlo y arrancarlo ahora?"; then
    systemctl --user enable --now wallflux.service &&
      ok "en marcha" ||
      warn "no ha arrancado; revisa: systemctl --user status wallflux"
  else
    muted "arráncalo luego con: systemctl --user enable --now wallflux"
  fi
}

# --- Restarting what is already running --------------------------------------

# A running process keeps the binary it started from, even after the file is
# replaced — Linux just marks the old inode "(deleted)". So an upgrade changes
# nothing until the daemon restarts, which is the single most confusing thing
# about updating a daemon you installed yourself.
daemon_is_stale() {
  local pid="$1" running installed
  running="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
  # The "(deleted)" marker is the cheap answer.
  [[ $running == *"(deleted)"* ]] && return 0
  # Otherwise compare the bytes: an in-place overwrite keeps the same inode.
  running="$(md5sum "/proc/$pid/exe" 2>/dev/null | cut -d' ' -f1)"
  installed="$(md5sum "$PREFIX/wallflux-daemon" 2>/dev/null | cut -d' ' -f1)"
  [[ -n $running && -n $installed && $running != "$installed" ]]
}

do_restart() {
  local pid
  pid="$(pgrep -x wallflux-daemon | head -1 || true)"
  [[ -z $pid ]] && return 0

  step "Reiniciando el daemon"
  if ! daemon_is_stale "$pid"; then
    ok "ya está corriendo la versión que acabas de instalar"
    return 0
  fi

  # A systemd-managed daemon must be restarted through systemd, or the unit
  # and reality disagree about what is running.
  if command -v systemctl >/dev/null 2>&1 &&
     systemctl --user is-active --quiet wallflux.service 2>/dev/null; then
    info "está corriendo como servicio de usuario"
    confirm "¿Reiniciarlo con systemctl?" || { muted "me lo salto"; return 0; }
    systemctl --user restart wallflux.service && ok "reiniciado" ||
      warn "revisa: systemctl --user status wallflux"
    return 0
  fi

  info "el pid $pid corre un binario antiguo"
  confirm "¿Reiniciarlo? Tu fondo vuelve solo desde config.toml." ||
    { muted "me lo salto — seguirá con el código viejo hasta que lo reinicies"; return 0; }

  "$PREFIX/wallfluxctl" shutdown >/dev/null 2>&1 || kill "$pid" 2>/dev/null || true
  # Wait for it to actually go, rather than racing the new one onto the socket.
  local waited=0
  while kill -0 "$pid" 2>/dev/null && (( waited < 50 )); do
    sleep 0.1
    waited=$((waited + 1))
  done

  if [[ -z ${DISPLAY:-}${WAYLAND_DISPLAY:-} ]]; then
    warn "aquí no hay DISPLAY ni WAYLAND_DISPLAY; arráncalo desde tu sesión"
    return 0
  fi
  setsid "$PREFIX/wallflux-daemon" >/dev/null 2>&1 < /dev/null &
  sleep 1
  if pgrep -x wallflux-daemon >/dev/null; then
    ok "reiniciado"
  else
    warn "no ha vuelto; ejecuta $PREFIX/wallflux-daemon en una terminal para ver por qué"
  fi
}

# A GUI window is just a window: there is nothing to migrate, it only has to
# be reopened to pick up the new binary.
note_stale_gui() {
  pgrep -x wallflux-gui >/dev/null 2>&1 || return 0
  warn "la ventana de wallflux está abierta con la build antigua — ciérrala y ábrela otra vez"
}

# --- Removal -----------------------------------------------------------------

do_uninstall() {
  step "Desinstalando wallflux"
  if command -v systemctl >/dev/null 2>&1; then
    systemctl --user disable --now wallflux.service 2>/dev/null || true
    rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/wallflux.service"
    systemctl --user daemon-reload 2>/dev/null || true
  fi
  local binary
  for binary in wallflux-daemon wallfluxctl wallflux-gui; do
    if [[ -e "$PREFIX/$binary" ]]; then
      rm -f "$PREFIX/$binary"
      ok "eliminado $PREFIX/$binary"
    fi
  done

  local apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  if [[ -e "$apps/wallflux.desktop" ]]; then
    rm -f "$apps/wallflux.desktop"
    command -v update-desktop-database >/dev/null 2>&1 &&
      update-desktop-database -q "$apps" 2>/dev/null || true
    ok "eliminada la entrada del menú de aplicaciones"
  fi
  rm -f "${XDG_DATA_HOME:-$HOME/.local/share}"/icons/hicolor/*/apps/wallflux.png

  if [[ $MODE == "purge" ]]; then
    local library="${XDG_DATA_HOME:-$HOME/.local/share}/wallflux"
    local config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/wallflux"
    local cache="${XDG_CACHE_HOME:-$HOME/.cache}/wallflux"
    warn "--purge borra también tus fondos de $library"
    if confirm "¿Borrar configuración, biblioteca y caché?"; then
      rm -rf "$library" "$config_dir" "$cache"
      ok "datos borrados"
    else
      muted "datos conservados"
    fi
  else
    muted "configuración y biblioteca conservadas; usa --purge para borrarlas también"
  fi

  local rc
  rc="$(shell_rc)"
  grep -Fq "$MARKER" "$rc" 2>/dev/null &&
    muted "PATH lines in $(basename "$rc") were left alone (marked '$MARKER')"
  return 0
}

# --- Main --------------------------------------------------------------------

main() {
  printf '%swallflux%s installer\n\n' "$C_BOLD" "$C_RESET"

  if [[ $MODE != "install" ]]; then
    do_uninstall
    printf '\n'
    ok "hecho"
    return 0
  fi

  [[ -f "$REPO/Cargo.toml" ]] || die "ejecuta esto desde el repositorio de wallflux"
  [[ $EUID -eq 0 ]] && die "no lo ejecutes como root: wallflux se instala en tu carpeta personal"

  [[ $INSTALL_DEPS -eq 1 ]] && do_deps
  do_rust
  do_build
  do_install_binaries
  do_data_dirs
  [[ $WITH_GUI -eq 1 ]] && do_gui
  [[ $WITH_SERVICE -eq 1 ]] && do_service
  do_restart
  note_stale_gui

  step "Comprobando la instalación"
  if "$PREFIX/wallflux-daemon" --check; then
    ok "wallflux está listo"
  else
    warn "falta algo de lo de arriba — el daemon no arrancará hasta arreglarlo"
  fi

  cat <<EOF

${C_BOLD}Siguientes pasos${C_RESET}
  1. Abre una terminal nueva, o ejecuta: ${C_DIM}. $(shell_rc)${C_RESET}
  2. Pon un vídeo en ${C_DIM}${XDG_DATA_HOME:-$HOME/.local/share}/wallflux/wallpapers/${C_RESET}
  3. ${C_DIM}wallflux-daemon &${C_RESET}
     ${C_DIM}wallfluxctl list${C_RESET}
     ${C_DIM}wallfluxctl set <id>${C_RESET}

  ¿Ya tienes Wallpaper Engine? ${C_DIM}wallfluxctl import-engine --list${C_RESET}

  Para arrancarlo con la sesión, añade ${C_DIM}wallflux-daemon &${C_RESET} al
  autoarranque de tu gestor de ventanas, o vuelve a ejecutar este script con
  ${C_DIM}--service${C_RESET}.
EOF
}

main
