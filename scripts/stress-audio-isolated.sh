#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
APPIMAGE="${1:-$(find "$DATA_HOME/wavelinux6" -maxdepth 1 -type f -name 'WaveLinux6_*_amd64.AppImage' -print 2>/dev/null | sort -V | tail -n1)}"
AUDIO_CORE="${WAVELINUX_STRESS_AUDIO_CORE:-${XDG_BIN_HOME:-$HOME/.local/bin}/wavelinux6-audio-core}"
PERIPHERAL_PLUGIN="${WAVELINUX_STRESS_PERIPHERAL_PLUGIN:-${XDG_BIN_HOME:-$HOME/.local/bin}/wavelinux6-peripheral-plugin}"
TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUTPUT_DIR="${WAVELINUX_STRESS_ISOLATED_OUTPUT_DIR:-$ROOT_DIR/target/stress/isolated-$TIMESTAMP}"
SESSION_ROOT="$OUTPUT_DIR/session"
SESSION_RUNTIME="${WAVELINUX_STRESS_SESSION_RUNTIME:-$(mktemp -d "${TMPDIR:-/tmp}/wl6-stress-rt.XXXXXX")}"
SESSION_CONFIG="$SESSION_ROOT/config"
SESSION_DATA="$SESSION_ROOT/data"
SERVICE_LOG_DIR="$SESSION_ROOT/logs"
XVFB_PID=""

[[ -x "$APPIMAGE" ]] || {
  echo "WaveLinux 6 AppImage is missing or not executable: ${APPIMAGE:-<none>}" >&2
  exit 2
}
[[ -x "$AUDIO_CORE" ]] || {
  echo "WaveLinux 6 audio core is missing or not executable: $AUDIO_CORE" >&2
  exit 2
}
[[ -x "$PERIPHERAL_PLUGIN" ]] || {
  echo "WaveLinux 6 peripheral helper is missing or not executable: $PERIPHERAL_PLUGIN" >&2
  exit 2
}
for command_name in dbus-run-session jq pipewire pipewire-pulse wireplumber pactl pw-dump; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "Isolated stress setup requires: $command_name" >&2
    exit 2
  }
done

install -d -m 0700 \
  "$OUTPUT_DIR" "$SESSION_RUNTIME" "$SESSION_CONFIG" "$SESSION_DATA" "$SERVICE_LOG_DIR"
OUTPUT_DIR="$(realpath "$OUTPUT_DIR")"
SESSION_ROOT="$OUTPUT_DIR/session"
SESSION_CONFIG="$SESSION_ROOT/config"
SESSION_DATA="$SESSION_ROOT/data"
SERVICE_LOG_DIR="$SESSION_ROOT/logs"

cleanup_outer() {
  if [[ -n "$XVFB_PID" ]]; then
    kill "$XVFB_PID" 2>/dev/null || true
    wait "$XVFB_PID" 2>/dev/null || true
  fi
  for _ in {1..20}; do
    if rmdir "$SESSION_RUNTIME/doc" "$SESSION_RUNTIME/gvfs" "$SESSION_RUNTIME" \
      2>/dev/null; then
      break
    fi
    sleep 0.1
  done
}
trap cleanup_outer EXIT

if command -v Xvfb >/dev/null 2>&1; then
  display_number="${WAVELINUX_STRESS_DISPLAY:-:97}"
  Xvfb "$display_number" -screen 0 1440x900x24 -nolisten tcp -ac \
    >"$SERVICE_LOG_DIR/xvfb.log" 2>&1 &
  XVFB_PID=$!
  export DISPLAY="$display_number"
  sleep 1
elif [[ -z "${DISPLAY:-}" ]]; then
  echo "Xvfb is unavailable and no desktop DISPLAY can host the isolated UI." >&2
  exit 2
else
  echo "Xvfb is unavailable; using DISPLAY=$DISPLAY while keeping audio on an isolated PipeWire server."
fi

export XDG_RUNTIME_DIR="$SESSION_RUNTIME"
export XDG_CONFIG_HOME="$SESSION_CONFIG"
export XDG_DATA_HOME="$SESSION_DATA"
export APPIMAGE_EXTRACT_AND_RUN=1
export WAVELINUX_ASSUME_RUNTIME_DEPS=1
export WAVELINUX_SKIP_AUDIO_SERVICE_START=1
export WAVELINUX_DSP_HELPER="$AUDIO_CORE"
export WAVELINUX_PERIPHERAL_PLUGIN="$PERIPHERAL_PLUGIN"
export WEBKIT_DISABLE_DMABUF_RENDERER=1
export WEBKIT_DISABLE_COMPOSITING_MODE=1

# The isolated session owns only PIDs it starts; cleanup cannot touch the
# user's normal WaveLinux or desktop PipeWire processes.
# shellcheck disable=SC2016
dbus-run-session -- bash -c '
  set -euo pipefail
  appimage="$1"
  output_dir="$2"
  service_log_dir="$3"
  stress_script="$4"
  manifest="$XDG_DATA_HOME/wavelinux6/effects/wavelinux6-audio-core.json"
  launch_pid=""
  app_pid=""
  core_pid=""

  pipewire >"$service_log_dir/pipewire.log" 2>&1 &
  pipewire_pid=$!
  pulse_pid=""
  wireplumber_pid=""

  cleanup_session() {
    set +e
    [[ -z "$launch_pid" ]] || kill -TERM "$launch_pid" 2>/dev/null
    [[ -z "$app_pid" ]] || kill -TERM "$app_pid" 2>/dev/null
    [[ -z "$core_pid" ]] || kill -TERM "$core_pid" 2>/dev/null
    sleep 0.25
    [[ -z "$launch_pid" ]] || kill -KILL "$launch_pid" 2>/dev/null
    [[ -z "$app_pid" ]] || kill -KILL "$app_pid" 2>/dev/null
    [[ -z "$core_pid" ]] || kill -KILL "$core_pid" 2>/dev/null
    [[ -z "$wireplumber_pid" ]] || kill -TERM "$wireplumber_pid" 2>/dev/null
    [[ -z "$pulse_pid" ]] || kill -TERM "$pulse_pid" 2>/dev/null
    kill -TERM "$pipewire_pid" 2>/dev/null
    for child_pid in "$launch_pid" "$app_pid" "$core_pid" "$wireplumber_pid" "$pulse_pid" "$pipewire_pid"; do
      [[ -z "$child_pid" ]] || wait "$child_pid" 2>/dev/null
    done
  }
  trap cleanup_session EXIT INT TERM

  native_ready=0
  for _ in {1..120}; do
    if kill -0 "$pipewire_pid" 2>/dev/null && pw-dump >/dev/null 2>&1; then
      native_ready=1
      break
    fi
    sleep 0.25
  done
  ((native_ready == 1)) || {
    echo "Isolated PipeWire server did not become ready." >&2
    cat "$service_log_dir/pipewire.log" >&2
    exit 1
  }

  wireplumber -p policy >"$service_log_dir/wireplumber.log" 2>&1 &
  wireplumber_pid=$!
  policy_ready=0
  for _ in {1..120}; do
    if ! kill -0 "$wireplumber_pid" 2>/dev/null; then
      break
    fi
    if pw-dump 2>/dev/null | jq -e "
      any(.[]; .type == \"PipeWire:Interface:Client\"
        and (.info.props[\"application.process.binary\"] // \"\") == \"wireplumber\")
    " >/dev/null; then
      policy_ready=1
      break
    fi
    sleep 0.25
  done
  ((policy_ready == 1)) || {
    echo "Isolated WirePlumber policy session did not become ready." >&2
    cat "$service_log_dir/wireplumber.log" >&2
    exit 1
  }

  pipewire-pulse >"$service_log_dir/pipewire-pulse.log" 2>&1 &
  pulse_pid=$!
  pulse_ready=0
  for _ in {1..120}; do
    if kill -0 "$pulse_pid" 2>/dev/null && pactl info >/dev/null 2>&1; then
      pulse_ready=1
      break
    fi
    sleep 0.25
  done
  ((pulse_ready == 1)) || {
    echo "Isolated PipeWire Pulse server did not become ready." >&2
    cat "$service_log_dir/pipewire-pulse.log" >&2
    exit 1
  }

  "$appimage" >"$service_log_dir/app.log" 2>&1 &
  launch_pid=$!
  graph_ready=0
  for _ in {1..160}; do
    app_pid="$(
      while read -r candidate_pid; do
        executable="$(readlink "/proc/$candidate_pid/exe" 2>/dev/null || true)"
        [[ "${executable##*/}" == wavelinux6 ]] || continue
        if tr "\0" "\n" <"/proc/$candidate_pid/environ" 2>/dev/null \
          | grep -Fxq "XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR"; then
          printf "%s\n" "$candidate_pid"
          break
        fi
      done < <(ps -u "$(id -u)" -o pid=)
    )"
    if [[ -f "$manifest" && -S "$XDG_RUNTIME_DIR/wavelinux6/control/wavelinux6-audio-core.sock" ]]; then
      core_pid="$(ps -u "$(id -u)" -o pid=,args= | awk -v manifest="$manifest" \
        '"'"'index($0, "--run-core --manifest " manifest) { print $1; exit }'"'"')"
      if [[ -n "$app_pid" && -n "$core_pid" ]] \
        && pactl list short sources | awk '"'"'$2 == "wavelinux6_mix_stream_source" { found=1 } END { exit !found }'"'"'; then
        graph_ready=1
        break
      fi
    fi
    kill -0 "$launch_pid" 2>/dev/null || {
      echo "WaveLinux UI exited before isolated graph readiness." >&2
      cat "$service_log_dir/app.log" >&2
      exit 1
    }
    sleep 0.25
  done
  ((graph_ready == 1)) || {
    echo "WaveLinux isolated graph did not become ready." >&2
    exit 1
  }

  unsafe_monitor_targets="$(jq '"'"'[
    .mixes[]
    | select(.mix_id == "monitor")
    | .output_target_node_names[]?
    | select(test("(^|[._-])(auto_null|null|dummy)($|[._-])"; "i") | not)
  ] | length'"'"' "$manifest")"
  ((unsafe_monitor_targets == 0)) || {
    echo "Isolated graph unexpectedly acquired a non-null monitor target." >&2
    exit 1
  }

  env \
    WAVELINUX_STRESS_APP_PID="$app_pid" \
    WAVELINUX_STRESS_CORE_PID="$core_pid" \
    WAVELINUX_STRESS_SERVICE_LOG_DIR="$service_log_dir" \
    WAVELINUX_STRESS_OUTPUT_DIR="$output_dir/results" \
    bash "$stress_script"
' bash "$APPIMAGE" "$OUTPUT_DIR" "$SERVICE_LOG_DIR" "$ROOT_DIR/scripts/stress-audio-runtime.sh"

echo "WaveLinux 6 isolated audio continuity stress passed: $OUTPUT_DIR/results/report.json"
