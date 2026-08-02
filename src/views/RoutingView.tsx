import {
  Cable,
  CirclePlus,
  GitBranch,
  Pencil,
  RefreshCw,
  Trash2,
  Volume2,
  VolumeX,
} from "lucide-react";
import { createPortal } from "react-dom";
import { useCallback, useEffect, useRef, useState } from "react";
import { channelDisplayName } from "../audio-ui";
import { AppSelect } from "../components/AppSelect";
import { EmptyState, shouldCommitSliderKey } from "../components/Controls";
import {
  matcherForStream,
  matcherFromKind,
  matcherKindLabel,
  matcherKinds,
  mergeTargetsForState,
  offlineRoutingEntries,
  routeKey,
  type MatcherKind,
} from "../routing";
import { invoke } from "../tauri";
import type {
  AppMatcher,
  AppStateSnapshot,
  AppStream,
  AppVolumePreset,
} from "../types";
import {
  appVolumePercent,
  appVolumeToPercent,
  sliderPercent,
  volumeToPercent,
} from "../volume";

type RunMutation = <T>(
  command: string,
  args?: Record<string, unknown>,
  message?: string,
) => Promise<T>;

export function RoutingView({
  state,
  run,
  setAppStreamMute,
}: {
  state: AppStateSnapshot;
  run: RunMutation;
  setAppStreamMute: (streamId: string, muted: boolean) => Promise<void>;
}) {
  const [matcherKind, setMatcherKind] = useState<MatcherKind>("app_id");
  const [matcherValue, setMatcherValue] = useState("");
  const [targetChannelId, setTargetChannelId] = useState(
    state.config.channels[0]?.id ?? "",
  );

  useEffect(() => {
    if (!state.config.channels.some((channel) => channel.id === targetChannelId)) {
      setTargetChannelId(state.config.channels[0]?.id ?? "");
    }
  }, [state.config.channels, targetChannelId]);

  const addRule = async () => {
    const value = matcherValue.trim();
    if (!value || !targetChannelId) return;
    await run(
      "assign_app_to_channel",
      { channelId: targetChannelId, matcher: matcherFromKind(matcherKind, value) },
      "Routing rule saved",
    );
    setMatcherValue("");
  };
  const offlineEntries = offlineRoutingEntries(state);

  return (
    <section className="two-column routing-view">
      <div className="panel">
        <div className="panel-header">
          <h2>Active Apps</h2>
          <Cable size={18} />
        </div>
        <div className="route-list">
          {state.graph.app_streams.map((stream) => (
            <StreamRouteRow
              key={stream.id}
              state={state}
              stream={stream}
              run={run}
              setAppStreamMute={setAppStreamMute}
            />
          ))}
          {state.graph.app_streams.length === 0 && (
            <EmptyState label="No active app streams" />
          )}
        </div>
      </div>
      <div className="panel">
        <div className="panel-header">
          <h2>Offline Rules</h2>
          <GitBranch size={18} />
        </div>
        <div className="rule-editor">
          <AppSelect
            ariaLabel="Rule matcher type"
            onChange={(value) => setMatcherKind(value as MatcherKind)}
            options={matcherKinds.map((kind) => ({
              value: kind,
              label: matcherKindLabel(kind),
            }))}
            value={matcherKind}
          />
          <input
            aria-label="Rule matcher value"
            onChange={(event) => setMatcherValue(event.currentTarget.value)}
            onKeyUp={(event) => {
              if (event.key === "Enter") void addRule();
            }}
            placeholder="com.discordapp.Discord"
            type="text"
            value={matcherValue}
          />
          <AppSelect
            ariaLabel="Rule channel"
            onChange={setTargetChannelId}
            options={state.config.channels.map((channel) => ({
              value: channel.id,
              label: channelDisplayName(channel),
            }))}
            value={targetChannelId}
          />
          <button
            className="secondary-button"
            disabled={!matcherValue.trim() || !targetChannelId}
            onClick={() => void addRule()}
            type="button"
          >
            <CirclePlus size={16} />
            Rule
          </button>
        </div>
        <div className="rules-grid">
          {offlineEntries.map((entry, index) => {
            const channel = state.config.channels.find(
              (item) => item.id === entry.channel_id,
            );
            return (
              <div className="rule-row" key={`${routeKey(entry.matcher)}-${index}`}>
                <div>
                  <strong>{entry.displayName}</strong>
                  <span>{entry.meta}</span>
                </div>
                <AppSelect
                  ariaLabel={`Route ${entry.displayName} to channel`}
                  onChange={(channelId) => {
                    if (channelId) {
                      void run(
                        "assign_app_to_channel",
                        { channelId, matcher: entry.matcher },
                        "Routing rule updated",
                      );
                    } else {
                      void run(
                        "remove_app_route",
                        { matcher: entry.matcher },
                        "Routing rule removed",
                      );
                    }
                  }}
                  options={[
                    { value: "", label: "Unassigned" },
                    ...state.config.channels.map((item) => ({
                      value: item.id,
                      label: channelDisplayName(item),
                    })),
                  ]}
                  value={channel?.id ?? ""}
                />
                <OfflineVolumeControl
                  label={entry.displayName}
                  matcher={entry.matcher}
                  preset={entry.volumePreset}
                />
                <AppIdentityActions
                  label={entry.displayName}
                  matcher={entry.matcher}
                  run={run}
                  state={state}
                />
                <button
                  className="mini-icon-button danger"
                  onClick={() =>
                    void run(
                      "forget_app",
                      { matcher: entry.matcher },
                      "App forgotten",
                    ).catch(() => undefined)
                  }
                  title="Forget remembered app and clear saved route"
                  type="button"
                >
                  <Trash2 size={14} />
                </button>
              </div>
            );
          })}
          {offlineEntries.length === 0 && (
            <EmptyState label="No saved or remembered apps" />
          )}
        </div>
      </div>
    </section>
  );
}

export function OfflineVolumeControl({
  label,
  matcher,
  preset,
}: {
  label: string;
  matcher: AppMatcher;
  preset?: AppVolumePreset;
}) {
  const [draft, setDraft] = useState(volumeToPercent(preset?.volume ?? 1));
  const lastCommitted = useRef(draft);

  useEffect(() => {
    const next = volumeToPercent(preset?.volume ?? 1);
    setDraft(next);
    lastCommitted.current = next;
  }, [preset?.volume]);

  const commit = useCallback(
    (nextValue = draft) => {
      const next = sliderPercent(nextValue);
      setDraft(next);
      if (lastCommitted.current === next) return;
      lastCommitted.current = next;
      void invoke("set_app_volume_preset", { matcher, volume: next / 100 }).catch(
        () => undefined,
      );
    },
    [draft, matcher],
  );

  return (
    <label className="route-volume-control" title="Offline app volume preset">
      <Volume2 size={14} />
      <input
        aria-label={`${label} saved volume`}
        max={100}
        min={0}
        onBlur={(event) => commit(Number(event.currentTarget.value))}
        onChange={(event) => setDraft(sliderPercent(Number(event.currentTarget.value)))}
        onKeyUp={(event) => {
          if (shouldCommitSliderKey(event)) commit(Number(event.currentTarget.value));
        }}
        onPointerUp={(event) => commit(Number(event.currentTarget.value))}
        type="range"
        value={draft}
      />
      <strong>{draft}</strong>
    </label>
  );
}

function StreamRouteRow({
  state,
  stream,
  run,
  setAppStreamMute,
}: {
  state: AppStateSnapshot;
  stream: AppStream;
  run: RunMutation;
  setAppStreamMute: (streamId: string, muted: boolean) => Promise<void>;
}) {
  const [draftVolume, setDraftVolume] = useState(appVolumeToPercent(stream.volume));
  const [draftRoute, setDraftRoute] = useState(stream.routed_channel_id ?? "");
  const lastCommitted = useRef(draftVolume);
  const volumeApplyInFlight = useRef(false);
  const queuedVolume = useRef<number | null>(null);
  const presetSaveTimer = useRef<ReturnType<typeof window.setTimeout> | null>(null);
  const optimisticVolumeUntil = useRef(0);
  const optimisticRouteUntil = useRef(0);

  useEffect(() => {
    if (
      volumeApplyInFlight.current ||
      queuedVolume.current !== null ||
      Date.now() < optimisticVolumeUntil.current
    ) {
      return;
    }
    const next = appVolumeToPercent(stream.volume);
    setDraftVolume(next);
    lastCommitted.current = next;
  }, [stream.volume]);

  useEffect(() => {
    if (Date.now() < optimisticRouteUntil.current) return;
    setDraftRoute(stream.routed_channel_id ?? "");
  }, [stream.routed_channel_id]);

  useEffect(() => {
    return () => {
      if (presetSaveTimer.current !== null) {
        window.clearTimeout(presetSaveTimer.current);
      }
    };
  }, []);

  const flushQueuedVolume = useCallback(() => {
    if (volumeApplyInFlight.current) return;
    const next = queuedVolume.current;
    if (next === null) return;
    queuedVolume.current = null;
    volumeApplyInFlight.current = true;
    optimisticVolumeUntil.current = Date.now() + 1500;
    void invoke("set_app_stream_volume", {
      streamId: stream.id,
      volume: next / 100,
    })
      .catch(() => undefined)
      .finally(() => {
        volumeApplyInFlight.current = false;
        if (queuedVolume.current !== null) {
          flushQueuedVolume();
        } else {
          optimisticVolumeUntil.current = Date.now() + 750;
        }
      });
  }, [stream.id]);

  const commitVolume = useCallback(
    (nextValue = draftVolume) => {
      const next = appVolumePercent(nextValue);
      setDraftVolume(next);
      if (lastCommitted.current === next) return;
      lastCommitted.current = next;
      optimisticVolumeUntil.current = Date.now() + 1500;
      queuedVolume.current = next;
      flushQueuedVolume();
      const volume = next / 100;
      if (presetSaveTimer.current !== null) {
        window.clearTimeout(presetSaveTimer.current);
      }
      presetSaveTimer.current = window.setTimeout(() => {
        void invoke("set_app_volume_preset", {
          matcher: matcherForStream(stream),
          volume,
        }).catch(() => undefined);
      }, 250);
    },
    [draftVolume, flushQueuedVolume, stream],
  );

  const routeStream = async (channelId: string) => {
    setDraftRoute(channelId);
    optimisticRouteUntil.current = Date.now() + 1500;
    if (!channelId) {
      const matcher = matcherForStream(stream);
      await invoke("remove_app_route", { matcher });
      await invoke("move_app_stream_to_default", { streamId: stream.id });
      optimisticRouteUntil.current = Date.now() + 750;
      return;
    }
    await invoke("move_app_stream", { streamId: stream.id, channelId });
    await invoke("assign_app_to_channel", {
      channelId,
      matcher: matcherForStream(stream),
    });
    optimisticRouteUntil.current = Date.now() + 750;
  };

  return (
    <div className="route-row">
      <div>
        <strong>{stream.display_name}</strong>
        <span>{stream.media_name ?? stream.process_name ?? stream.id}</span>
      </div>
      <AppSelect
        ariaLabel={`Route ${stream.display_name} to channel`}
        onChange={(value) =>
          void routeStream(value).catch(() =>
            setDraftRoute(stream.routed_channel_id ?? ""),
          )
        }
        options={[
          { value: "", label: "Unassigned" },
          ...state.config.channels.map((channel) => ({
            value: channel.id,
            label: channelDisplayName(channel),
          })),
        ]}
        value={draftRoute}
      />
      <label className="route-volume-control" title="App stream volume">
        <Volume2 size={14} />
        <input
          aria-label={`${stream.display_name} volume`}
          max={100}
          min={1}
          onBlur={(event) => commitVolume(Number(event.currentTarget.value))}
          onChange={(event) =>
            setDraftVolume(appVolumePercent(Number(event.currentTarget.value)))
          }
          onKeyUp={(event) => {
            if (shouldCommitSliderKey(event)) {
              commitVolume(Number(event.currentTarget.value));
            }
          }}
          onPointerUp={(event) => commitVolume(Number(event.currentTarget.value))}
          type="range"
          value={draftVolume}
        />
        <strong>{draftVolume}</strong>
      </label>
      <AppIdentityActions
        label={stream.display_name}
        matcher={matcherForStream(stream)}
        run={run}
        state={state}
      />
      <button
        className={stream.muted ? "icon-button danger active" : "icon-button"}
        onClick={() => void setAppStreamMute(stream.id, !stream.muted).catch(() => undefined)}
        title="Mute app"
        type="button"
      >
        {stream.muted ? <VolumeX size={17} /> : <Volume2 size={17} />}
      </button>
    </div>
  );
}

function AppIdentityActions({
  label,
  matcher,
  run,
  state,
}: {
  label: string;
  matcher: AppMatcher;
  run: RunMutation;
  state: AppStateSnapshot;
}) {
  const [mergeOpen, setMergeOpen] = useState(false);
  const mergeButtonRef = useRef<HTMLButtonElement | null>(null);
  const [mergePosition, setMergePosition] = useState<{
    left: number;
    top: number;
  } | null>(null);
  const mergeTargets = mergeTargetsForState(state, matcher);

  const updateMergePosition = useCallback(() => {
    const rect = mergeButtonRef.current?.getBoundingClientRect();
    if (!rect) return;
    const width = Math.min(360, Math.max(260, window.innerWidth - 24));
    const maxLeft = Math.max(12, window.innerWidth - width - 12);
    const left = Math.min(maxLeft, Math.max(12, rect.right - width));
    const below = rect.bottom + 8;
    const maxTop = Math.max(12, window.innerHeight - 332);
    const top = Math.min(maxTop, Math.max(12, below));
    setMergePosition({ left, top });
  }, []);

  useEffect(() => {
    if (!mergeOpen) return;
    const close = () => setMergeOpen(false);
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMergeOpen(false);
    };
    const onLayout = () => updateMergePosition();
    updateMergePosition();
    window.addEventListener("click", close);
    window.addEventListener("keydown", onKey);
    window.addEventListener("resize", onLayout);
    window.addEventListener("scroll", onLayout, true);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", onLayout);
      window.removeEventListener("scroll", onLayout, true);
    };
  }, [mergeOpen, updateMergePosition]);

  return (
    <div className="identity-actions">
      <button
        className="mini-icon-button"
        onClick={() => {
          const next = window.prompt("Pinned app label", label);
          if (next?.trim()) {
            void run(
              "pin_app_identity",
              { matcher, label: next.trim() },
              "App identity pinned",
            );
          }
        }}
        title="Pin or rename app identity"
        type="button"
      >
        <Pencil size={14} />
      </button>
      <button
        className="mini-icon-button"
        disabled={mergeTargets.length === 0}
        ref={mergeButtonRef}
        onClick={(event) => {
          event.stopPropagation();
          setMergeOpen((open) => {
            if (open) return false;
            updateMergePosition();
            return true;
          });
        }}
        title="Merge into remembered app"
        type="button"
      >
        <GitBranch size={14} />
      </button>
      {mergeOpen &&
        mergePosition &&
        createPortal(
          <div
            className="identity-merge-popover"
            onClick={(event) => event.stopPropagation()}
            style={{ left: mergePosition.left, top: mergePosition.top }}
          >
            <strong>Merge Into</strong>
            <div className="identity-merge-list">
              {mergeTargets.map((target) => (
                <button
                  key={routeKey(target.matcher)}
                  onClick={() => {
                    setMergeOpen(false);
                    void run(
                      "merge_app_identity",
                      { source: matcher, target: target.matcher },
                      "App identities merged",
                    ).catch(() => undefined);
                  }}
                  type="button"
                >
                  <span>{target.displayName}</span>
                  <small>{target.meta}</small>
                </button>
              ))}
            </div>
          </div>,
          document.body,
        )}
      <button
        className="mini-icon-button"
        onClick={() =>
          void run("reset_app_identity", { matcher }, "App identity reset").catch(
            () => undefined,
          )
        }
        title="Reset app identity"
        type="button"
      >
        <RefreshCw size={14} />
      </button>
    </div>
  );
}
