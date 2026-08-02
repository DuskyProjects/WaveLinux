import {
  Check,
  CircleAlert,
  CirclePlus,
  SlidersHorizontal,
  Sparkles,
} from "lucide-react";
import { useMemo } from "react";
import {
  autoMicrophoneLabel,
  channelDisplayName,
  isHardwareChannel,
  sortedMicrophoneInputs,
} from "../audio-ui";
import { AppSelect } from "../components/AppSelect";
import { EmptyState } from "../components/Controls";
import { EffectBlock } from "../components/EffectBlock";
import { EffectStateButton } from "../components/EffectStateButton";
import { effectRuntimeForChannel, effectRuntimeTitle } from "../effect-runtime";
import {
  useEffectChainEditor,
  type SetEffectChain,
} from "../hooks/useEffectChainEditor";
import type {
  AppStateSnapshot,
  Channel,
  DeviceInfo,
  EffectDefinition,
} from "../types";

type EffectEditor = ReturnType<typeof useEffectChainEditor>;

export function WaveLinkEffectsEditor({
  channel,
  className,
  setChannelInput,
  setEffectChain,
  state,
}: {
  channel?: Channel;
  className?: string;
  setChannelInput: (channelId: string, sourceDevice: string | null) => Promise<void>;
  setEffectChain: SetEffectChain;
  state: AppStateSnapshot;
}) {
  const microphoneInputs = useMemo(
    () => sortedMicrophoneInputs(state.graph.inputs),
    [state.graph.inputs],
  );
  const editor = useEffectChainEditor(state, channel, setEffectChain);

  return (
    <div className={["wl-effects-editor-stack", className].filter(Boolean).join(" ")}>
      <div className="panel wl-effect-chain-panel">
        <EffectPanelHeader
          addEffect={editor.addEffect}
          catalogEffects={editor.catalogEffects}
          channel={channel}
          title={channel ? channelDisplayName(channel) : "Effects"}
        />
        <EffectEditorStatus error={editor.effectError} pending={editor.pending} />
        <HardwareSourceCard
          channel={channel}
          inputs={microphoneInputs}
          setChannelInput={setChannelInput}
          state={state}
        />
        <EffectChainList editor={editor} state={state} />
      </div>
      <EffectCatalog
        channel={channel}
        className="panel catalog-panel wl-effects-catalog-panel"
        editor={editor}
        state={state}
      />
    </div>
  );
}

export function EffectsView({
  state,
  selectedChannel,
  selectedChannelId,
  setSelectedChannelId,
  setChannelInput,
  setEffectChain,
  setChannelEffectsEnabled,
}: {
  state: AppStateSnapshot;
  selectedChannel?: Channel;
  selectedChannelId: string;
  setSelectedChannelId: (channelId: string) => void;
  setChannelInput: (channelId: string, sourceDevice: string | null) => Promise<void>;
  setEffectChain: SetEffectChain;
  setChannelEffectsEnabled: (channelId: string, enabled: boolean) => Promise<void>;
}) {
  const microphoneInputs = useMemo(
    () => sortedMicrophoneInputs(state.graph.inputs),
    [state.graph.inputs],
  );
  const editor = useEffectChainEditor(state, selectedChannel, setEffectChain);

  return (
    <section className="two-column effects-view">
      <div className="panel">
        <div className="panel-header">
          <h2>Channel</h2>
          <SlidersHorizontal size={18} />
        </div>
        <div className="channel-picker">
          {state.config.channels.map((channel) => {
            const displayChannel = {
              ...channel,
              effects: editor.effectsForChannel(channel),
            };
            const runtime = effectRuntimeForChannel(state.engine.effects, channel.id);
            const effectTitle = effectRuntimeTitle(displayChannel, runtime);
            return (
              <div
                className={
                  channel.id === selectedChannelId ? "picker-row active" : "picker-row"
                }
                key={channel.id}
                title={`${channelDisplayName(channel)} · ${effectTitle}`}
              >
                <button
                  className="picker-row-select"
                  onClick={() => setSelectedChannelId(channel.id)}
                  type="button"
                >
                  <span>{channelDisplayName(channel)}</span>
                </button>
                <EffectStateButton
                  channel={displayChannel}
                  onToggle={(enabled) =>
                    setChannelEffectsEnabled(channel.id, enabled)
                  }
                  runtime={runtime}
                />
              </div>
            );
          })}
        </div>
      </div>
      <div className="panel">
        <EffectPanelHeader
          addEffect={editor.addEffect}
          catalogEffects={editor.catalogEffects}
          channel={selectedChannel}
          title={selectedChannel ? channelDisplayName(selectedChannel) : "Effects"}
        />
        <EffectEditorStatus error={editor.effectError} pending={editor.pending} />
        <HardwareSourceCard
          channel={selectedChannel}
          inputs={microphoneInputs}
          setChannelInput={setChannelInput}
          state={state}
        />
        <EffectChainList editor={editor} state={state} />
      </div>
      <EffectCatalog
        channel={selectedChannel}
        className="panel catalog-panel"
        editor={editor}
        state={state}
      />
    </section>
  );
}

function EffectPanelHeader({
  addEffect,
  catalogEffects,
  channel,
  title,
}: {
  addEffect: (effect: EffectDefinition) => void;
  catalogEffects: EffectDefinition[];
  channel?: Channel;
  title: string;
}) {
  return (
    <div className="panel-header">
      <h2>{title}</h2>
      <div className="panel-actions">
        <button
          className="secondary-button"
          disabled={!channel || catalogEffects.length === 0}
          onClick={() => {
            if (catalogEffects[0]) addEffect(catalogEffects[0]);
          }}
          type="button"
        >
          <CirclePlus size={16} />
          Add
        </button>
      </div>
    </div>
  );
}

function EffectEditorStatus({
  error,
  pending,
}: {
  error: string | null;
  pending: boolean;
}) {
  return (
    <>
      {pending && <div className="effect-sync-status">Syncing effect chain...</div>}
      {error && (
        <div className="effect-warning">
          <CircleAlert size={15} />
          <span>{error}</span>
        </div>
      )}
    </>
  );
}

function HardwareSourceCard({
  channel,
  inputs,
  setChannelInput,
  state,
}: {
  channel?: Channel;
  inputs: DeviceInfo[];
  setChannelInput: (channelId: string, sourceDevice: string | null) => Promise<void>;
  state: AppStateSnapshot;
}) {
  if (!channel || !isHardwareChannel(channel)) return null;
  const fieldId = `effects-microphone-source-${channel.id}`;
  return (
    <div className="hardware-source-card">
      <label className="field-label" htmlFor={fieldId}>
        Microphone
      </label>
      <AppSelect
        ariaLabel="Microphone"
        id={fieldId}
        onChange={(value) =>
          void setChannelInput(channel.id, value || null).catch(() => undefined)
        }
        options={[
          {
            value: "",
            label: autoMicrophoneLabel(
              inputs,
              "Auto hardware input",
              state.graph.auto_devices,
              channel.id,
            ),
          },
          ...inputs.map((input) => ({
            value: input.id,
            label: input.description,
          })),
        ]}
        value={channel.source_device ?? ""}
      />
      <div className="field-label">Input mode</div>
      <div className="static-field">Mono</div>
    </div>
  );
}

function EffectChainList({
  editor,
  state,
}: {
  editor: EffectEditor;
  state: AppStateSnapshot;
}) {
  return (
    <div className="effect-chain">
      {editor.selectedEffects.map((effect, index) => {
        const definition = state.catalog.effects.find(
          (item) => item.id === effect.effect_id,
        );
        return (
          <EffectBlock
            availability={state.graph.effect_availability.find(
              (item) => item.effect_id === effect.effect_id,
            )}
            definition={definition}
            effect={effect}
            index={index}
            key={effect.instance_id}
            onApplyPreset={editor.applyPreset}
            onDelete={editor.deleteEffect}
            onMove={editor.moveEffect}
            onRename={editor.renameEffect}
            onUpdateParam={editor.updateEffectParam}
            total={editor.selectedEffects.length}
          />
        );
      })}
      {editor.selectedEffects.length === 0 && (
        <EmptyState label="No effects on this channel" />
      )}
    </div>
  );
}

function EffectCatalog({
  channel,
  className,
  editor,
  state,
}: {
  channel?: Channel;
  className: string;
  editor: EffectEditor;
  state: AppStateSnapshot;
}) {
  return (
    <div className={className}>
      <div className="panel-header">
        <h2>Catalog</h2>
        <Sparkles size={18} />
      </div>
      <div className="catalog-grid">
        {editor.catalogEffects.map((effect) => {
          const availability = state.graph.effect_availability.find(
            (item) => item.effect_id === effect.id,
          );
          const isEnabled = editor.selectedEffects.some(
            (item) => item.effect_id === effect.id && !item.bypassed,
          );
          const isPresent = editor.selectedEffects.some(
            (item) => item.effect_id === effect.id,
          );
          const isUnavailable = availability?.available === false;
          const itemClassName = [
            "catalog-item",
            isEnabled ? "enabled" : "",
            isPresent && !isEnabled ? "bypassed" : "",
          ]
            .filter(Boolean)
            .join(" ");
          return (
            <button
              className={itemClassName}
              disabled={!channel}
              key={effect.id}
              onClick={() => editor.addEffect(effect)}
              title={
                isEnabled
                  ? "Enabled on this source"
                  : isUnavailable
                    ? availability.detail
                    : isPresent
                      ? "Bypassed on this source"
                      : "Add to this source"
              }
              type="button"
            >
              <span>{effect.name}</span>
              {isUnavailable ? (
                <CircleAlert size={15} />
              ) : isEnabled ? (
                <Check size={15} />
              ) : (
                <CirclePlus size={15} />
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}

export type { SetEffectChain };
