import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  effectChainsEqual,
  insertEffectByPreferredOrder,
  isSingleInstanceEffect,
  normalizeSourceEffects,
  visibleUserCatalogEffects,
} from "../effect-chain";
import type {
  AppStateSnapshot,
  Channel,
  EffectDefinition,
  EffectInstance,
} from "../types";

export type SetEffectChain = (
  channelId: string,
  effects: EffectInstance[],
) => Promise<Channel>;

export function useEffectChainEditor(
  state: AppStateSnapshot,
  selectedChannel: Channel | undefined,
  setEffectChain: SetEffectChain,
) {
  const [draftEffectsByChannel, setDraftEffectsByChannel] = useState<
    Record<string, EffectInstance[]>
  >({});
  const [pendingEffectWrites, setPendingEffectWrites] = useState<Record<string, number>>(
    {},
  );
  const [effectError, setEffectError] = useState<string | null>(null);
  const effectWriteGeneration = useRef<Record<string, number>>({});
  const catalogEffects = useMemo(
    () =>
      visibleUserCatalogEffects(
        state.catalog.effects,
        state.catalog.preferred_order,
      ),
    [state.catalog.effects, state.catalog.preferred_order],
  );
  const selectedEffects = selectedChannel
    ? draftEffectsByChannel[selectedChannel.id] ?? selectedChannel.effects
    : [];

  useEffect(() => {
    setDraftEffectsByChannel((current) => {
      let changed = false;
      const next = { ...current };
      for (const channel of state.config.channels) {
        const draft = next[channel.id];
        if (draft && effectChainsEqual(draft, channel.effects)) {
          delete next[channel.id];
          changed = true;
        }
      }
      return changed ? next : current;
    });
  }, [state.config.channels]);

  useEffect(() => {
    setEffectError(null);
  }, [selectedChannel?.id]);

  const updateEffects = useCallback(
    (
      effects: EffectInstance[],
      preferredInstanceId?: string,
    ) => {
      if (!selectedChannel) return;
      const channelId = selectedChannel.id;
      const optimisticEffects = normalizeSourceEffects(effects, preferredInstanceId);
      const writeGeneration = (effectWriteGeneration.current[channelId] ?? 0) + 1;
      effectWriteGeneration.current[channelId] = writeGeneration;
      setEffectError(null);
      setDraftEffectsByChannel((current) => ({
        ...current,
        [channelId]: optimisticEffects,
      }));
      setPendingEffectWrites((current) => ({
        ...current,
        [channelId]: (current[channelId] ?? 0) + 1,
      }));
      void setEffectChain(channelId, optimisticEffects)
        .then((channel) => {
          if (effectWriteGeneration.current[channelId] !== writeGeneration) return;
          setDraftEffectsByChannel((current) => ({
            ...current,
            [channelId]: channel.effects,
          }));
          setEffectError(null);
        })
        .catch((error) => {
          if (effectWriteGeneration.current[channelId] !== writeGeneration) return;
          setEffectError(String(error));
          setDraftEffectsByChannel((current) => {
            const next = { ...current };
            delete next[channelId];
            return next;
          });
        })
        .finally(() => {
          setPendingEffectWrites((current) => {
            const count = Math.max(0, (current[channelId] ?? 1) - 1);
            const next = { ...current };
            if (count === 0) delete next[channelId];
            else next[channelId] = count;
            return next;
          });
        });
    },
    [selectedChannel, setEffectChain],
  );

  const addEffect = useCallback(
    (definition: EffectDefinition) => {
      if (!selectedChannel) return;
      const existing = selectedEffects.find(
        (effect) => effect.effect_id === definition.id,
      );
      if (existing && isSingleInstanceEffect(definition.id)) {
        if (!existing.bypassed) return;
        updateEffects(
          selectedEffects.map((effect) =>
            effect.instance_id === existing.instance_id
              ? { ...effect, bypassed: false }
              : effect,
          ),
          existing.instance_id,
        );
        return;
      }
      const instance: EffectInstance = {
        instance_id: crypto.randomUUID(),
        effect_id: definition.id,
        name: null,
        bypassed: false,
        params: Object.fromEntries(
          definition.params.map((param) => [param.id, param.default]),
        ),
      };
      updateEffects(
        insertEffectByPreferredOrder(
          selectedEffects,
          instance,
          state.catalog.preferred_order,
        ),
        instance.instance_id,
      );
    },
    [selectedChannel, selectedEffects, state.catalog.preferred_order, updateEffects],
  );

  const applyPreset = useCallback(
    (instanceId: string, values: Record<string, number>) => {
      updateEffects(
        selectedEffects.map((effect) =>
          effect.instance_id === instanceId
            ? { ...effect, params: { ...effect.params, ...values } }
            : effect,
        ),
      );
    },
    [selectedEffects, updateEffects],
  );

  const updateEffectParam = useCallback(
    (instanceId: string, paramId: string, value: number) => {
      updateEffects(
        selectedEffects.map((effect) =>
          effect.instance_id === instanceId
            ? { ...effect, params: { ...effect.params, [paramId]: value } }
            : effect,
        ),
      );
    },
    [selectedEffects, updateEffects],
  );

  const moveEffect = useCallback(
    (index: number, direction: -1 | 1) => {
      const target = index + direction;
      if (target < 0 || target >= selectedEffects.length) return;
      const effects = [...selectedEffects];
      [effects[index], effects[target]] = [effects[target], effects[index]];
      updateEffects(effects);
    },
    [selectedEffects, updateEffects],
  );

  const renameEffect = useCallback(
    (instanceId: string) => {
      const effect = selectedEffects.find((item) => item.instance_id === instanceId);
      if (!effect) return;
      const definition = state.catalog.effects.find(
        (item) => item.id === effect.effect_id,
      );
      const name = window.prompt(
        "Effect name",
        effect.name ?? definition?.name ?? effect.effect_id,
      );
      if (!name) return;
      updateEffects(
        selectedEffects.map((item) =>
          item.instance_id === instanceId
            ? { ...item, name: name.trim() || null }
            : item,
        ),
      );
    },
    [selectedEffects, state.catalog.effects, updateEffects],
  );

  const deleteEffect = useCallback(
    (instanceId: string) => {
      updateEffects(
        selectedEffects.filter((effect) => effect.instance_id !== instanceId),
      );
    },
    [selectedEffects, updateEffects],
  );

  const effectsForChannel = useCallback(
    (channel: Channel) => draftEffectsByChannel[channel.id] ?? channel.effects,
    [draftEffectsByChannel],
  );

  return {
    addEffect,
    applyPreset,
    catalogEffects,
    deleteEffect,
    effectError,
    effectsForChannel,
    moveEffect,
    pending: (pendingEffectWrites[selectedChannel?.id ?? ""] ?? 0) > 0,
    renameEffect,
    selectedEffects,
    updateEffectParam,
  };
}
