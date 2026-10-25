#!/usr/bin/env bash
#
# Runs wallflux inside a nested X server.
#
# Anything that covers the screen, grabs focus or restacks windows has to be
# tested against a real window manager, and doing that on your own session
# means a stray fullscreen window lands on top of your work. Xephyr gives you
# a throwaway display with its own bspwm, and everything here dies with it.
#
#   ./scripts/test-session.sh              # nested session, daemon, wait
#   ./scripts/test-session.sh --occlusion  # run the occlusion scenario
#
# Needs: xserver-xephyr, bspwm. Nothing touches display :0.

set -euo pipefail

readonly REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly NESTED_DISPLAY="${WALLFLUX_TEST_DISPLAY:-:9}"
readonly SIZE="${WALLFLUX_TEST_SIZE:-1280x800}"

SCENARIO="interactive"
[[ ${1:-} == "--occlusion" ]] && SCENARIO="occlusion"

command -v Xephyr >/dev/null || { echo "need Xephyr (apt install xserver-xephyr)" >&2; exit 1; }
command -v bspwm  >/dev/null || { echo "need bspwm" >&2; exit 1; }

# Isolate every piece of state, so a test run cannot read or write the
# wallpapers, config or socket of your real session.
WORK="$(mktemp -d /tmp/wallflux-test.XXXXXX)"
export WALLFLUX_CONFIG_DIR="$WORK/config"
export WALLFLUX_LIBRARY_DIR="$WORK/wallpapers"
export WALLFLUX_CACHE_DIR="$WORK/cache"
export WALLFLUX_SOCKET="$WORK/daemon.sock"
mkdir -p "$WALLFLUX_CONFIG_DIR" "$WALLFLUX_LIBRARY_DIR" "$WALLFLUX_CACHE_DIR"

declare -a PIDS=()

cleanup() {
  local pid
  # Reverse order: daemon and clients before the server they are talking to.
  for (( i=${#PIDS[@]}-1; i>=0; i-- )); do
    pid="${PIDS[i]}"
    kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
  rm -rf "$WORK"
  echo "cleaned up"
}
trap cleanup EXIT INT TERM

echo "==> nested X on $NESTED_DISPLAY ($SIZE), state in $WORK"
Xephyr -br -ac -noreset -screen "$SIZE" "$NESTED_DISPLAY" >/dev/null 2>&1 &
PIDS+=($!)

# Wait for the server rather than sleeping a guessed amount.
for _ in $(seq 1 50); do
  DISPLAY="$NESTED_DISPLAY" xdpyinfo >/dev/null 2>&1 && break
  sleep 0.1
done
DISPLAY="$NESTED_DISPLAY" xdpyinfo >/dev/null 2>&1 || { echo "Xephyr did not come up" >&2; exit 1; }

echo "==> bspwm"
# An empty config: no polybar, no autostart, nothing from your real setup.
DISPLAY="$NESTED_DISPLAY" bspwm -c /dev/null >/dev/null 2>&1 &
PIDS+=($!)
sleep 0.5
DISPLAY="$NESTED_DISPLAY" bspc monitor -d I II >/dev/null 2>&1 || true
DISPLAY="$NESTED_DISPLAY" bspc config window_gap 20 >/dev/null 2>&1 || true

echo "==> test wallpaper"
ffmpeg -nostdin -loglevel error -f lavfi -i "testsrc2=size=640x360:rate=24" \
  -t 5 -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
  -y "$WALLFLUX_LIBRARY_DIR/testcard.mp4"

echo "==> daemon"
DISPLAY="$NESTED_DISPLAY" WALLFLUX_BACKEND=x11 WALLFLUX_LOG="${WALLFLUX_LOG:-debug}" \
  "$REPO/target/release/wallflux-daemon" > "$WORK/daemon.log" 2>&1 &
PIDS+=($!)
sleep 2

ctl() { "$REPO/target/release/wallfluxctl" "$@"; }

ctl set testcard >/dev/null
sleep 2
ctl status

if [[ $SCENARIO == "occlusion" ]]; then
  echo
  echo "==> scenario: a fullscreen window appears, then goes away"
  # xterm is not guaranteed to exist; a plain X client that can go fullscreen
  # is enough, and bspwm can make any window fullscreen for us.
  DISPLAY="$NESTED_DISPLAY" ffplay -loglevel quiet -f lavfi -i "color=c=navy:s=320x200:r=10" >/dev/null 2>&1 &
  local_ffplay=$!
  PIDS+=("$local_ffplay")
  sleep 1
  DISPLAY="$NESTED_DISPLAY" bspc node -t fullscreen 2>/dev/null || true

  echo "    waiting out the pause debounce..."
  sleep 2
  echo "--- while covered (expect: paused) ---"
  ctl status | tail -2

  kill "$local_ffplay" 2>/dev/null || true
  sleep 2
  echo "--- after it closes (expect: playing) ---"
  ctl status | tail -2

  echo
  echo "--- pause transitions seen (expect few, not a flicker storm) ---"
  grep -c 'pause state changed' "$WORK/daemon.log" || true
  grep 'pause state changed' "$WORK/daemon.log" | tail -6 || true
  exit 0
fi

echo
echo "Nested session is up. Try:"
echo "  WALLFLUX_SOCKET=$WALLFLUX_SOCKET $REPO/target/release/wallfluxctl status"
echo "Ctrl-C to tear everything down."
wait "${PIDS[0]}"
