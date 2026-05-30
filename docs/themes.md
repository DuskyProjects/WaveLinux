# WaveLinux UI Themes

WaveLinux UI themes are editable JSON files loaded by the frontend. They are
intentionally separate from the Rust audio engine: a theme can choose a shipped
UI surface and override visual tokens, but it cannot run code, create Tauri
commands, change PipeWire routing behavior, or add proprietary integrations.

This keeps user-made themes easy to share while keeping the audio graph and
desktop permissions predictable.

## What A Theme Can Do

- Select a UI surface: `wavelink2` or `wavelink3`.
- Select a base variant: `light`, `dark`, or `custom`.
- Override WaveLinux CSS tokens such as app background, text, panels, borders,
  accent colors, danger colors, and effect LED colors.
- Appear in Settings > General > Interface after the file is loaded.
- Persist as the selected interface after the user chooses it.

## What A Theme Cannot Do

- Execute JavaScript, shell commands, Rust code, or Tauri commands.
- Change the backend mixer model, PipeWire graph, hardware profiles, effect
  definitions, or update behavior.
- Replace built-in theme ids such as `wavelink2`, `wavelink3`, or
  `wavelink3_dark`.
- Ship Elgato logos, proprietary Marketplace effects, Stream Deck integration,
  Clipguard controls, or vendor-specific hardware panels.

## Built-In Themes

WaveLinux ships these built-in choices:

- `wavelink2`: WaveLinux Original, the older WaveLinux/Wave Link 2-style
  mixer surface.
- `wavelink3`: Wave Link 3-style Matrix, the newer light matrix workflow.
- `wavelink3_dark`: Wave Link 3-style Matrix Dark.

Legacy ids are accepted for migration:

- `classic` resolves to `wavelink2`.
- `wavelink` resolves to `wavelink3`.
- `wavelink_dark` resolves to `wavelink3_dark`.

## Theme Folder

The easiest way to find the theme folder is inside the app:

1. Open Settings.
2. Go to General.
3. Find Interface.
4. Press Folder.
5. Add one `.json` file per custom theme.
6. Press Refresh or restart WaveLinux.

On current Linux desktop builds the folder is usually:

```bash
~/.config/io.github.duskyprojects.WaveLinux/themes
```

The selected theme preference is stored by the app shell next to that config
directory and is also mirrored in frontend local storage as a fast startup
fallback.

## Minimal Theme

Save this as `my-matrix-dark.json` in the theme folder:

```json
{
  "id": "my_matrix_dark",
  "name": "My Matrix Dark",
  "surface": "wavelink3",
  "variant": "dark",
  "tokens": {
    "--wl-bg": "#10141b",
    "--wl-surface": "#171f28",
    "--wl-panel": "#1b2530",
    "--wl-panel-alt": "#121821",
    "--wl-border": "#2e3a45",
    "--wl-text": "#eef5f8",
    "--wl-muted": "#97a8b5",
    "--wl-accent": "#0b8ea4",
    "--wl-accent-soft": "#15323a",
    "--wl-danger": "#d75f6b",
    "--wl-led-on": "#56d78b"
  }
}
```

After Refresh, the selector shows `My Matrix Dark (custom)`.

## File Format

Each file must be a JSON object:

```json
{
  "id": "theme_id",
  "name": "Theme Name",
  "surface": "wavelink3",
  "variant": "dark",
  "tokens": {}
}
```

Fields:

- `id`: required. Use lowercase letters, numbers, dashes, or underscores. The
  id must start with a lowercase letter or number and must be 2 to 41
  characters long.
- `name`: required. Display name shown in the Interface selector.
- `surface`: required. Use `wavelink2` for the original surface or `wavelink3`
  for the matrix mixer surface. `classic` and `wavelink` are accepted aliases.
- `variant`: optional-ish. Use `light`, `dark`, or `custom`. Invalid values are
  treated as `custom`.
- `tokens`: optional. A map of CSS custom properties. Token names must start
  with `--wl-`, and token values must be strings shorter than 121 characters.

Invalid files are ignored instead of blocking app startup.

## Surface Choice

Use `wavelink2` when you want the older WaveLinux layout with its classic
source strips and existing workflow.

Use `wavelink3` when you want the matrix mixer workflow with source rows, mix
columns, send cells, mix headers, routing drawer, FX drawer, Settings,
Profiles, and Health.

Custom theme files currently style and select shipped surfaces. If WaveLinux
adds third-party layout plugins later, they should stay frontend-only and keep
the same boundary from the Rust audio engine.

## Token Reference

Common tokens:

- `--wl-bg`: app background.
- `--wl-surface`: primary cards, cells, and controls.
- `--wl-panel`: main panel surfaces.
- `--wl-panel-alt`: headers and alternate surfaces.
- `--wl-border`: panel and cell borders.
- `--wl-text`: primary text.
- `--wl-muted`: secondary text and subtle meter backgrounds.
- `--wl-accent`: selected controls, primary buttons, meter fill, and highlights.
- `--wl-accent-soft`: selected row and navigation background.
- `--wl-danger`: destructive controls and muted send tint.
- `--wl-led-on`: active effect LED color.

Most themes can get a coherent look by setting only those tokens. Unknown
`--wl-*` tokens are accepted, so future frontend CSS can opt into additional
theme values without changing the file format.

## Light Theme Example

```json
{
  "id": "clean_control_room",
  "name": "Clean Control Room",
  "surface": "wavelink3",
  "variant": "light",
  "tokens": {
    "--wl-bg": "#eef2f5",
    "--wl-surface": "#ffffff",
    "--wl-panel": "#f8fafc",
    "--wl-panel-alt": "#edf3f7",
    "--wl-border": "#c9d5de",
    "--wl-text": "#111820",
    "--wl-muted": "#6e7f8f",
    "--wl-accent": "#087f95",
    "--wl-accent-soft": "#dff6fa",
    "--wl-danger": "#c8344f",
    "--wl-led-on": "#2fba72"
  }
}
```

## Original Surface Example

```json
{
  "id": "classic_blue",
  "name": "Classic Blue",
  "surface": "wavelink2",
  "variant": "custom",
  "tokens": {
    "--wl-bg": "#101923",
    "--wl-surface": "#182536",
    "--wl-panel": "#1f3044",
    "--wl-border": "#38516a",
    "--wl-text": "#f4f8fb",
    "--wl-muted": "#9eb0c1",
    "--wl-accent": "#28a9cc",
    "--wl-accent-soft": "#163947",
    "--wl-danger": "#ef6b7a",
    "--wl-led-on": "#5ee38f"
  }
}
```

## Authoring Checklist

- Keep filenames descriptive, for example `clean-control-room.json`.
- Use a unique `id`; built-in ids are reserved.
- Test both Start Audio idle state and active audio state.
- Check Settings, Routing, Effects, and the Matrix drawer after changing colors.
- Make sure text has enough contrast on panel and button backgrounds.
- Avoid putting secrets or local paths in theme files; they are plain JSON and
  are easy to share.

## Troubleshooting

If a theme does not show up:

- Confirm the file extension is `.json`.
- Confirm the file is in the app theme folder, not the hardware profile folder.
- Confirm `id`, `name`, and `surface` are present.
- Confirm the `id` is not a built-in id.
- Confirm every token key starts with `--wl-`.
- Press Refresh in Settings > General > Interface or restart the app.

If colors look partly unchanged, the surface may be using a component-specific
style that does not read the token you changed yet. Prefer the common tokens
above first; then add a narrowly named `--wl-*` token only when the frontend CSS
has been taught to use it.
