import { describe, expect, it } from "vitest";
import {
  matcherForStream,
  matcherFromKind,
  matcherKindLabel,
  routeKey,
} from "./routing";
import type { AppStream } from "./types";

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
});
