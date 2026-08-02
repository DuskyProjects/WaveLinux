#!/usr/bin/env bash
set -euo pipefail

# Mesa/EGL loads host graphics drivers at runtime. Bundling an older Wayland
# client ABI beside those host drivers can make WebKit's render process abort
# with EGL_BAD_PARAMETER and leave an otherwise healthy Tauri window blank.
MODE="sanitize"

usage() {
  cat <<'HELP'
Remove or check Wayland libraries that must not be bundled in WaveLinux AppImages.

Usage:
  bash scripts/sanitize-appimage-wayland.sh [--sanitize|--check] TARGET...

TARGET may be a WaveLinux AppDir or a WaveLinux AppImage. AppImages are extracted
to a temporary directory for checking.
HELP
}

while (($#)); do
  case "$1" in
    --sanitize)
      MODE="sanitize"
      ;;
    --check)
      MODE="check"
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    --)
      shift
      break
      ;;
    -*)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
    *)
      break
      ;;
  esac
  shift
done

if (($# == 0)); then
  usage >&2
  exit 1
fi

is_path_present() {
  [[ -e "$1" || -L "$1" ]]
}

library_roots() {
  local root="$1"
  local dir
  for dir in \
    "$root/usr/lib" \
    "$root/usr/lib64" \
    "$root/usr/lib32" \
    "$root/lib" \
    "$root/lib64" \
    "$root/lib32" \
    "$root/usr/lib/x86_64-linux-gnu" \
    "$root/usr/lib/aarch64-linux-gnu" \
    "$root/usr/lib/arm-linux-gnueabihf"; do
    [[ -d "$dir" ]] && printf '%s\n' "$dir"
  done
}

forbidden_paths() {
  local root="$1"
  local libdir prefix entry
  while IFS= read -r libdir; do
    [[ -d "$libdir" ]] || continue
    for prefix in \
      libwayland-client.so \
      libwayland-cursor.so \
      libwayland-egl.so \
      libwayland-server.so; do
      for entry in "$libdir"/"$prefix"*; do
        is_path_present "$entry" && printf '%s\n' "$entry"
      done
    done
  done < <(library_roots "$root")
}

check_root() {
  local root="$1"
  mapfile -t found < <(forbidden_paths "$root")
  if ((${#found[@]} == 0)); then
    echo "AppImage Wayland bundle check: ok ($root)"
    return 0
  fi

  echo "AppImage Wayland bundle check failed for $root:" >&2
  printf '  %s\n' "${found[@]}" >&2
  return 1
}

sanitize_root() {
  local root="$1"
  mapfile -t found < <(forbidden_paths "$root")
  if ((${#found[@]} == 0)); then
    echo "AppImage Wayland sanitizer: nothing to remove ($root)"
    return 0
  fi

  printf 'Removing bundled Wayland client artifact: %s\n' "${found[@]}"
  rm -f -- "${found[@]}"
  check_root "$root"
}

with_appimage_root() {
  local appimage="$1"
  local tmp status
  tmp="$(mktemp -d)"
  (
    cd "$tmp"
    "$appimage" --appimage-extract >/dev/null
  )
  if [[ ! -d "$tmp/squashfs-root" ]]; then
    echo "Failed to extract AppImage: $appimage" >&2
    rm -rf "$tmp"
    return 1
  fi

  check_root "$tmp/squashfs-root"
  status=$?
  rm -rf "$tmp"
  return "$status"
}

status=0
for target in "$@"; do
  if [[ -d "$target" ]]; then
    if [[ "$MODE" == "sanitize" ]]; then
      sanitize_root "$target" || status=1
    else
      check_root "$target" || status=1
    fi
  elif [[ -f "$target" ]]; then
    if [[ "$MODE" == "sanitize" ]]; then
      echo "Cannot sanitize sealed AppImage directly: $target" >&2
      status=1
    else
      with_appimage_root "$(realpath "$target")" || status=1
    fi
  else
    echo "No such AppDir or AppImage: $target" >&2
    status=1
  fi
done

exit "$status"
