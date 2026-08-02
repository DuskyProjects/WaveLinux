import type {
  AppMatcher,
  AppStateSnapshot,
  AppStream,
  AppVolumePreset,
} from "./types";

export const matcherKinds = [
  "app_id",
  "process_name",
  "binary",
  "window_class",
  "media_name",
] as const;

export type MatcherKind = (typeof matcherKinds)[number];

export type OfflineRoutingEntry = {
  matcher: AppMatcher;
  displayName: string;
  meta: string;
  channel_id?: string;
  volumePreset?: AppVolumePreset;
};

export type MergeTarget = {
  matcher: AppMatcher;
  displayName: string;
  meta: string;
};

export function matcherForStream(stream: AppStream): AppMatcher {
  const keepMediaName = shouldKeepStreamMediaName(stream);
  const matcher = {
    app_id: stream.app_id ?? null,
    binary: stream.binary ?? null,
    process_name: stream.process_name ?? null,
    window_class: stream.window_class ?? null,
    media_name: keepMediaName ? (stream.media_name ?? null) : null,
  };
  if (!matcherIsEmpty(matcher)) return matcher;

  return {
    app_id: fallbackMatcherValueForStream(stream),
    binary: null,
    process_name: null,
    window_class: null,
  };
}

export function matcherFromKind(kind: MatcherKind, value: string): AppMatcher {
  const cleaned = value.trim();
  return {
    app_id: kind === "app_id" ? cleaned : null,
    binary: kind === "binary" ? cleaned : null,
    process_name: kind === "process_name" ? cleaned : null,
    window_class: kind === "window_class" ? cleaned : null,
    media_name: kind === "media_name" ? cleaned : null,
  };
}

export function matcherKindLabel(kind: MatcherKind): string {
  return {
    app_id: "App ID",
    process_name: "Process",
    binary: "Binary",
    window_class: "Window Class",
    media_name: "Media Name",
  }[kind];
}

export function routeKey(matcher: AppMatcher): string {
  const entries = matcherEntries(matcher);
  if (entries.length === 0) return "empty";
  return entries
    .map(([kind, value]) => `${kind}:${normalizedMatcherValue(value)}`)
    .join("|");
}

export function mergeTargetsForState(
  state: AppStateSnapshot,
  source: AppMatcher,
): MergeTarget[] {
  const sourceKey = routeKey(source);
  const targets = new Map<string, MergeTarget>();

  for (const stream of state.graph.app_streams) {
    const matcher = matcherForStream(stream);
    const key = routeKey(matcher);
    if (key === sourceKey || !isMergeableAppTarget(stream.display_name, matcher)) continue;
    targets.set(key, {
      matcher,
      displayName: stream.display_name || matcherLabel(matcher),
      meta: "Active app",
    });
  }

  for (const entry of offlineRoutingEntries(state)) {
    const key = routeKey(entry.matcher);
    if (
      key === sourceKey ||
      targets.has(key) ||
      !isMergeableAppTarget(entry.displayName, entry.matcher)
    ) {
      continue;
    }
    targets.set(key, {
      matcher: entry.matcher,
      displayName: entry.displayName,
      meta: entry.meta || "Offline app",
    });
  }

  return [...targets.values()].sort((left, right) =>
    left.displayName.localeCompare(right.displayName),
  );
}

export function offlineRoutingEntries(state: AppStateSnapshot): OfflineRoutingEntry[] {
  const entries = new Map<string, OfflineRoutingEntry>();
  const activeMatchers = state.graph.app_streams.map((stream) => matcherForStream(stream));
  for (const app of state.config.app_history ?? []) {
    if (app.forgotten || matcherIsActive(app.matcher, activeMatchers)) continue;
    const key = routeKey(app.matcher);
    entries.set(key, {
      matcher: app.matcher,
      displayName: app.display_name || matcherLabel(app.matcher),
      meta: [matcherTypeLabel(app.matcher), formatLastSeen(app.last_seen_unix)]
        .filter(Boolean)
        .join(" · "),
      channel_id: undefined,
      volumePreset: volumePresetForMatcher(state.config.app_volume_presets, app.matcher),
    });
  }

  for (const route of state.config.app_routes) {
    if (matcherIsActive(route.matcher, activeMatchers)) continue;
    const key = routeKey(route.matcher);
    const existing = entries.get(key);
    entries.set(key, {
      matcher: route.matcher,
      displayName: existing?.displayName ?? matcherLabel(route.matcher),
      meta: existing?.meta ?? matcherTypeLabel(route.matcher),
      channel_id: route.channel_id,
      volumePreset:
        existing?.volumePreset ??
        volumePresetForMatcher(state.config.app_volume_presets, route.matcher),
    });
  }

  return [...entries.values()].sort((left, right) => {
    const leftRouted = left.channel_id ? 0 : 1;
    const rightRouted = right.channel_id ? 0 : 1;
    return leftRouted - rightRouted || left.displayName.localeCompare(right.displayName);
  });
}

function shouldKeepStreamMediaName(stream: AppStream): boolean {
  const mediaName = stream.media_name?.trim();
  if (!mediaName || isGenericMediaName(mediaName)) return false;

  const identityValues = [
    stream.app_id,
    stream.binary,
    stream.process_name,
    stream.window_class,
  ]
    .map((value) => value?.trim().toLowerCase())
    .filter((value): value is string => Boolean(value));

  if (identityValues.length === 0) return true;

  const wrapperNeedles = [
    "ferdium",
    "electron",
    "chromium",
    "chrome",
    "brave",
    "vivaldi",
    "webapp",
    "web-app",
  ];
  return identityValues.some((value) =>
    wrapperNeedles.some((needle) => value.includes(needle)),
  );
}

function isGenericMediaName(value: string): boolean {
  return ["audio-src", "audio src", "audio", "playback", "output", "input"].includes(
    value.trim().toLowerCase(),
  );
}

function matcherIsEmpty(matcher: AppMatcher): boolean {
  return matcherKinds.every((kind) => !matcher[kind]?.trim());
}

function fallbackMatcherValueForStream(stream: AppStream): string {
  const candidates = [
    stream.display_name && !/^Stream\s+\d+$/i.test(stream.display_name)
      ? stream.display_name
      : null,
    stream.media_name && !isGenericMediaName(stream.media_name) ? stream.media_name : null,
    stream.id ? `stream-${stream.id}` : null,
  ];
  const value = candidates
    .map((candidate) => candidate?.trim())
    .find((candidate): candidate is string => Boolean(candidate));
  return `stream:${value ?? "unknown"}`;
}

function matcherEntries(matcher: AppMatcher): Array<[MatcherKind, string]> {
  return matcherKinds
    .map((kind) => [kind, matcher[kind]?.trim() ?? ""] as [MatcherKind, string])
    .filter(([, value]) => value.length > 0);
}

function matcherLabel(matcher: AppMatcher): string {
  return matcherEntries(matcher)[0]?.[1] ?? "Unknown app";
}

function matcherTypeLabel(matcher: AppMatcher): string {
  const entries = matcherEntries(matcher);
  if (entries.length === 0) return "No matcher";
  return entries.map(([kind]) => matcherKindLabel(kind)).join(" + ");
}

function normalizedMatcherValue(value: string): string {
  return value.trim().toLowerCase();
}

function matchersOverlap(left: AppMatcher, right: AppMatcher): boolean {
  if (routeKey(left) === routeKey(right)) return true;
  const rightEntries = new Map(
    matcherEntries(right).map(([kind, value]) => [kind, normalizedMatcherValue(value)]),
  );
  return matcherEntries(left).some(([kind, value]) => {
    const rightValue = rightEntries.get(kind);
    return Boolean(rightValue && rightValue === normalizedMatcherValue(value));
  });
}

function matcherIsActive(matcher: AppMatcher, activeMatchers: AppMatcher[]): boolean {
  return activeMatchers.some((activeMatcher) => matchersOverlap(activeMatcher, matcher));
}

function volumePresetForMatcher(
  presets: AppVolumePreset[] | undefined,
  matcher: AppMatcher,
): AppVolumePreset | undefined {
  const key = routeKey(matcher);
  return presets?.find((preset) => routeKey(preset.matcher) === key);
}

function isMergeableAppTarget(displayName: string, matcher: AppMatcher): boolean {
  if (routeKey(matcher) === "empty") return false;
  const haystack = [displayName, ...matcherEntries(matcher).map(([, value]) => value)]
    .join("\n")
    .toLowerCase();
  const blocked = [
    "wavelinux",
    "pipewire",
    "wireplumber",
    "libcanberra",
    "pw-play",
    "pw-cat",
    "paplay",
    "wavelinux-route-test",
  ];
  return !blocked.some((needle) => haystack.includes(needle));
}

function formatLastSeen(lastSeenUnix: number): string {
  if (!Number.isFinite(lastSeenUnix) || lastSeenUnix <= 0) return "";
  const elapsedSeconds = Math.max(0, Math.round(Date.now() / 1000 - lastSeenUnix));
  if (elapsedSeconds < 120) return "Seen now";
  const minutes = Math.round(elapsedSeconds / 60);
  if (minutes < 60) return `Seen ${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `Seen ${hours}h ago`;
  return `Seen ${Math.round(hours / 24)}d ago`;
}
