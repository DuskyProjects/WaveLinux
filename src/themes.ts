import type { CSSProperties } from "react";

export type ThemeSurface = "wavelink2" | "wavelink3";
export type ThemeVariant = "light" | "dark" | "custom";

export interface UiThemeDefinition {
  id: string;
  name: string;
  surface: ThemeSurface;
  variant: ThemeVariant;
  tokens: Record<string, string>;
  builtin?: boolean;
}

const THEME_ID_STORAGE_KEY = "wavelinux.ui.themeId.v1";
const LEGACY_THEME_ALIASES: Record<string, string> = {
  classic: "wavelink2",
  wavelink: "wavelink3",
  wavelink_dark: "wavelink3_dark",
};
const BUILTIN_THEME_IDS = new Set([
  "wavelink2",
  "wavelink3",
  "wavelink3_dark",
  ...Object.keys(LEGACY_THEME_ALIASES),
]);

export const builtInUiThemes: UiThemeDefinition[] = [
  {
    id: "wavelink2",
    name: "WaveLinux Original (Wave Link 2-style)",
    surface: "wavelink2",
    variant: "dark",
    tokens: {},
    builtin: true,
  },
  {
    id: "wavelink3",
    name: "Wave Link 3-style Matrix",
    surface: "wavelink3",
    variant: "light",
    tokens: {},
    builtin: true,
  },
  {
    id: "wavelink3_dark",
    name: "Wave Link 3-style Matrix Dark",
    surface: "wavelink3",
    variant: "dark",
    tokens: {},
    builtin: true,
  },
];

export function loadStoredThemeId(): string {
  if (typeof window === "undefined") return "wavelink2";
  try {
    return normalizeThemeId(window.localStorage.getItem(THEME_ID_STORAGE_KEY) || "wavelink2");
  } catch {
    return "wavelink2";
  }
}

export function saveStoredThemeId(themeId: string) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(THEME_ID_STORAGE_KEY, normalizeThemeId(themeId));
  } catch {
    // The Tauri app-shell preference file is the durable fallback.
  }
}

export function allUiThemes(customThemes: UiThemeDefinition[]): UiThemeDefinition[] {
  return [...builtInUiThemes, ...customThemes];
}

export function resolveUiTheme(themeId: string, customThemes: UiThemeDefinition[]): UiThemeDefinition {
  const normalizedThemeId = normalizeThemeId(themeId);
  return allUiThemes(customThemes).find((theme) => theme.id === normalizedThemeId) ?? builtInUiThemes[0];
}

export function normalizeFileUiThemes(value: unknown): UiThemeDefinition[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set(BUILTIN_THEME_IDS);
  return value.flatMap((theme) => {
    try {
      const normalized = normalizeTheme(theme);
      if (seen.has(normalized.id)) return [];
      seen.add(normalized.id);
      return [normalized];
    } catch {
      return [];
    }
  });
}

export function themeToStyle(theme: UiThemeDefinition): CSSProperties {
  return theme.tokens as CSSProperties;
}

function normalizeTheme(value: unknown): UiThemeDefinition {
  if (!isRecord(value)) throw new Error("Theme must be a JSON object");
  const id = cleanId(value.id);
  if (BUILTIN_THEME_IDS.has(id)) throw new Error("Custom theme id cannot replace a built-in theme");
  const name = cleanString(value.name, "Theme name");
  const surface = normalizeThemeSurface(value.surface);
  const variant =
    value.variant === "dark" || value.variant === "custom" || value.variant === "light"
      ? value.variant
      : "custom";
  const tokens = normalizeTokens(value.tokens);
  return { id, name, surface, variant, tokens, builtin: false };
}

function normalizeThemeId(themeId: string): string {
  return LEGACY_THEME_ALIASES[themeId] ?? themeId;
}

function normalizeThemeSurface(value: unknown): ThemeSurface {
  if (value === "wavelink2" || value === "classic") return "wavelink2";
  if (value === "wavelink3" || value === "wavelink") return "wavelink3";
  throw new Error("Theme surface must be wavelink2 or wavelink3");
}

function normalizeTokens(value: unknown): Record<string, string> {
  if (value === undefined) return {};
  if (!isRecord(value)) throw new Error("Theme tokens must be an object");
  const tokens: Record<string, string> = {};
  for (const [key, tokenValue] of Object.entries(value)) {
    if (!/^--wl-[a-z0-9-]+$/.test(key)) {
      throw new Error(`Unsupported token name: ${key}`);
    }
    if (typeof tokenValue !== "string" || tokenValue.length > 120) {
      throw new Error(`Theme token ${key} must be a short string`);
    }
    tokens[key] = tokenValue;
  }
  return tokens;
}

function cleanId(value: unknown): string {
  if (typeof value !== "string" || !/^[a-z0-9][a-z0-9_-]{1,40}$/.test(value)) {
    throw new Error("Theme id must use lowercase letters, numbers, dashes, or underscores");
  }
  return value;
}

function cleanString(value: unknown, label: string): string {
  if (typeof value !== "string") throw new Error(`${label} must be a string`);
  const cleaned = value.trim();
  if (!cleaned) throw new Error(`${label} is required`);
  return cleaned.slice(0, 80);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
