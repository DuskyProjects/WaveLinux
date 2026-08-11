#!/usr/bin/env bash

# Canonical host runtime packages for the WaveLinux AppImage and standalone
# installer. Native package metadata is checked against these capabilities by
# scripts/check-package-contents.sh.

WAVELINUX_APT_RUNTIME_PACKAGES=(
  gawk
  procps
  pipewire
  pipewire-audio
  pipewire-pulse
  pipewire-bin
  wireplumber
  pulseaudio-utils
  alsa-utils
  libasound2-plugins
  rtkit
  libusb-1.0-0
  bubblewrap
  xdg-dbus-proxy
  xwayland
  libegl1
  libgl1
  libgbm1
  libdrm2
  libwayland-client0
  libwayland-cursor0
  libwayland-egl1
  libwayland-server0
  fontconfig
  fonts-dejavu-core
  xdg-desktop-portal
)

WAVELINUX_DNF_RUNTIME_PACKAGES=(
  gawk
  procps-ng
  pipewire
  pipewire-utils
  pipewire-alsa
  pipewire-pulseaudio
  wireplumber
  pulseaudio-utils
  alsa-utils
  alsa-plugins-pulseaudio
  rtkit
  libusb1
  bubblewrap
  xdg-dbus-proxy
  xorg-x11-server-Xwayland
  mesa-libEGL
  mesa-libGL
  mesa-libgbm
  libdrm
  libwayland-client
  libwayland-cursor
  libwayland-egl
  libwayland-server
  fontconfig
  google-noto-sans-fonts
  xdg-desktop-portal
)

WAVELINUX_PACMAN_RUNTIME_PACKAGES=(
  gawk
  procps-ng
  pipewire
  pipewire-audio
  pipewire-alsa
  pipewire-pulse
  wireplumber
  libpulse
  alsa-utils
  alsa-plugins
  rtkit
  libusb
  bubblewrap
  xdg-dbus-proxy
  xorg-xwayland
  mesa
  libglvnd
  wayland
  libdrm
  fontconfig
  noto-fonts
  xdg-desktop-portal
)

WAVELINUX_ZYPPER_RUNTIME_PACKAGES=(
  gawk
  procps
  pipewire
  pipewire-alsa
  pipewire-pulseaudio
  wireplumber
  pulseaudio-utils
  alsa
  rtkit
  libusb-1_0-0
  bubblewrap
  xdg-dbus-proxy
  xwayland
  Mesa-libEGL1
  Mesa-libGL1
  libgbm1
  libdrm2
  libwayland-client0
  libwayland-cursor0
  libwayland-egl1
  libwayland-server0
  fontconfig
  google-noto-sans-fonts
  xdg-desktop-portal
)

wavelinux_runtime_packages() {
  local manager="$1"
  local packages=()
  case "$manager" in
    apt) packages=("${WAVELINUX_APT_RUNTIME_PACKAGES[@]}") ;;
    dnf) packages=("${WAVELINUX_DNF_RUNTIME_PACKAGES[@]}") ;;
    pacman) packages=("${WAVELINUX_PACMAN_RUNTIME_PACKAGES[@]}") ;;
    zypper) packages=("${WAVELINUX_ZYPPER_RUNTIME_PACKAGES[@]}") ;;
    *) return 0 ;;
  esac
  ((${#packages[@]})) && printf '%s\n' "${packages[@]}"
}

wavelinux_privilege_helper_order() {
  local terminal="$1"
  local sudo_available="$2"
  local pkexec_available="$3"

  if [[ "$terminal" == 1 ]]; then
    [[ "$sudo_available" == 1 ]] && printf '%s\n' sudo
    [[ "$pkexec_available" == 1 ]] && printf '%s\n' pkexec
  else
    [[ "$pkexec_available" == 1 ]] && printf '%s\n' pkexec
    [[ "$sudo_available" == 1 ]] && printf '%s\n' sudo
  fi
}

wavelinux_portal_candidates() {
  local manager="$1"
  local desktop="${XDG_CURRENT_DESKTOP:-${DESKTOP_SESSION:-}}"
  desktop="${desktop,,}"

  case "$manager:$desktop" in
    apt:*kde*|apt:*plasma*)
      printf '%s\n' xdg-desktop-portal-kde xdg-desktop-portal-gtk
      ;;
    apt:*gnome*)
      printf '%s\n' xdg-desktop-portal-gnome xdg-desktop-portal-gtk
      ;;
    apt:*)
      printf '%s\n' xdg-desktop-portal-gtk xdg-desktop-portal-kde xdg-desktop-portal-gnome
      ;;
    dnf:*kde*|dnf:*plasma*)
      printf '%s\n' xdg-desktop-portal-kde xdg-desktop-portal-gtk
      ;;
    dnf:*gnome*)
      printf '%s\n' xdg-desktop-portal-gnome xdg-desktop-portal-gtk
      ;;
    dnf:*hypr*)
      printf '%s\n' xdg-desktop-portal-hyprland xdg-desktop-portal-gtk
      ;;
    dnf:*)
      printf '%s\n' xdg-desktop-portal-gtk xdg-desktop-portal-kde xdg-desktop-portal-gnome
      ;;
    pacman:*kde*|pacman:*plasma*|pacman:*cachy*)
      printf '%s\n' xdg-desktop-portal-kde xdg-desktop-portal-gtk
      ;;
    pacman:*gnome*)
      printf '%s\n' xdg-desktop-portal-gnome xdg-desktop-portal-gtk
      ;;
    pacman:*hypr*)
      printf '%s\n' xdg-desktop-portal-hyprland xdg-desktop-portal-gtk
      ;;
    pacman:*sway*|pacman:*wlroots*)
      printf '%s\n' xdg-desktop-portal-wlr xdg-desktop-portal-gtk
      ;;
    pacman:*)
      printf '%s\n' xdg-desktop-portal-gtk xdg-desktop-portal-kde xdg-desktop-portal-gnome
      ;;
    zypper:*kde*|zypper:*plasma*)
      printf '%s\n' xdg-desktop-portal-kde xdg-desktop-portal-gtk
      ;;
    zypper:*gnome*)
      printf '%s\n' xdg-desktop-portal-gnome xdg-desktop-portal-gtk
      ;;
    zypper:*)
      printf '%s\n' xdg-desktop-portal-gtk xdg-desktop-portal-kde xdg-desktop-portal-gnome
      ;;
  esac
}
