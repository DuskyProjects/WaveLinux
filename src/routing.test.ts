import { describe, expect, it } from "vitest";
import {
  matcherForStream,
  matcherFromKind,
  matcherKindLabel,
  offlineRoutingEntries,
  routeKey,
} from "./routing";
import type { AppStream } from "./types";
import { demoState } from "./demo";

function stream(overrides: Partial<AppStream> = {}): AppStream {
  return {
    id: "42",
    display_name: "Browser",
    volume: 1,
    muted: false,
    ...overrides,
  };
}

describe("routing identity helpers", () => {
  it("keeps media names for browser wrappers but not ordinary native apps", () => {
    expect(
      matcherForStream(
        stream({ app_id: "com.brave.Browser", media_name: "YouTube Music" }),
      ).media_name,
    ).toBe("YouTube Music");
    expect(
      matcherForStream(
        stream({ app_id: "org.videolan.VLC", media_name: "Current track" }),
      ).media_name,
    ).toBeNull();
  });

  it("creates a stable non-empty identity when PipeWire supplies no app metadata", () => {
    const matcher = matcherForStream(stream({ display_name: "Stream 42" }));
    expect(matcher.app_id).toBe("stream:stream-42");
    expect(routeKey(matcher)).toBe("app_id:stream:stream-42");
  });

  it("normalizes rule keys without changing the saved matcher value", () => {
    const matcher = matcherFromKind("window_class", "  Discord  ");
    expect(matcher.window_class).toBe("Discord");
    expect(routeKey(matcher)).toBe("window_class:discord");
    expect(matcherKindLabel("window_class")).toBe("Window Class");
  });

  it("keeps an offline browser app visible while a different media app is active", () => {
    const state = structuredClone(demoState);
    const active = stream({ app_id: "com.brave.Browser", media_name: "YouTube Music" });
    state.graph.app_streams = [active];
    state.config.app_history = [];
    state.config.app_routes = [
      { matcher: { app_id: "com.brave.Browser", media_name: "Discord" }, channel_id: "chat" },
      { matcher: { app_id: "com.brave.Browser", media_name: "YouTube Music" }, channel_id: "music" },
    ];
    expect(offlineRoutingEntries(state).map((entry) => entry.channel_id)).toEqual(["chat"]);
  });

  it("requires every saved matcher field while allowing broad rules and binary fallback", () => {
    const state = structuredClone(demoState);
    state.graph.app_streams = [stream({ app_id: "active", process_name: "player" })];
    state.config.app_history = [];
    state.config.app_routes = [
      { matcher: { app_id: "other", process_name: "player" }, channel_id: "chat" },
      { matcher: { app_id: "active" }, channel_id: "music" },
      { matcher: { binary: "PLAYER" }, channel_id: "game" },
    ];
    expect(offlineRoutingEntries(state).map((entry) => entry.channel_id)).toEqual(["chat"]);
    state.graph.app_streams = [stream({ process_name: "player" })];
    expect(offlineRoutingEntries(state).map((entry) => entry.channel_id).sort()).toEqual(["chat", "music"]);
  });

  it.each(["Stream", "Stream 42", " Audio-src "])("ignores generic media label %s", (media_name) => {
    expect(matcherForStream(stream({ app_id: "com.brave.Browser", media_name })).media_name).toBeNull();
  });
});
