import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import {
  applyOperationAcknowledgement,
  reconcileWaveLinuxState,
  replaceWaveLinuxState,
  waitForWaveLinuxStateRevision,
  waveLinuxRevisions,
  type OperationAcknowledgement,
} from "./state";
import type { AppStateSnapshot } from "./types";

const isTauri =
  typeof window !== "undefined" &&
  ("__TAURI_INTERNALS__" in window || "__TAURI__" in window);

const acknowledgedCommands = new Set([
  "assign_app_to_channel",
  "bypass_effect",
  "create_channel",
  "create_mix",
  "delete_channel",
  "delete_mix",
  "forget_app",
  "merge_app_identity",
  "move_channel",
  "move_app_stream",
  "move_app_stream_to_default",
  "move_mix",
  "pin_app_identity",
  "rename_channel",
  "rename_mix",
  "remove_app_route",
  "remove_app_volume_preset",
  "reset_app_identity",
  "restore_app",
  "set_app_stream_mute",
  "set_app_stream_volume",
  "set_app_volume_preset",
  "set_channel_bus_enabled",
  "set_channel_effects_enabled",
  "set_channel_icon",
  "set_channel_input",
  "set_channel_input_mode",
  "set_channel_linked",
  "set_channel_mute",
  "set_channel_volume",
  "set_device_hardware_profile",
  "set_effect_chain",
  "set_effect_param",
  "set_fallback_hardware_profile",
  "set_hardware_input_device",
  "set_hardware_profile_policy",
  "set_mix_icon",
  "set_mix_monitor_output",
  "set_mix_mute",
  "set_mix_outputs",
  "set_mix_volume",
  "set_settings",
]);

type OperationResponse<T> = OperationAcknowledgement & {
  value: T;
};

let requestSequence = 0;
let recoveryTarget: OperationAcknowledgement | null = null;
let recoveryInFlight = false;

function nextRequestId(command: string): string {
  requestSequence += 1;
  return `${command}-${Date.now().toString(36)}-${requestSequence.toString(36)}`;
}

function mergeRecoveryTarget(next: OperationAcknowledgement): void {
  if (recoveryTarget === null || next.state_revision >= recoveryTarget.state_revision) {
    recoveryTarget = next;
  }
}

async function recoverMissingStateDelta(target: OperationAcknowledgement): Promise<void> {
  mergeRecoveryTarget(target);
  if (recoveryInFlight) return;
  recoveryInFlight = true;
  try {
    while (recoveryTarget !== null) {
      const nextTarget = recoveryTarget;
      recoveryTarget = null;
      if (waveLinuxRevisions().state >= nextTarget.state_revision) continue;
      const next = await tauriInvoke<AppStateSnapshot>("observe_state");
      reconcileWaveLinuxState(next, nextTarget);
    }
  } catch {
    // The regular state event stream remains authoritative. A later mutation
    // will retry recovery if this one-shot compatibility snapshot fails.
  } finally {
    recoveryInFlight = false;
    if (recoveryTarget !== null) void recoverMissingStateDelta(recoveryTarget);
  }
}

function verifyStateDelivery(response: OperationAcknowledgement): void {
  void waitForWaveLinuxStateRevision(response.state_revision).then((delivered) => {
    if (!delivered) void recoverMissingStateDelta(response);
  });
}

export async function invoke<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (isTauri) {
    if (!acknowledgedCommands.has(command)) {
      return tauriInvoke<T>(command, args);
    }
    const requestId = nextRequestId(command);
    const response = await tauriInvoke<OperationResponse<T>>(command, {
      ...args,
      requestId,
    });
    applyOperationAcknowledgement(response);
    if (response.status !== "succeeded") {
      throw new Error(response.error || `${command} failed`);
    }
    verifyStateDelivery(response);
    return response.value;
  }

  if (import.meta.env.DEV) {
    const { invokeDemo } = await import("./demo");
    const value = invokeDemo<T>(command, args);
    if (acknowledgedCommands.has(command)) {
      replaceWaveLinuxState(invokeDemo<AppStateSnapshot>("get_state"));
    }
    return value;
  }

  throw new Error("WaveLinux requires the Tauri runtime");
}

export function initialSnapshot(): AppStateSnapshot | null {
  return null;
}
