#!/usr/bin/env bash

# Process discovery shared by installers. Match executable identity instead of
# command text so an installer, terminal, or diagnostic command mentioning a
# WaveLinux filename can never be mistaken for the running application.

wavelinux_list_user_pids() {
  if [[ -n "${WAVELINUX_PROCESS_PID_FILE:-}" ]]; then
    cat "$WAVELINUX_PROCESS_PID_FILE"
    return
  fi

  ps -u "${WAVELINUX_PROCESS_UID:-$(id -u)}" -o pid=
}

wavelinux_executable_basename() {
  local pid="$1"
  local proc_root="${WAVELINUX_PROCESS_PROC_ROOT:-/proc}"
  local executable
  executable="$(readlink "$proc_root/$pid/exe" 2>/dev/null || true)"
  executable="${executable% (deleted)}"
  printf '%s\n' "${executable##*/}"
}

wavelinux_collect_process_pids() {
  local role="$1"
  local pid executable_name

  while read -r pid; do
    [[ "$pid" =~ ^[0-9]+$ ]] || continue
    executable_name="$(wavelinux_executable_basename "$pid")"
    case "$role:$executable_name" in
      app:wavelinux6|app:WaveLinux6_*_amd64.AppImage)
        printf '%s\n' "$pid"
        ;;
      app-runtime:wavelinux6)
        printf '%s\n' "$pid"
        ;;
      legacy-app:wavelinux5|legacy-app:WaveLinux5_*_amd64.AppImage)
        printf '%s\n' "$pid"
        ;;
      audio-core:wavelinux6-audio-core)
        printf '%s\n' "$pid"
        ;;
      legacy-helper:wavelinux5-dsp-helper)
        printf '%s\n' "$pid"
        ;;
      peripheral:wavelinux6-peripheral-plugin)
        printf '%s\n' "$pid"
        ;;
    esac
  done < <(wavelinux_list_user_pids)
}

wavelinux_collect_filter_chain_pids() {
  wavelinux_collect_owned_filter_chain_pids wavelinux6 wavelinux6-chain-
}

wavelinux_collect_legacy_filter_chain_pids() {
  wavelinux_collect_owned_filter_chain_pids wavelinux5 wavelinux5-chain-
}

wavelinux_collect_owned_filter_chain_pids() {
  local owner_dir="$1"
  local config_prefix="$2"
  local proc_root="${WAVELINUX_PROCESS_PROC_ROOT:-/proc}"
  local pid executable_name index
  local arguments=()

  while read -r pid; do
    [[ "$pid" =~ ^[0-9]+$ ]] || continue
    executable_name="$(wavelinux_executable_basename "$pid")"
    [[ "$executable_name" == "pipewire" ]] || continue
    [[ -r "$proc_root/$pid/cmdline" ]] || continue

    arguments=()
    mapfile -d '' -t arguments <"$proc_root/$pid/cmdline" || true
    for ((index = 0; index + 1 < ${#arguments[@]}; index++)); do
      if [[ "${arguments[index]}" == "-c" \
        && "${arguments[index + 1]}" == */"$owner_dir"/effects/"$config_prefix"* ]]; then
        printf '%s\n' "$pid"
        break
      fi
    done
  done < <(wavelinux_list_user_pids)
}
