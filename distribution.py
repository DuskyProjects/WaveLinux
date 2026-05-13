"""Distribution/runtime helpers for source, packaged, and AppImage runs."""

from __future__ import annotations

from dataclasses import dataclass
import os
import shlex
import shutil
import stat
import subprocess
import sys

APP_NAME = "WaveLinux"
APP_DESKTOP_ID = "io.github.duskyprojects.WaveLinux"
APPIMAGE_FILENAME = "WaveLinux.AppImage"
WRAPPER_FILENAME = "wavelinux"
DESKTOP_FILENAME = f"{APP_DESKTOP_ID}.desktop"
ICON_FILENAME = "wavelinux.png"


@dataclass(frozen=True)
class AppImageInstallResult:
    appimage_path: str
    desktop_path: str
    icon_path: str
    wrapper_path: str


@dataclass(frozen=True)
class LauncherEntry:
    path: str
    exec_command: str | None
    exec_target: str | None
    is_canonical: bool


@dataclass(frozen=True)
class InstallState:
    running_appimage_path: str | None
    installed_appimage_path: str
    installed_appimage_exists: bool
    appimage_missing: bool
    wrapper_path: str
    wrapper_exists: bool
    wrapper_target: str | None
    wrapper_mismatch: bool
    desktop_path: str
    desktop_exists: bool
    desktop_exec_command: str | None
    desktop_exec_target: str | None
    desktop_mismatch: bool
    launcher_entries: tuple[LauncherEntry, ...]
    stale_launcher_entries: tuple[LauncherEntry, ...]
    warnings: tuple[str, ...]


@dataclass(frozen=True)
class LauncherRepairResult:
    appimage_path: str
    wrapper_path: str
    desktop_path: str
    removed_entries: tuple[str, ...]


@dataclass(frozen=True)
class RuntimeMode:
    kind: str
    running_path: str
    allows_self_update: bool
    update_channel: str


def app_root() -> str:
    """Return the directory that contains bundled resources."""
    if getattr(sys, "frozen", False):
        bundle_dir = getattr(sys, "_MEIPASS", None)
        if bundle_dir:
            return os.path.abspath(bundle_dir)
        return os.path.dirname(os.path.abspath(sys.executable))
    return os.path.dirname(os.path.abspath(__file__))


def resource_path(*parts: str) -> str:
    return os.path.join(app_root(), *parts)


def current_runtime_path(*, environ=None, argv=None, executable=None,
                         frozen=None) -> str:
    appimage = current_appimage_path(environ=environ, argv=argv)
    if appimage:
        return appimage
    is_frozen = bool(getattr(sys, "frozen", False) if frozen is None else frozen)
    if is_frozen:
        path = executable or sys.executable
        return os.path.abspath(path)
    args = argv if argv is not None else sys.argv
    if args:
        return os.path.abspath(args[0])
    return resource_path("main.py")


def current_appimage_path(*, environ=None, argv=None) -> str | None:
    env = environ if environ is not None else os.environ
    path = env.get("APPIMAGE")
    if path:
        return os.path.abspath(path)
    args = argv if argv is not None else sys.argv
    if args:
        candidate = os.path.abspath(args[0])
        if candidate.endswith(".AppImage") and os.path.exists(candidate):
            return candidate
    return None


def is_running_in_appimage(*, environ=None, argv=None) -> bool:
    return current_appimage_path(environ=environ, argv=argv) is not None


def runtime_mode(*, home=None, environ=None, argv=None, executable=None,
                 frozen=None) -> RuntimeMode:
    running_path = current_runtime_path(
        environ=environ,
        argv=argv,
        executable=executable,
        frozen=frozen,
    )
    if current_appimage_path(environ=environ, argv=argv):
        return RuntimeMode(
            kind="appimage",
            running_path=running_path,
            allows_self_update=True,
            update_channel="appimage",
        )
    is_frozen = bool(getattr(sys, "frozen", False) if frozen is None else frozen)
    if not is_frozen:
        return RuntimeMode(
            kind="source",
            running_path=running_path,
            allows_self_update=True,
            update_channel="appimage",
        )

    home_dir = os.path.expanduser("~") if home is None else os.path.abspath(home)
    normalized = os.path.abspath(running_path)
    user_prefixes = (
        os.path.join(home_dir, ".local"),
        home_dir,
    )
    if normalized.startswith(tuple(os.path.abspath(prefix) + os.sep for prefix in user_prefixes)):
        return RuntimeMode(
            kind="bundle",
            running_path=normalized,
            allows_self_update=True,
            update_channel="appimage",
        )
    return RuntimeMode(
        kind="package",
        running_path=normalized,
        allows_self_update=False,
        update_channel="package-manager",
    )


def installed_appimage_path(*, home=None) -> str:
    home_dir = os.path.expanduser("~") if home is None else os.path.abspath(home)
    return os.path.join(home_dir, ".local", "bin", APPIMAGE_FILENAME)


def installed_wrapper_path(*, home=None) -> str:
    home_dir = os.path.expanduser("~") if home is None else os.path.abspath(home)
    return os.path.join(home_dir, ".local", "bin", WRAPPER_FILENAME)


def installed_desktop_path(*, home=None) -> str:
    home_dir = os.path.expanduser("~") if home is None else os.path.abspath(home)
    return os.path.join(home_dir, ".local", "share", "applications", DESKTOP_FILENAME)


def installed_icon_path(*, home=None) -> str:
    home_dir = os.path.expanduser("~") if home is None else os.path.abspath(home)
    return os.path.join(
        home_dir,
        ".local",
        "share",
        "icons",
        "hicolor",
        "512x512",
        "apps",
        ICON_FILENAME,
    )


def _read_text(path: str) -> str:
    try:
        with open(path, "r", encoding="utf-8") as handle:
            return handle.read()
    except OSError:
        return ""


def _desktop_exec_command(path: str) -> str | None:
    for raw_line in _read_text(path).splitlines():
        line = raw_line.strip()
        if line.startswith("Exec="):
            value = line.split("=", 1)[1].strip()
            return value or None
    return None


def _command_target(command: str | None) -> str | None:
    if not command:
        return None
    try:
        parts = shlex.split(command)
    except ValueError:
        return None
    if not parts:
        return None
    candidate = parts[0]
    if os.path.isabs(candidate):
        return os.path.abspath(candidate)
    return candidate


def _wrapper_target(path: str) -> str | None:
    for raw_line in _read_text(path).splitlines():
        line = raw_line.strip()
        if not line.startswith("exec "):
            continue
        return _command_target(line[len("exec "):].strip())
    return None


def _is_wavelinux_entry(path: str, exec_command: str | None) -> bool:
    basename = os.path.basename(path).lower()
    if "wavelinux" in basename:
        return True
    text = _read_text(path).lower()
    if "name=wavelinux" in text:
        return True
    if exec_command and "wavelinux" in exec_command.lower():
        return True
    return False


def _launcher_entries(*, home=None) -> list[LauncherEntry]:
    home_dir = os.path.expanduser("~") if home is None else os.path.abspath(home)
    apps_dir = os.path.join(home_dir, ".local", "share", "applications")
    canonical = installed_desktop_path(home=home_dir)
    try:
        names = sorted(os.listdir(apps_dir))
    except OSError:
        return []
    entries = []
    for name in names:
        if not name.endswith(".desktop"):
            continue
        path = os.path.join(apps_dir, name)
        exec_command = _desktop_exec_command(path)
        if not _is_wavelinux_entry(path, exec_command):
            continue
        entries.append(LauncherEntry(
            path=path,
            exec_command=exec_command,
            exec_target=_command_target(exec_command),
            is_canonical=(os.path.abspath(path) == os.path.abspath(canonical)),
        ))
    return entries


def _write_installed_appimage_launchers(target_appimage: str, *, home_dir: str) -> tuple[str, str]:
    target_wrapper = installed_wrapper_path(home=home_dir)
    target_desktop = installed_desktop_path(home=home_dir)
    os.makedirs(os.path.dirname(target_wrapper), exist_ok=True)
    os.makedirs(os.path.dirname(target_desktop), exist_ok=True)

    wrapper_contents = (
        "#!/bin/sh\n"
        "# Auto-generated by WaveLinux AppImage install.\n"
        f'exec "{target_appimage}" "$@"\n'
    )
    with open(target_wrapper, "w", encoding="utf-8") as handle:
        handle.write(wrapper_contents)
    os.chmod(
        target_wrapper,
        stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR
        | stat.S_IRGRP | stat.S_IXGRP
        | stat.S_IROTH | stat.S_IXOTH,
    )

    desktop_contents = (
        "[Desktop Entry]\n"
        "Name=WaveLinux\n"
        "Comment=PipeWire Audio Router & Mixer\n"
        f"Exec={target_wrapper}\n"
        "Icon=wavelinux\n"
        "Type=Application\n"
        "Categories=AudioVideo;Audio;Mixer;\n"
        "Keywords=audio;mixer;pipewire;routing;\n"
        "StartupNotify=true\n"
    )
    with open(target_desktop, "w", encoding="utf-8") as handle:
        handle.write(desktop_contents)
    return target_wrapper, target_desktop


def _refresh_desktop_database(applications_dir: str):
    try:
        subprocess.run(
            ["update-desktop-database", applications_dir],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=5,
            check=False,
        )
    except Exception:
        pass


def launch_command(*, home=None) -> list[str]:
    """Return the launch command for the current runtime mode."""
    appimage = current_appimage_path()
    if appimage:
        return [appimage]
    if getattr(sys, "frozen", False):
        return [os.path.abspath(sys.executable)]
    return [sys.executable, resource_path("main.py")]


def launch_command_string(*, home=None) -> str:
    return " ".join(shlex.quote(part) for part in launch_command(home=home))


def desktop_exec_command(*, home=None) -> str:
    """Return a Desktop Entry-safe Exec= command."""
    def _quote(part: str) -> str:
        if part and all(ch not in part for ch in ' \t"\'\\'):
            return part
        escaped = part.replace("\\", "\\\\").replace('"', '\\"')
        return f'"{escaped}"'

    return " ".join(_quote(part) for part in launch_command(home=home))


def install_state(*, home=None, environ=None, argv=None) -> InstallState:
    home_dir = os.path.expanduser("~") if home is None else os.path.abspath(home)
    running = current_appimage_path(environ=environ, argv=argv)
    installed_appimage = installed_appimage_path(home=home_dir)
    wrapper = installed_wrapper_path(home=home_dir)
    desktop = installed_desktop_path(home=home_dir)

    launcher_entries = tuple(_launcher_entries(home=home_dir))
    desktop_exec = _desktop_exec_command(desktop) if os.path.exists(desktop) else None
    desktop_target = _command_target(desktop_exec)
    wrapper_target = _wrapper_target(wrapper) if os.path.exists(wrapper) else None
    appimage_exists = os.path.exists(installed_appimage)
    appimage_missing = not appimage_exists
    wrapper_mismatch = (
        os.path.exists(wrapper)
        and wrapper_target not in {None, os.path.abspath(installed_appimage)}
    )
    desktop_mismatch = (
        os.path.exists(desktop)
        and desktop_target not in {
            None,
            os.path.abspath(wrapper),
            os.path.abspath(installed_appimage),
            os.path.basename(wrapper),
            os.path.basename(installed_appimage),
        }
    )
    stale_entries = tuple(
        entry for entry in launcher_entries
        if not entry.is_canonical
    )

    warnings = []
    if wrapper_mismatch:
        warnings.append("Installed wrapper points at an unexpected AppImage path.")
    if desktop_mismatch:
        warnings.append("Installed desktop launcher points at an unexpected target.")
    if stale_entries:
        warnings.append(
            f"Found {len(stale_entries)} extra WaveLinux desktop launcher(s) in ~/.local/share/applications."
        )

    return InstallState(
        running_appimage_path=running,
        installed_appimage_path=installed_appimage,
        installed_appimage_exists=appimage_exists,
        appimage_missing=appimage_missing,
        wrapper_path=wrapper,
        wrapper_exists=os.path.exists(wrapper),
        wrapper_target=wrapper_target,
        wrapper_mismatch=wrapper_mismatch,
        desktop_path=desktop,
        desktop_exists=os.path.exists(desktop),
        desktop_exec_command=desktop_exec,
        desktop_exec_target=desktop_target,
        desktop_mismatch=desktop_mismatch,
        launcher_entries=launcher_entries,
        stale_launcher_entries=stale_entries,
        warnings=tuple(warnings),
    )


def repair_installed_appimage_launchers(*, home=None) -> LauncherRepairResult:
    home_dir = os.path.expanduser("~") if home is None else os.path.abspath(home)
    state = install_state(home=home_dir)
    if not state.installed_appimage_exists:
        raise RuntimeError(
            f"No installed AppImage found at {state.installed_appimage_path}."
        )

    removed = []
    for entry in state.stale_launcher_entries:
        try:
            os.remove(entry.path)
            removed.append(entry.path)
        except OSError:
            continue

    wrapper_path, desktop_path = _write_installed_appimage_launchers(
        state.installed_appimage_path,
        home_dir=home_dir,
    )
    _refresh_desktop_database(os.path.dirname(desktop_path))

    return LauncherRepairResult(
        appimage_path=state.installed_appimage_path,
        wrapper_path=wrapper_path,
        desktop_path=desktop_path,
        removed_entries=tuple(removed),
    )


def install_appimage_file(source: str, *, home=None) -> AppImageInstallResult:
    source = os.path.abspath(str(source or ""))
    if not os.path.isfile(source):
        raise RuntimeError(f"AppImage not found at {source}.")

    home_dir = os.path.expanduser("~") if home is None else os.path.abspath(home)
    target_appimage = installed_appimage_path(home=home_dir)
    target_icon = installed_icon_path(home=home_dir)

    os.makedirs(os.path.dirname(target_appimage), exist_ok=True)
    os.makedirs(os.path.dirname(target_icon), exist_ok=True)

    if os.path.abspath(source) != os.path.abspath(target_appimage):
        tmp_target = target_appimage + ".tmp"
        try:
            shutil.copy2(source, tmp_target)
            os.chmod(
                tmp_target,
                stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR
                | stat.S_IRGRP | stat.S_IXGRP
                | stat.S_IROTH | stat.S_IXOTH,
            )
            os.replace(tmp_target, target_appimage)
        finally:
            if os.path.exists(tmp_target):
                try:
                    os.remove(tmp_target)
                except OSError:
                    pass
    os.chmod(
        target_appimage,
        stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR
        | stat.S_IRGRP | stat.S_IXGRP
        | stat.S_IROTH | stat.S_IXOTH,
    )

    icon_source = resource_path("icon.png")
    if os.path.exists(icon_source):
        shutil.copy2(icon_source, target_icon)
    target_wrapper, target_desktop = _write_installed_appimage_launchers(
        target_appimage,
        home_dir=home_dir,
    )
    _refresh_desktop_database(os.path.dirname(target_desktop))

    return AppImageInstallResult(
        appimage_path=target_appimage,
        desktop_path=target_desktop,
        icon_path=target_icon,
        wrapper_path=target_wrapper,
    )


def install_current_appimage(*, home=None) -> AppImageInstallResult:
    source = current_appimage_path()
    if not source:
        raise RuntimeError("WaveLinux is not running from an AppImage.")
    return install_appimage_file(source, home=home)
