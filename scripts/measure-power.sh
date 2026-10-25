#!/usr/bin/env bash
#
# Mide lo que wallflux cuesta de batería, de verdad.
#
# Compara tres estados —sin daemon, reproduciendo y en pausa— leyendo la
# potencia instantánea que informa la batería, y traduce la diferencia a
# horas de autonomía. Hay que estar **desenchufado**: con el cargador puesto
# el sensor informa de la carga, no del consumo.
#
#   ./scripts/measure-power.sh [segundos por estado]

set -euo pipefail

SAMPLE="${1:-45}"
BAT="$(find /sys/class/power_supply -maxdepth 1 -name 'BAT*' | head -1)"
CTL="$(command -v wallfluxctl || echo "$HOME/.local/bin/wallfluxctl")"
DAEMON="$(command -v wallflux-daemon || echo "$HOME/.local/bin/wallflux-daemon")"

[[ -d $BAT ]] || { echo "no encuentro ninguna batería en /sys/class/power_supply"; exit 1; }

estado="$(cat "$BAT/status")"
if [[ $estado != "Discharging" ]]; then
  echo "La batería está en estado '$estado'."
  echo "Desenchufa el cargador: con él puesto el sensor mide la carga, no el consumo."
  exit 1
fi

# Algunas baterías informan en µW y otras en µA; con µA hace falta el voltaje.
leer_vatios() {
  if [[ -r $BAT/power_now ]]; then
    awk '{printf "%.3f", $1/1000000}' "$BAT/power_now"
  elif [[ -r $BAT/current_now && -r $BAT/voltage_now ]]; then
    python3 -c "
corriente=$(cat "$BAT/current_now")/1000000
voltaje=$(cat "$BAT/voltage_now")/1000000
print(f'{corriente*voltaje:.3f}')"
  else
    echo "0"
  fi
}

# Varias muestras y mediana: el consumo instantáneo salta mucho, y la media
# se la lleva cualquier pico de otro proceso.
medir() {
  local etiqueta="$1" muestras=()
  local fin=$((SECONDS + SAMPLE))
  while (( SECONDS < fin )); do
    muestras+=("$(leer_vatios)")
    sleep 3
  done
  local mediana
  mediana="$(printf '%s\n' "${muestras[@]}" | sort -n | awk '{v[NR]=$1} END {print (NR%2) ? v[(NR+1)/2] : (v[NR/2]+v[NR/2+1])/2}')"
  printf '  %-26s %6.2f W  (%d muestras)\n' "$etiqueta" "$mediana" "${#muestras[@]}"
  echo "$mediana"
}

capacidad="$(awk '{printf "%.1f", $1/1000000}' "$BAT/energy_full" 2>/dev/null || echo 0)"
echo "Batería: ${capacidad} Wh · ${SAMPLE}s por estado · cierra lo que puedas antes de empezar"
echo

estaba_corriendo=0
pgrep -x wallflux-daemon >/dev/null && estaba_corriendo=1
"$CTL" shutdown >/dev/null 2>&1 || true
sleep 3
base="$(medir 'sin wallflux' | tail -1)"

setsid "$DAEMON" >/dev/null 2>&1 &
sleep 5
"$CTL" resume >/dev/null 2>&1 || true
sleep 2
jugando="$(medir 'reproduciendo' | tail -1)"

"$CTL" pause >/dev/null 2>&1 || true
sleep 3
pausado="$(medir 'en pausa (tapado)' | tail -1)"
"$CTL" resume >/dev/null 2>&1 || true

(( estaba_corriendo == 0 )) && "$CTL" shutdown >/dev/null 2>&1 || true

echo
python3 - "$base" "$jugando" "$pausado" "$capacidad" <<'PY'
import sys
base, jugando, pausado, capacidad = (float(x) for x in sys.argv[1:5])
coste = jugando - base
coste_pausa = pausado - base

def horas(vatios):
    return capacidad / vatios if vatios > 0.05 else float("inf")

print(f"Coste de wallflux reproduciendo: {coste:+.2f} W")
print(f"Coste de wallflux en pausa:      {coste_pausa:+.2f} W")
if capacidad > 0 and base > 0.05:
    h_sin, h_con = horas(base), horas(jugando)
    print()
    print(f"Autonomía estimada sin wallflux: {h_sin:.1f} h")
    print(f"Autonomía estimada con el fondo: {h_con:.1f} h")
    perdida = (h_sin - h_con) * 60
    print(f"Diferencia: {perdida:.0f} minutos sobre una carga completa")
print()
print("Son cifras de tu máquina con tu fondo. Un vídeo más grande o más fps")
print("cambian el resultado; mira la tabla del README.")
PY
