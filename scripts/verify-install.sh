#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/wavelinux-processes.sh
source "$SCRIPT_DIR/wavelinux-processes.sh"

LAUNCH=1
TIMEOUT_SECONDS=30
STABILITY_SECONDS="${WAVELINUX_VERIFY_STABILITY_SECONDS:-5}"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/wavelinux6"
RUNTIME_BASE="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
RUNTIME_DIR="$RUNTIME_BASE/wavelinux6"

resolve_installed_executable() {
  local override="$1"
  local local_path="$2"
  local command_name="$3"
  if [[ -n "$override" ]]; then
    printf '%s\n' "$override"
  elif [[ -x "$local_path" ]]; then
    printf '%s\n' "$local_path"
  else
    command -v "$command_name" 2>/dev/null || printf '%s\n' "$local_path"
  fi
}

LAUNCHER="$(resolve_installed_executable "${WAVELINUX_VERIFY_LAUNCHER:-}" "$BIN_DIR/wavelinux6" wavelinux6)"
AUDIO_CORE="$(resolve_installed_executable "${WAVELINUX_VERIFY_AUDIO_CORE:-}" "$BIN_DIR/wavelinux6-audio-core" wavelinux6-audio-core)"
PERIPHERAL_PLUGIN="$(resolve_installed_executable "${WAVELINUX_VERIFY_PERIPHERAL_PLUGIN:-}" "$BIN_DIR/wavelinux6-peripheral-plugin" wavelinux6-peripheral-plugin)"
LAUNCH_LOG="$CONFIG_DIR/installer-launch.log"

usage() {
  cat <<'HELP'
Verify a WaveLinux 6 installation and its live PipeWire graph.

Usage:
  verify-install.sh [--no-launch] [--timeout SECONDS]

Options:
  --no-launch       Verify installed files only; do not launch or inspect the graph.
  --timeout VALUE   Maximum time to wait for services and public nodes (default: 30).
HELP
}

while (($#)); do
  case "$1" in
    --no-launch)
      LAUNCH=0
      ;;
    --timeout)
      TIMEOUT_SECONDS="${2:?missing timeout value}"
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

[[ "$TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]] || {
  echo "Invalid timeout: $TIMEOUT_SECONDS" >&2
  exit 2
}
[[ "$STABILITY_SECONDS" =~ ^[1-9][0-9]*$ ]] || {
  echo "Invalid startup stability interval: $STABILITY_SECONDS" >&2
  exit 2
}

failures=()

record_failure() {
  failures+=("$1")
}

require_executable() {
  local path="$1"
  local label="$2"
  [[ -x "$path" ]] || record_failure "$label is missing or not executable: $path"
}

print_failure_report() {
  echo >&2
  echo "WaveLinux 6 installation verification failed:" >&2
  local failure
  for failure in "${failures[@]}"; do
    echo "  - $failure" >&2
  done
  echo >&2
  echo "Relevant logs:" >&2
  echo "  $LAUNCH_LOG" >&2
  echo "  $CONFIG_DIR/wavelinux-engine.log" >&2
  echo "  $CONFIG_DIR/wavelinux6-audio-core.log" >&2
  echo >&2
  echo "Service status:" >&2
  echo "  systemctl --user status pipewire pipewire-pulse wireplumber" >&2
  echo "Audio graph:" >&2
  echo "  pactl list short sinks" >&2
  echo "  pactl list short sources" >&2

  local log
  for log in \
    "$LAUNCH_LOG" \
    "$CONFIG_DIR/wavelinux-engine.log" \
    "$CONFIG_DIR/wavelinux6-audio-core.log"; do
    if [[ -s "$log" ]]; then
      echo >&2
      echo "Last 80 lines of $log:" >&2
      tail -n 80 "$log" >&2
    fi
  done
}

require_executable "$LAUNCHER" "WaveLinux launcher"
require_executable "$AUDIO_CORE" "WaveLinux audio core"
require_executable "$PERIPHERAL_PLUGIN" "WaveLinux peripheral helper"

if ((${#failures[@]})); then
  print_failure_report
  exit 1
fi

"$AUDIO_CORE" --probe-binary >/dev/null || {
  record_failure "audio-core binary probe failed: $AUDIO_CORE --probe-binary"
}
"$PERIPHERAL_PLUGIN" --version >/dev/null || {
  record_failure "peripheral-helper binary probe failed: $PERIPHERAL_PLUGIN --version"
}

if ((LAUNCH == 0)); then
  if ((${#failures[@]})); then
    print_failure_report
    exit 1
  fi
  echo "WaveLinux 6 files and native helpers verified; launch was skipped."
  exit 0
fi

for command in pactl pw-dump; do
  command -v "$command" >/dev/null 2>&1 || record_failure "required PipeWire client command is missing: $command"
done
if ((${#failures[@]})); then
  print_failure_report
  exit 1
fi

if command -v systemctl >/dev/null 2>&1; then
  systemctl --user start pipewire.socket pipewire-pulse.socket wireplumber.service \
    >/dev/null 2>&1 || true
fi

deadline=$((SECONDS + TIMEOUT_SECONDS))
while ((SECONDS < deadline)); do
  if pactl info >/dev/null 2>&1 && pw-dump >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done

pactl info >/dev/null 2>&1 || record_failure "Pulse compatibility server is unreachable (pactl info failed)"
pw-dump >/dev/null 2>&1 || record_failure "native PipeWire server is unreachable (pw-dump failed)"
if ((${#failures[@]})); then
  print_failure_report
  exit 1
fi

install -d -m 0700 "$CONFIG_DIR"
mapfile -t running_app_pids < <(wavelinux_collect_process_pids app-runtime)
launch_pid=""
if ((${#running_app_pids[@]} == 0)); then
  nohup "$LAUNCHER" >"$LAUNCH_LOG" 2>&1 </dev/null &
  launch_pid=$!
fi

deadline=$((SECONDS + TIMEOUT_SECONDS))

core_pid=""
control_socket="$RUNTIME_DIR/control/wavelinux6-audio-core.sock"
required_sources=(
  wavelinux6-mic
  wavelinux6_mix_stream_source
  wavelinux6_mix_monitor_source
)
required_sinks=(
  wavelinux6_channel_hardware_in
  wavelinux6_channel_music
  wavelinux6_channel_game
  wavelinux6_channel_chat
  wavelinux6_channel_browser
  wavelinux6_channel_system
)

graph_is_ready() {
  core_pid="$(wavelinux_collect_process_pids audio-core | head -n1 || true)"
  app_runtime_pid="$(wavelinux_collect_process_pids app-runtime | head -n1 || true)"
  [[ -n "$app_runtime_pid" && -n "$core_pid" && -S "$control_socket" ]] || return 1

  local sources sinks name
  sources="$(pactl list short sources 2>/dev/null || true)"
  sinks="$(pactl list short sinks 2>/dev/null || true)"
  for name in "${required_sources[@]}"; do
    [[ "$(awk -v expected="$name" '$2 == expected { count++ } END { print count+0 }' <<<"$sources")" == 1 ]] \
      || return 1
  done
  for name in "${required_sinks[@]}"; do
    [[ "$(awk -v expected="$name" '$2 == expected { count++ } END { print count+0 }' <<<"$sinks")" == 1 ]] \
      || return 1
  done
  return 0
}

while ((SECONDS < deadline)); do
  graph_is_ready && break
  sleep 0.25
done

core_pid="$(wavelinux_collect_process_pids audio-core | head -n1 || true)"
app_runtime_pid="$(wavelinux_collect_process_pids app-runtime | head -n1 || true)"
[[ -n "$app_runtime_pid" ]] \
  || record_failure "WaveLinux UI process did not start (an AppImage mount helper alone is not healthy)"
[[ -n "$core_pid" ]] || record_failure "wavelinux6-audio-core did not start"
[[ -S "$control_socket" ]] || record_failure "audio-core control socket is missing: $control_socket"
if [[ -n "$launch_pid" ]] && ! kill -0 "$launch_pid" 2>/dev/null; then
  record_failure "WaveLinux application exited during startup (launch PID $launch_pid)"
fi
if [[ -s "$LAUNCH_LOG" ]]; then
  missing_library="$(sed -n \
    's/.*error while loading shared libraries: \([^:]*\):.*/\1/p' \
    "$LAUNCH_LOG" | tail -n1)"
  if [[ -n "$missing_library" ]]; then
    record_failure "WaveLinux could not load required dynamic library: $missing_library"
  fi
fi

sources="$(pactl list short sources 2>/dev/null || true)"
sinks="$(pactl list short sinks 2>/dev/null || true)"
for name in "${required_sources[@]}"; do
  node_count="$(awk -v expected="$name" '$2 == expected { count++ } END { print count+0 }' <<<"$sources")"
  [[ "$node_count" == 1 ]] \
    || record_failure "public recording source must appear exactly once: $name (found $node_count)"
done
for name in "${required_sinks[@]}"; do
  node_count="$(awk -v expected="$name" '$2 == expected { count++ } END { print count+0 }' <<<"$sinks")"
  [[ "$node_count" == 1 ]] \
    || record_failure "public channel sink must appear exactly once: $name (found $node_count)"
done

# A WebKit or graphics failure can occur shortly after the audio graph becomes
# ready. Keep checking the same processes and graph long enough to reject a
# transient startup that would otherwise leave only an orphaned audio core.
if ((${#failures[@]} == 0)); then
  stable_app_pid="$app_runtime_pid"
  stable_core_pid="$core_pid"
  stable_deadline=$((SECONDS + STABILITY_SECONDS))
  while ((SECONDS < stable_deadline)); do
    if ! graph_is_ready; then
      record_failure "WaveLinux graph stopped being ready during the ${STABILITY_SECONDS}s startup stability check"
      break
    fi
    if [[ "$app_runtime_pid" != "$stable_app_pid" ]]; then
      record_failure "WaveLinux UI PID changed during startup stability check ($stable_app_pid -> $app_runtime_pid)"
      break
    fi
    if [[ "$core_pid" != "$stable_core_pid" ]]; then
      record_failure "WaveLinux audio-core PID changed during startup stability check ($stable_core_pid -> $core_pid)"
      break
    fi
    if [[ -n "$launch_pid" ]] && ! kill -0 "$launch_pid" 2>/dev/null; then
      record_failure "WaveLinux application exited during startup stability check (launch PID $launch_pid)"
      break
    fi
    sleep 0.25
  done
  app_runtime_pid="$stable_app_pid"
  core_pid="$stable_core_pid"
fi

if [[ -n "$core_pid" && -r "/proc/$core_pid/maps" ]]; then
  pipewire_maps="$(awk '$NF ~ /libpipewire-0[.]3[.]so/ { print $NF }' "/proc/$core_pid/maps" | sort -u)"
  [[ -n "$pipewire_maps" ]] || record_failure "audio core did not load host libpipewire"
  if grep -Eq '/[.]mount_|/wavelinux6/.*libpipewire|/squashfs-root/' <<<"$pipewire_maps"; then
    record_failure "audio core loaded libpipewire from an AppImage path: $pipewire_maps"
  fi
fi

if ((${#failures[@]})); then
  print_failure_report
  exit 1
fi

echo "WaveLinux 6 live installation verified."
echo "  PipeWire and Pulse compatibility: connected"
echo "  Application PID: $app_runtime_pid"
echo "  Audio core PID: $core_pid"
echo "  Startup stability: ${STABILITY_SECONDS}s with stable UI/core PIDs"
echo "  Control socket: $control_socket"
echo "  Public sources: ${required_sources[*]}"
echo "  Public channel sinks: ${required_sinks[*]}"
