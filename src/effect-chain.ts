import type { EffectDefinition, EffectInstance } from "./types";

const singleInstanceEffectIds = new Set([
  "rnnoise",
  "highpass",
  "eq",
  "compressor",
  "gate",
  "karaoke_stage",
  "limiter",
]);

const hiddenUserCatalogEffectIds = new Set<string>();

export function normalizeSourceEffects(
  effects: EffectInstance[],
  preferredInstanceId?: string,
): EffectInstance[] {
  const singleInstanceIndexes = new Map<string, number[]>();
  for (const [index, effect] of effects.entries()) {
    if (!isSingleInstanceEffect(effect.effect_id)) continue;
    const indexes = singleInstanceIndexes.get(effect.effect_id) ?? [];
    indexes.push(index);
    singleInstanceIndexes.set(effect.effect_id, indexes);
  }

  if (singleInstanceIndexes.size === 0) return structuredClone(effects);

  const keepIndexes = new Set<number>();
  for (const indexes of singleInstanceIndexes.values()) {
    const preferred = indexes.find((index) => effects[index]?.instance_id === preferredInstanceId);
    const active = [...indexes]
      .reverse()
      .find((index) => effects[index] && !effects[index].bypassed);
    const keepIndex = preferred ?? active ?? indexes.at(-1);
    if (keepIndex !== undefined) keepIndexes.add(keepIndex);
  }

  return effects
    .filter((effect, index) => !isSingleInstanceEffect(effect.effect_id) || keepIndexes.has(index))
    .map((effect) => structuredClone(effect));
}

export function insertEffectByPreferredOrder(
  effects: EffectInstance[],
  nextEffect: EffectInstance,
  preferredOrder: string[],
): EffectInstance[] {
  const preferredIndex = effectPreferredOrderIndex(nextEffect.effect_id, preferredOrder);
  const next = structuredClone(effects);
  let insertAt = next.length;
  for (let index = 0; index < next.length; index += 1) {
    if (effectPreferredOrderIndex(next[index].effect_id, preferredOrder) > preferredIndex) {
      insertAt = index;
      break;
    }
  }
  next.splice(insertAt, 0, structuredClone(nextEffect));
  return next;
}

export function isSingleInstanceEffect(effectId: string): boolean {
  return singleInstanceEffectIds.has(effectId);
}

export function visibleUserCatalogEffects(
  effects: EffectDefinition[],
  preferredOrder: string[] = [],
): EffectDefinition[] {
  return [...effects]
    .filter((effect) => !hiddenUserCatalogEffectIds.has(effect.id))
    .sort((left, right) => {
      const leftOrder = effectPreferredOrderIndex(left.id, preferredOrder);
      const rightOrder = effectPreferredOrderIndex(right.id, preferredOrder);
      if (leftOrder !== rightOrder) return leftOrder - rightOrder;
      return left.name.localeCompare(right.name);
    });
}

export function effectChainsEqual(left: EffectInstance[], right: EffectInstance[]): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function effectPreferredOrderIndex(effectId: string, preferredOrder: string[]): number {
  const index = preferredOrder.indexOf(effectId);
  return index >= 0 ? index : Number.MAX_SAFE_INTEGER;
}
