#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/wavelinux-processes.sh
source "$ROOT_DIR/scripts/wavelinux-processes.sh"
DURATION_SEC="${WAVELINUX_STRESS_DURATION_SEC:-3600}"
CPU_COUNT="$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc)"
# The parallel network stream consumes several additional cores. Keep half the
# machine free for PipeWire, recording, disk I/O, and that network workload so
# the gate measures audio continuity instead of total scheduler starvation.
CPU_WORKERS="${WAVELINUX_STRESS_CPU_WORKERS:-$((CPU_COUNT > 3 ? CPU_COUNT / 2 : 1))}"
DISK_MIB="${WAVELINUX_STRESS_DISK_MIB:-512}"
NETWORK_PARALLEL="${WAVELINUX_STRESS_NETWORK_PARALLEL:-4}"
TONE_FREQUENCY="${WAVELINUX_STRESS_TONE_HZ:-400}"
# Keep the deterministic pilot near -42 dBFS. It remains well above the
# analyzer floor but cannot become a dangerous full-volume signal if a host
# policy unexpectedly links the fixture to physical monitoring.
TONE_AMPLITUDE="${WAVELINUX_STRESS_TONE_AMPLITUDE:-256}"
TONE_SILENCE_THRESHOLD="$((TONE_AMPLITUDE > 16 ? TONE_AMPLITUDE / 8 : 1))"
TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUTPUT_DIR="${WAVELINUX_STRESS_OUTPUT_DIR:-$ROOT_DIR/target/stress/$TIMESTAMP}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux6"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/wavelinux6"
MANIFEST="$DATA_DIR/effects/wavelinux6-audio-core.json"
ENGINE_LOG="$CONFIG_DIR/wavelinux-engine.log"
CORE_LOG="$CONFIG_DIR/wavelinux6-audio-core.log"
BASELINE_FILE="$ROOT_DIR/benchmarks/wavelinux5-live-baseline.json"
CONTROL_SOCKET=""
CORE_PROTOCOL=""
STRESS_CHANNEL_ID=""
STRESS_CHANNEL_SINK=""
MONITOR_BUS_VOLUME=""
MONITOR_BUS_MUTED=""
MONITOR_BUS_ENABLED=""
STREAM_BUS_VOLUME=""
STREAM_BUS_MUTED=""
STREAM_BUS_ENABLED=""
STREAM_MASTER_VOLUME=""
STREAM_MASTER_MUTED=""
stress_bus_overrides_active=0

if ! [[ "$DURATION_SEC" =~ ^[0-9]+$ ]] || ((DURATION_SEC < 10)); then
  echo "WAVELINUX_STRESS_DURATION_SEC must be an integer of at least 10" >&2
  exit 2
fi
if ! [[ "$CPU_WORKERS" =~ ^[0-9]+$ ]] || ((CPU_WORKERS < 1)); then
  echo "WAVELINUX_STRESS_CPU_WORKERS must be a positive integer" >&2
  exit 2
fi
if ! [[ "$DISK_MIB" =~ ^[0-9]+$ ]] || ((DISK_MIB < 16)); then
  echo "WAVELINUX_STRESS_DISK_MIB must be at least 16" >&2
  exit 2
fi

required_commands=(awk cmp cp cut date dd getconf grep head iperf3 journalctl jq pactl paplay parec pgrep ps pw-metadata python3 sed setsid sha256sum socat sort timeout wc)
for command_name in "${required_commands[@]}"; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "Required stress-test command is missing: $command_name" >&2
    exit 2
  fi
done
if [[ ! -f "$MANIFEST" ]]; then
  echo "WaveLinux 6 audio-core manifest is missing: $MANIFEST" >&2
  exit 2
fi

unsafe_monitor_target_count="$(jq '[
  .mixes[]
  | select(.mix_id == "monitor")
  | .output_target_node_names[]?
  | select(test("(^|[._-])(auto_null|null|dummy)($|[._-])"; "i") | not)
] | length' "$MANIFEST")"
if ((unsafe_monitor_target_count > 0)) && [[ "${WAVELINUX_STRESS_ALLOW_PHYSICAL_MONITOR:-0}" != 1 ]]; then
  echo "Refusing to inject a continuity pilot into a graph connected to physical monitoring." >&2
  echo "Run scripts/stress-audio-isolated.sh, or set WAVELINUX_STRESS_ALLOW_PHYSICAL_MONITOR=1 only in a controlled lab." >&2
  exit 2
fi
if [[ ! -f "$BASELINE_FILE" ]]; then
  echo "WaveLinux5 live baseline is missing: $BASELINE_FILE" >&2
  exit 2
fi

CORE_PROTOCOL="$(jq -r '.protocol_version // empty' "$MANIFEST")"
CONTROL_SOCKET="$(jq -r '.control_socket_path // empty' "$MANIFEST")"
if [[ ! "$CORE_PROTOCOL" =~ ^[0-9]+$ ]] || [[ ! -S "$CONTROL_SOCKET" ]]; then
  echo "WaveLinux 6 audio-core control protocol is unavailable" >&2
  exit 2
fi
STRESS_CHANNEL_ID="${WAVELINUX_STRESS_CHANNEL_ID:-$(jq -r '
  [.channels[]
    | select(.channel_id != "hardware_in")
    | select([.effects[]? | select(.bypassed != true)] | length == 0)
    | .channel_id] as $ids
  | if ($ids | index("chat")) then "chat" else ($ids[0] // empty) end
' "$MANIFEST")}"
STRESS_CHANNEL_SINK="$(jq -r --arg channel "$STRESS_CHANNEL_ID" \
  '.channels[] | select(.channel_id == $channel) | .input_node_name' "$MANIFEST")"
if [[ -z "$STRESS_CHANNEL_ID" || -z "$STRESS_CHANNEL_SINK" ]]; then
  echo "No effect-free WaveLinux 6 app channel is available for the stress fixture" >&2
  exit 2
fi

read -r MONITOR_BUS_VOLUME MONITOR_BUS_MUTED MONITOR_BUS_ENABLED < <(
  jq -r --arg channel "$STRESS_CHANNEL_ID" '
    .mixes[] | select(.mix_id == "monitor")
    | .buses[] | select(.channel_id == $channel)
    | [.volume, .muted, .enabled] | @tsv
  ' "$MANIFEST"
)
read -r STREAM_BUS_VOLUME STREAM_BUS_MUTED STREAM_BUS_ENABLED < <(
  jq -r --arg channel "$STRESS_CHANNEL_ID" '
    .mixes[] | select(.mix_id == "stream")
    | .buses[] | select(.channel_id == $channel)
    | [.volume, .muted, .enabled] | @tsv
  ' "$MANIFEST"
)
read -r STREAM_MASTER_VOLUME STREAM_MASTER_MUTED < <(
  jq -r '.mixes[] | select(.mix_id == "stream") | [.volume, .muted] | @tsv' "$MANIFEST"
)
if [[ -z "$MONITOR_BUS_VOLUME" || -z "$STREAM_BUS_VOLUME" || -z "$STREAM_MASTER_VOLUME" ]]; then
  echo "Stress channel $STRESS_CHANNEL_ID is not connected to both native mixes" >&2
  exit 2
fi

live_pid_by_comm() {
  local name="$1"
  ps -eo pid=,stat=,comm= | awk -v name="$name" '$2 !~ /^Z/ && $3 == name { print $1; exit }'
}

app_pid="${WAVELINUX_STRESS_APP_PID:-$(wavelinux_collect_process_pids app-runtime | head -n1 || true)}"
core_pid="${WAVELINUX_STRESS_CORE_PID:-$(wavelinux_collect_process_pids audio-core | head -n1 || true)}"
pipewire_pid="$(live_pid_by_comm pipewire || true)"
pulse_pid="$(live_pid_by_comm pipewire-pulse || true)"
if [[ -z "$app_pid" || -z "$core_pid" ]]; then
  echo "WaveLinux 6 and its audio core must already be running" >&2
  exit 2
fi

mkdir -p "$OUTPUT_DIR"
chmod 700 "$OUTPUT_DIR"
DIAGNOSTICS_FILE="$OUTPUT_DIR/core-diagnostics.jsonl"
CPU_FILE="$OUTPUT_DIR/process-cpu.jsonl"
CAPTURE_FILE="$OUTPUT_DIR/stream-s16le.raw"
ANALYSIS_FILE="$OUTPUT_DIR/audio-analysis.json"
REPORT_FILE="$OUTPUT_DIR/report.json"
ENGINE_DELTA="$OUTPUT_DIR/engine-delta.log"
CORE_DELTA="$OUTPUT_DIR/core-delta.log"
JOURNAL_FILE="$OUTPUT_DIR/journal.log"
WARNING_FILE="$OUTPUT_DIR/pipewire-warnings.log"
ROUTE_TIMINGS_FILE="$OUTPUT_DIR/route-timings-ms.txt"
DISK_ITERATIONS_FILE="$OUTPUT_DIR/disk-iterations.log"
NETWORK_FILE="$OUTPUT_DIR/network.json"
TONE_LOG="$OUTPUT_DIR/tone.log"
RECORDER_LOG="$OUTPUT_DIR/recorder.log"

declare -a BACKGROUND_PIDS=()
declare -a PROCESS_GROUPS=()
cleanup_complete=0

stop_background_loads() {
  set +e
  for pid in "${PROCESS_GROUPS[@]:-}"; do
    [[ -n "$pid" ]] && kill -TERM -- "-$pid" 2>/dev/null
  done
  for pid in "${BACKGROUND_PIDS[@]:-}"; do
    [[ -n "$pid" ]] && kill -TERM "$pid" 2>/dev/null
  done
  sleep 0.2
  for pid in "${PROCESS_GROUPS[@]:-}"; do
    [[ -n "$pid" ]] && kill -KILL -- "-$pid" 2>/dev/null
  done
  for pid in "${BACKGROUND_PIDS[@]:-}"; do
    [[ -n "$pid" ]] && kill -KILL "$pid" 2>/dev/null
  done
  for pid in "${BACKGROUND_PIDS[@]:-}" "${PROCESS_GROUPS[@]:-}"; do
    [[ -n "$pid" ]] && wait "$pid" 2>/dev/null
  done
  rm -f "$OUTPUT_DIR/disk-load.tmp"
  restore_stress_bus_overrides
  set -e
}

# Invoked indirectly by the EXIT/INT/TERM trap.
# shellcheck disable=SC2317,SC2329
cleanup_on_exit() {
  stop_background_loads
  if ((cleanup_complete == 0)); then
    printf '{"complete":false,"reason":"stress runner interrupted"}\n' > "$REPORT_FILE"
  fi
}
trap cleanup_on_exit EXIT INT TERM

line_count() {
  if [[ -f "$1" ]]; then
    wc -l < "$1"
  else
    printf '0\n'
  fi
}

copy_log_delta() {
  local source="$1"
  local starting_line="$2"
  local destination="$3"
  if [[ ! -f "$source" ]]; then
    : > "$destination"
    return
  fi
  local current_lines
  current_lines="$(line_count "$source")"
  if ((current_lines >= starting_line)); then
    sed -n "$((starting_line + 1)),\$p" "$source" > "$destination"
  else
    cp "$source" "$destination"
  fi
}

core_control() {
  local payload="$1"
  printf '%s' "$payload" | timeout 2s socat - "UNIX-CONNECT:$CONTROL_SOCKET"
}

set_mix_bus_runtime() {
  local mix_id="$1"
  local channel_id="$2"
  local volume="$3"
  local muted="$4"
  local enabled="$5"
  local payload response
  payload="$(jq -cn \
    --argjson protocol_version "$CORE_PROTOCOL" \
    --arg mix_id "$mix_id" \
    --arg channel_id "$channel_id" \
    --argjson volume "$volume" \
    --argjson muted "$muted" \
    --argjson enabled "$enabled" \
    '{protocol_version:$protocol_version,command:"set_mix_bus",request_id:"stress-gate",mix_id:$mix_id,channel_id:$channel_id,volume:$volume,muted:$muted,enabled:$enabled}')"
  response="$(core_control "$payload")"
  jq -e '.ok == true' >/dev/null <<<"$response"
}

set_mix_master_runtime() {
  local mix_id="$1"
  local volume="$2"
  local muted="$3"
  local payload response
  payload="$(jq -cn \
    --argjson protocol_version "$CORE_PROTOCOL" \
    --arg mix_id "$mix_id" \
    --argjson volume "$volume" \
    --argjson muted "$muted" \
    '{protocol_version:$protocol_version,command:"set_mix_master",request_id:"stress-gate",mix_id:$mix_id,volume:$volume,muted:$muted}')"
  response="$(core_control "$payload")"
  jq -e '.ok == true' >/dev/null <<<"$response"
}

restore_stress_bus_overrides() {
  if ((stress_bus_overrides_active == 0)) || [[ ! -S "$CONTROL_SOCKET" ]]; then
    return
  fi
  stress_bus_overrides_active=0
  set_mix_bus_runtime monitor "$STRESS_CHANNEL_ID" \
    "$MONITOR_BUS_VOLUME" "$MONITOR_BUS_MUTED" "$MONITOR_BUS_ENABLED" || true
  set_mix_bus_runtime stream "$STRESS_CHANNEL_ID" \
    "$STREAM_BUS_VOLUME" "$STREAM_BUS_MUTED" "$STREAM_BUS_ENABLED" || true
  set_mix_master_runtime stream "$STREAM_MASTER_VOLUME" "$STREAM_MASTER_MUTED" || true
}

query_core_socket() {
  local socket="$1"
  local route_kind="$2"
  local route_id="$3"
  local payload
  if [[ "$route_kind" == "mix" ]]; then
    payload="$(jq -cn \
      --argjson protocol_version "$CORE_PROTOCOL" \
      --arg mix_id "$route_id" \
      '{protocol_version:$protocol_version,command:"get_diagnostics",request_id:"stress-gate",mix_id:$mix_id}')"
  else
    payload="$(jq -cn \
      --argjson protocol_version "$CORE_PROTOCOL" \
      --arg route_id "$route_id" \
      '{protocol_version:$protocol_version,command:"get_diagnostics",request_id:"stress-gate",route_id:$route_id}')"
  fi
  printf '%s' "$payload" | timeout 2s socat - "UNIX-CONNECT:$socket"
}

capture_core_diagnostics() {
  local phase="$1"
  local now_ms
  now_ms="$(date +%s%3N)"
  while IFS=$'\t' read -r route_kind route_id socket; do
    [[ -S "$socket" ]] || continue
    local response
    if response="$(query_core_socket "$socket" "$route_kind" "$route_id" 2>/dev/null)" \
      && jq -e . >/dev/null 2>&1 <<<"$response"; then
      jq -cn \
        --argjson timestamp_ms "$now_ms" \
        --arg phase "$phase" \
        --arg socket "$socket" \
        --arg route_kind "$route_kind" \
        --arg route_id "$route_id" \
        --argjson response "$response" \
        '{timestamp_ms:$timestamp_ms,phase:$phase,socket:$socket,route_kind:$route_kind,route_id:$route_id,response:$response}' \
        >> "$DIAGNOSTICS_FILE"
    else
      jq -cn \
        --argjson timestamp_ms "$now_ms" \
        --arg phase "$phase" \
        --arg socket "$socket" \
        '{timestamp_ms:$timestamp_ms,phase:$phase,socket:$socket,response:{ok:false,error:"socket query failed"}}' \
        >> "$DIAGNOSTICS_FILE"
    fi
  done < <(
    jq -r '.channels[] | ["channel", .channel_id, .control_socket_path] | @tsv' "$MANIFEST"
    jq -r '.control_socket_path as $socket | .mixes[] | ["mix", .mix_id, $socket] | @tsv' "$MANIFEST"
  )
}

graph_identity() {
  local destination="$1"
  {
    printf 'core_pid %s\n' "$core_pid"
    pactl list short sources | awk '$2 ~ /^wavelinux6[-_]/ {print "source " $1 " " $2}'
    pactl list short sinks | awk '$2 ~ /^wavelinux6[-_]/ {print "sink " $1 " " $2}'
    pactl list modules short | grep -E 'wavelinux6[._-]' | sed 's/^/module /' || true
  } | LC_ALL=C sort > "$destination"
}

process_ticks() {
  local pid="$1"
  [[ -r "/proc/$pid/stat" ]] || return 1
  awk '{print $14 + $15}' "/proc/$pid/stat"
}

process_rss_kib() {
  local pid="$1"
  awk '/^VmRSS:/ {print $2; found=1} END {if (!found) print 0}' "/proc/$pid/status" 2>/dev/null || printf '0\n'
}

read_system_cpu() {
  awk '/^cpu / {idle=$5+$6; total=0; for (i=2; i<=NF; i++) total+=$i; print total, idle; exit}' /proc/stat
}

percentile_from_file() {
  local file="$1"
  local percentile="$2"
  if [[ ! -s "$file" ]]; then
    printf 'null\n'
    return
  fi
  local sorted="$file.sorted"
  LC_ALL=C sort -n "$file" > "$sorted"
  local count index
  count="$(wc -l < "$sorted")"
  index=$(((count * percentile + 99) / 100))
  sed -n "${index}p" "$sorted"
}

ENGINE_START_LINES="$(line_count "$ENGINE_LOG")"
CORE_START_LINES="$(line_count "$CORE_LOG")"
START_EPOCH="$(date +%s)"
graph_identity "$OUTPUT_DIR/graph-start.txt"
capture_core_diagnostics start

quantum="$(pw-metadata -n settings 0 2>/dev/null | sed -n "s/.*key:'clock.quantum' value:'\([0-9][0-9]*\)'.*/\1/p" | tail -n 1)"
rate="$(pw-metadata -n settings 0 2>/dev/null | sed -n "s/.*key:'clock.rate' value:'\([0-9][0-9]*\)'.*/\1/p" | tail -n 1)"
quantum="${quantum:-128}"
rate="${rate:-48000}"

jq -n \
  --arg started_at "$(date -u --date="@$START_EPOCH" +%FT%TZ)" \
  --argjson duration_sec "$DURATION_SEC" \
  --argjson cpu_workers "$CPU_WORKERS" \
  --argjson disk_mib "$DISK_MIB" \
  --argjson network_parallel "$NETWORK_PARALLEL" \
  --argjson app_pid "$app_pid" \
  --argjson core_pid "$core_pid" \
  --argjson quantum "$quantum" \
  --argjson rate "$rate" \
  --arg manifest "$MANIFEST" \
  --arg stress_channel_id "$STRESS_CHANNEL_ID" \
  --arg stress_channel_sink "$STRESS_CHANNEL_SINK" \
  '{started_at:$started_at,duration_sec:$duration_sec,cpu_workers:$cpu_workers,disk_mib:$disk_mib,network_parallel:$network_parallel,app_pid:$app_pid,core_pid:$core_pid,pipewire_quantum_frames:$quantum,pipewire_rate_hz:$rate,manifest:$manifest,stress_channel_id:$stress_channel_id,stress_channel_sink:$stress_channel_sink}' \
  > "$OUTPUT_DIR/metadata.json"

echo "WaveLinux 6 stress gate"
echo "duration=${DURATION_SEC}s cpu_workers=$CPU_WORKERS disk=${DISK_MIB}MiB network_parallel=$NETWORK_PARALLEL"
echo "output=$OUTPUT_DIR"

stress_bus_overrides_active=1
set_mix_bus_runtime monitor "$STRESS_CHANNEL_ID" \
  "$MONITOR_BUS_VOLUME" true "$MONITOR_BUS_ENABLED"
set_mix_bus_runtime stream "$STRESS_CHANNEL_ID" 1 false true
set_mix_master_runtime stream 1 false

# The child script receives values as positional arguments.
# shellcheck disable=SC2016
setsid bash -c '
  set -o pipefail
  python3 "$1" --rate 48000 --frequency "$2" --amplitude "$3" --channels 2 --channel-mode antiphase |
    paplay --raw --format=s16le --rate=48000 --channels=2 \
      --latency-msec=50 \
      --device="$4" \
      --client-name="WaveLinux 6 Stress Tone" \
      --stream-name="Continuity Fixture" \
      --property=application.id=wavelinux6-stress-tone \
      --property=application.process.binary=wavelinux6-stress-tone \
      --property=wavelinux6.managed=1 \
      --property=media.role=production
' _ "$ROOT_DIR/scripts/generate-stress-tone.py" "$TONE_FREQUENCY" "$TONE_AMPLITUDE" "$STRESS_CHANNEL_SINK" >"$TONE_LOG" 2>&1 &
tone_group_pid=$!
PROCESS_GROUPS+=("$tone_group_pid")
sleep 1
if ! kill -0 "$tone_group_pid" 2>/dev/null; then
  echo "Stress tone failed to start; see $TONE_LOG" >&2
  exit 1
fi

parec --raw --format=s16le --rate=48000 --channels=2 \
  --latency-msec=50 \
  --device=wavelinux6_mix_stream_source \
  --client-name="WaveLinux 6 Stress Recorder" \
  --stream-name="Continuity Capture" \
  --property=application.id=wavelinux6-stress-recorder \
  >"$CAPTURE_FILE" 2>"$RECORDER_LOG" &
recorder_pid=$!
BACKGROUND_PIDS+=("$recorder_pid")
RECORD_START_EPOCH="$(date +%s)"
sleep 1
if ! kill -0 "$recorder_pid" 2>/dev/null; then
  echo "Stress recorder failed to start; see $RECORDER_LOG" >&2
  exit 1
fi

for ((worker = 0; worker < CPU_WORKERS; worker++)); do
  sha256sum /dev/zero >/dev/null 2>&1 &
  BACKGROUND_PIDS+=("$!")
done

disk_blocks=$(((DISK_MIB + 3) / 4))
# The child script receives values as positional arguments.
# shellcheck disable=SC2016
setsid bash -c '
  set -e
  while :; do
    dd if=/dev/zero of="$1" bs=4M count="$2" conv=fdatasync status=none
    dd if="$1" of=/dev/null bs=4M status=none
    date +%s >> "$3"
  done
' _ "$OUTPUT_DIR/disk-load.tmp" "$disk_blocks" "$DISK_ITERATIONS_FILE" >"$OUTPUT_DIR/disk.log" 2>&1 &
PROCESS_GROUPS+=("$!")

network_port=$((52000 + ($$ % 1000)))
network_duration=$((DURATION_SEC > 4 ? DURATION_SEC - 2 : DURATION_SEC))
iperf3 -s -1 -p "$network_port" >"$OUTPUT_DIR/network-server.log" 2>&1 &
BACKGROUND_PIDS+=("$!")
sleep 0.2
iperf3 -c 127.0.0.1 -p "$network_port" -t "$network_duration" -P "$NETWORK_PARALLEL" --json >"$NETWORK_FILE" 2>"$OUTPUT_DIR/network-client.log" &
network_pid=$!
BACKGROUND_PIDS+=("$network_pid")

clock_ticks="$(getconf CLK_TCK)"
declare -A tracked_pids=(
  [wavelinux6]="$app_pid"
  [audio_core]="$core_pid"
  [pipewire]="$pipewire_pid"
  [pipewire_pulse]="$pulse_pid"
)
declare -A prior_ticks=()
for label in "${!tracked_pids[@]}"; do
  pid="${tracked_pids[$label]}"
  [[ -n "$pid" ]] && prior_ticks[$label]="$(process_ticks "$pid" || printf '0')"
done
read -r prior_system_total prior_system_idle < <(read_system_cpu)
prior_sample_ns="$(date +%s%N)"
deadline=$((SECONDS + DURATION_SEC))
tick=0
process_failure=0

while ((SECONDS < deadline)); do
  sleep 1
  tick=$((tick + 1))
  now_ns="$(date +%s%N)"
  elapsed_ns=$((now_ns - prior_sample_ns))
  read -r system_total system_idle < <(read_system_cpu)
  total_delta=$((system_total - prior_system_total))
  idle_delta=$((system_idle - prior_system_idle))
  system_cpu="$(awk -v total="$total_delta" -v idle="$idle_delta" 'BEGIN {if (total <= 0) print 0; else printf "%.3f", 100 * (total-idle) / total}')"

  sample="$(jq -cn --argjson timestamp_ms "$(date +%s%3N)" --argjson system_cpu_percent "$system_cpu" '{timestamp_ms:$timestamp_ms,system_cpu_percent:$system_cpu_percent}')"
  for label in "${!tracked_pids[@]}"; do
    pid="${tracked_pids[$label]}"
    [[ -n "$pid" ]] || continue
    if ! current_ticks="$(process_ticks "$pid")"; then
      process_failure=1
      sample="$(jq -c --arg label "$label" '.processes[$label]={alive:false}' <<<"$sample")"
      continue
    fi
    tick_delta=$((current_ticks - ${prior_ticks[$label]:-current_ticks}))
    cpu_percent="$(awk -v ticks="$tick_delta" -v hz="$clock_ticks" -v ns="$elapsed_ns" 'BEGIN {if (ns <= 0) print 0; else printf "%.3f", 100 * (ticks/hz) / (ns/1000000000)}')"
    rss_kib="$(process_rss_kib "$pid")"
    sample="$(jq -c --arg label "$label" --argjson pid "$pid" --argjson cpu "$cpu_percent" --argjson rss "$rss_kib" '.processes[$label]={alive:true,pid:$pid,cpu_percent:$cpu,rss_kib:$rss}' <<<"$sample")"
    prior_ticks[$label]="$current_ticks"
  done
  printf '%s\n' "$sample" >> "$CPU_FILE"
  prior_sample_ns="$now_ns"
  prior_system_total="$system_total"
  prior_system_idle="$system_idle"

  hardware_socket="$(jq -r '.channels[] | select(.channel_id == "hardware_in") | .control_socket_path' "$MANIFEST")"
  if [[ -S "$hardware_socket" ]]; then
    response="$(query_core_socket "$hardware_socket" channel hardware_in 2>/dev/null || true)"
    if jq -e . >/dev/null 2>&1 <<<"$response"; then
      jq -cn \
        --argjson timestamp_ms "$(date +%s%3N)" \
        --arg phase sample \
        --arg socket "$hardware_socket" \
        --argjson response "$response" \
        '{timestamp_ms:$timestamp_ms,phase:$phase,socket:$socket,response:$response}' \
        >> "$DIAGNOSTICS_FILE"
    fi
  fi
  if ((tick % 10 == 0)); then
    capture_core_diagnostics sample-all
  fi

  if ((tick == 3 || tick % 60 == 0)); then
    route_probe_bytes=$((48000 * 2 * 2 * 2))
    # The child script receives values as positional arguments.
    # shellcheck disable=SC2016
    setsid bash -c '
      set -o pipefail
      head -c "$1" /dev/zero |
        paplay --raw --format=s16le --rate=48000 --channels=2 \
          --device=@DEFAULT_SINK@ \
          --client-name=Brave \
          --stream-name=Playback \
          --property=application.id=brave \
          --property=application.process.binary=brave \
          --property=application.process.name=brave
    ' _ "$route_probe_bytes" >>"$OUTPUT_DIR/route-probe.log" 2>&1 &
    PROCESS_GROUPS+=("$!")
  fi

  if ! kill -0 "$app_pid" 2>/dev/null || ! kill -0 "$core_pid" 2>/dev/null; then
    process_failure=1
    echo "WaveLinux process exited during stress test" >&2
    break
  fi
  if ((tick % 60 == 0 || tick == DURATION_SEC)); then
    echo "stress_progress=${tick}/${DURATION_SEC}s"
  fi
done

END_EPOCH="$(date +%s)"
RECORD_EXPECTED_SEC=$((END_EPOCH - RECORD_START_EPOCH))
processes_alive_at_end=true
if ((process_failure != 0)) || ! kill -0 "$app_pid" 2>/dev/null || ! kill -0 "$core_pid" 2>/dev/null; then
  processes_alive_at_end=false
fi
capture_core_diagnostics end
graph_identity "$OUTPUT_DIR/graph-end.txt"

kill -TERM "$recorder_pid" 2>/dev/null || true
wait "$recorder_pid" 2>/dev/null || true
kill -TERM -- "-$tone_group_pid" 2>/dev/null || true
wait "$tone_group_pid" 2>/dev/null || true
stop_background_loads
BACKGROUND_PIDS=()
PROCESS_GROUPS=()

copy_log_delta "$ENGINE_LOG" "$ENGINE_START_LINES" "$ENGINE_DELTA"
copy_log_delta "$CORE_LOG" "$CORE_START_LINES" "$CORE_DELTA"
if [[ -n "${WAVELINUX_STRESS_SERVICE_LOG_DIR:-}" ]]; then
  find "$WAVELINUX_STRESS_SERVICE_LOG_DIR" -maxdepth 1 -type f \
    \( -name 'pipewire*.log' -o -name 'wireplumber*.log' \) -print0 \
    | sort -z \
    | xargs -0r cat > "$JOURNAL_FILE"
else
  journalctl --user --since "@$START_EPOCH" --until "@$END_EPOCH" --no-pager > "$JOURNAL_FILE" 2>/dev/null || :
fi
grep -Eai '(out of buffers|xrun|underrun|resync|failed to (link|activate)|link[^ ]* failure|buffer[^ ]* error)' "$JOURNAL_FILE" \
  | grep -Eai '(pipewire|wireplumber|wavelinux|pw\.)' > "$WARNING_FILE" || :

audio_analysis_ok=false
if python3 "$ROOT_DIR/scripts/analyze-stress-audio.py" \
  "$CAPTURE_FILE" \
  --rate 48000 \
  --channels 2 \
  --frequency "$TONE_FREQUENCY" \
  --amplitude "$TONE_AMPLITUDE" \
  --silence-threshold "$TONE_SILENCE_THRESHOLD" \
  --channel-mode antiphase \
  --expected-duration "$RECORD_EXPECTED_SEC" \
  > "$ANALYSIS_FILE"; then
  audio_analysis_ok=true
fi

{
  grep -oE 'estimated_event_to_route_ms=[0-9]+' "$ENGINE_DELTA" | cut -d= -f2 || :
  grep -oE '\[route[.]streams[.]fast\] moved=[0-9]+ elapsed_ms=[0-9]+' "$ENGINE_DELTA" \
    | sed 's/.*elapsed_ms=//' || :
} > "$ROUTE_TIMINGS_FILE"
route_p95="$(percentile_from_file "$ROUTE_TIMINGS_FILE" 95)"
readiness_ms="$(grep '\[repair.end\]' "$ENGINE_LOG" | sed -n 's/.*elapsed_ms=\([0-9][0-9]*\).*/\1/p' | tail -n 1)"
readiness_ms="${readiness_ms:-null}"
warning_count="$(line_count "$WARNING_FILE")"
repair_count="$(grep -Ec '\[repair\.start\]' "$ENGINE_DELTA" || true)"
engine_failure_count="$(grep -Eic 'status=error|failed=[1-9]|invalid argument|panic|fatal' "$ENGINE_DELTA" || true)"
graph_stable=false
if cmp -s "$OUTPUT_DIR/graph-start.txt" "$OUTPUT_DIR/graph-end.txt"; then
  graph_stable=true
fi

diagnostic_summary="$(jq -s '
  [.[] | select(.response.ok == true and (.response.route_id // "") != "")]
  | sort_by(.timestamp_ms)
  | group_by(.response.route_id)
  | map({
      route_id: .[0].response.route_id,
      samples: length,
      dropped_delta: ((.[-1].response.dropped_frames // 0) - (.[0].response.dropped_frames // 0)),
      underrun_delta: ((.[-1].response.underrun_frames // 0) - (.[0].response.underrun_frames // 0)),
      chain_swap_delta: ((.[-1].response.chain_swaps // 0) - (.[0].response.chain_swaps // 0)),
      non_finite_block_delta: ((.[-1].response.non_finite_blocks // 0) - (.[0].response.non_finite_blocks // 0)),
      non_finite_sample_delta: ((.[-1].response.non_finite_samples // 0) - (.[0].response.non_finite_samples // 0)),
      chain_recovery_delta: ((.[-1].response.chain_recoveries // 0) - (.[0].response.chain_recoveries // 0)),
      retired_chain_overflow_delta: ((.[-1].response.retired_chain_overflows // 0) - (.[0].response.retired_chain_overflows // 0)),
      lifetime_non_finite_effect_mask: (.[-1].response.non_finite_effect_mask // 0),
      min_target_latency_msec: (map(.response.target_latency_msec // 0) | min),
      max_target_latency_msec: (map(.response.target_latency_msec // 0) | max),
      max_lifetime_process_micros: (map(.response.max_process_micros // 0) | max),
      rt_callback_count: (.[-1].response.rt_callback_count // 0),
      rt_callback_p99_micros: (.[-1].response.rt_callback_p99_micros // 0),
      rt_callback_max_micros: (.[-1].response.rt_callback_max_micros // 0)
    })
' "$DIAGNOSTICS_FILE")"
discontinuity_delta="$(jq '[.[] | .dropped_delta + .underrun_delta] | add // 0' <<<"$diagnostic_summary")"
non_finite_block_delta="$(jq '[.[].non_finite_block_delta] | add // 0' <<<"$diagnostic_summary")"
non_finite_sample_delta="$(jq '[.[].non_finite_sample_delta] | add // 0' <<<"$diagnostic_summary")"
chain_recovery_delta="$(jq '[.[].chain_recovery_delta] | add // 0' <<<"$diagnostic_summary")"
retired_chain_overflow_delta="$(jq '[.[].retired_chain_overflow_delta] | add // 0' <<<"$diagnostic_summary")"
hardware_p99_micros="$(jq -s '
  [.[]
    | select(.response.ok == true and .response.route_id == "hardware_in")
    | .response.rt_callback_p99_micros
    | select(. != null)]
  | if length == 0 then null else .[-1] end
' "$DIAGNOSTICS_FILE")"
callback_budget_micros="$(awk -v quantum="$quantum" -v rate="$rate" 'BEGIN {printf "%.3f", 1000000 * quantum / rate * 0.25}')"
core_cpu_average="$(jq -s '[.[] | .processes.audio_core.cpu_percent? // empty] | if length == 0 then null else add / length end' "$CPU_FILE")"
system_cpu_average="$(jq -s '[.[].system_cpu_percent] | if length == 0 then null else add / length end' "$CPU_FILE")"
baseline_cpu="$(jq -r '.observed_fx_helper_cpu_percent' "$BASELINE_FILE")"
cpu_reduction_percent="$(awk -v baseline="$baseline_cpu" -v current="${core_cpu_average:-0}" 'BEGIN {if (baseline <= 0) print 0; else printf "%.3f", 100 * (baseline-current) / baseline}')"
disk_iterations="$(line_count "$DISK_ITERATIONS_FILE")"
network_ok=false
if jq -e '(.error // "") == "" and ((.end.sum_received.bits_per_second // .end.sum.bits_per_second // 0) > 0)' "$NETWORK_FILE" >/dev/null 2>&1; then
  network_ok=true
fi

public_nodes_ok=true
while IFS= read -r node_name; do
  node_count="$({ pactl list short sources; pactl list short sinks; } \
    | awk -v expected="$node_name" '$2 == expected { count++ } END { print count+0 }')"
  if [[ "$node_count" != 1 ]]; then
    public_nodes_ok=false
    break
  fi
done < <(
  {
    jq -r '.channels[].input_node_name, .channels[].output_node_name' "$MANIFEST"
    jq -r '.mixes[].output_node_name' "$MANIFEST"
  } | sort -u
)

jq -n \
  --arg completed_at "$(date -u +%FT%TZ)" \
  --arg output_dir "$OUTPUT_DIR" \
  --argjson duration_sec "$DURATION_SEC" \
  --argjson processes_alive "$processes_alive_at_end" \
  --argjson graph_stable "$graph_stable" \
  --argjson public_nodes_ok "$public_nodes_ok" \
  --argjson diagnostics "$diagnostic_summary" \
  --argjson discontinuity_delta "$discontinuity_delta" \
  --argjson non_finite_block_delta "$non_finite_block_delta" \
  --argjson non_finite_sample_delta "$non_finite_sample_delta" \
  --argjson chain_recovery_delta "$chain_recovery_delta" \
  --argjson retired_chain_overflow_delta "$retired_chain_overflow_delta" \
  --argjson callback_p99_micros "$hardware_p99_micros" \
  --argjson callback_budget_micros "$callback_budget_micros" \
  --argjson core_cpu_percent "$core_cpu_average" \
  --argjson system_cpu_percent "$system_cpu_average" \
  --argjson baseline_cpu_percent "$baseline_cpu" \
  --argjson cpu_reduction_percent "$cpu_reduction_percent" \
  --argjson route_p95_msec "$route_p95" \
  --argjson readiness_msec "$readiness_ms" \
  --argjson warning_count "$warning_count" \
  --argjson repair_count "$repair_count" \
  --argjson engine_failure_count "$engine_failure_count" \
  --argjson disk_iterations "$disk_iterations" \
  --argjson network_ok "$network_ok" \
  --argjson audio "$(cat "$ANALYSIS_FILE")" \
  --argjson audio_analysis_ok "$audio_analysis_ok" \
  '{
    completed_at:$completed_at,
    output_dir:$output_dir,
    duration_sec:$duration_sec,
    processes_alive:$processes_alive,
    graph_stable:$graph_stable,
    public_nodes_ok:$public_nodes_ok,
    audio_core:$diagnostics,
    discontinuity_delta:$discontinuity_delta,
    audio_integrity:{
      non_finite_block_delta:$non_finite_block_delta,
      non_finite_sample_delta:$non_finite_sample_delta,
      chain_recovery_delta:$chain_recovery_delta,
      retired_chain_overflow_delta:$retired_chain_overflow_delta
    },
    callback:{p99_micros:$callback_p99_micros,budget_micros:$callback_budget_micros},
    cpu:{audio_core_percent:$core_cpu_percent,system_percent:$system_cpu_percent,wavelinux5_baseline_percent:$baseline_cpu_percent,reduction_percent:$cpu_reduction_percent},
    routing:{p95_msec:$route_p95_msec},
    startup:{audio_readiness_msec:$readiness_msec},
    logs:{pipewire_warning_count:$warning_count,graph_repair_count:$repair_count,engine_failure_count:$engine_failure_count},
    load:{disk_iterations:$disk_iterations,network_ok:$network_ok},
    capture:$audio,
    gates:{
      processes_alive:$processes_alive,
      stable_graph:$graph_stable,
      public_nodes:$public_nodes_ok,
      zero_core_discontinuities:($discontinuity_delta == 0),
      zero_non_finite_audio:($non_finite_block_delta == 0 and $non_finite_sample_delta == 0),
      no_chain_recoveries:($chain_recovery_delta == 0),
      no_retired_chain_overflows:($retired_chain_overflow_delta == 0),
      callback_budget:($callback_p99_micros != null and $callback_p99_micros <= $callback_budget_micros),
      measurement_headroom:($system_cpu_percent < 95),
      cpu_reduction:($cpu_reduction_percent >= 30),
      route_latency:($route_p95_msec != null and $route_p95_msec < 100),
      startup_readiness:($readiness_msec != null and $readiness_msec < 2000),
      clean_pipewire_log:($warning_count == 0),
      no_graph_repairs:($repair_count == 0),
      clean_engine_log:($engine_failure_count == 0),
      disk_load:($disk_iterations > 0),
      network_load:$network_ok,
      capture_continuity:($audio_analysis_ok and ($audio.continuity_pass == true))
    }
  }
  | .passed = ([.gates[]] | all)
  ' > "$REPORT_FILE"

cleanup_complete=1
trap - EXIT INT TERM
cat "$REPORT_FILE"
if jq -e '.passed == true' "$REPORT_FILE" >/dev/null; then
  echo "WaveLinux 6 stress gate passed"
  exit 0
fi

echo "WaveLinux 6 stress gate failed; inspect $REPORT_FILE" >&2
exit 1
