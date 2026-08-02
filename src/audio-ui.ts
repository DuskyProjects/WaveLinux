import type { AppStateSnapshot, Channel } from "./types";

type AutoDevices = AppStateSnapshot["graph"]["auto_devices"];
type InputDevice = AppStateSnapshot["graph"]["inputs"][number];

export function isHardwareChannel(channel: Pick<Channel, "kind">): boolean {
  return channel.kind === "microphone" || channel.kind === "generic";
}

export function channelDisplayName(
  channel: Pick<Channel, "id" | "kind" | "name">,
): string {
  if (
    channel.id === "hardware_in" &&
    isHardwareChannel(channel) &&
    ["hardware in", "hardware input", "input"].includes(channel.name.trim().toLowerCase())
  ) {
    return "Input";
  }
  return channel.name;
}

export function autoMicrophoneLabel(
  inputs: AppStateSnapshot["graph"]["inputs"],
  fallback: string,
  autoDevices: AutoDevices = [],
  channelId?: string,
): string {
  const resolved = resolvedAutoInput(autoDevices, channelId);
  if (resolved?.device_description || resolved?.device_id) {
    return `Auto: ${resolved.device_description ?? resolved.device_id}`;
  }
  const input = inputs[0];
  return input ? `Auto: ${input.description}` : fallback;
}

export function sortedMicrophoneInputs(
  inputs: AppStateSnapshot["graph"]["inputs"],
): AppStateSnapshot["graph"]["inputs"] {
  return inputs
    .filter(isMicrophoneSource)
    .slice()
    .sort((left, right) => {
      const priority = microphoneInputPriority(right) - microphoneInputPriority(left);
      if (priority !== 0) return priority;
      if (left.is_default !== right.is_default) return left.is_default ? -1 : 1;
      return left.description.localeCompare(right.description);
    });
}

export function resolvedAutoInput(autoDevices: AutoDevices, channelId?: string) {
  return autoDevices.find(
    (device) => device.kind === "input" && (!channelId || device.channel_id === channelId),
  );
}

function isMicrophoneSource(device: InputDevice): boolean {
  const name = device.name.toLowerCase();
  const description = device.description.toLowerCase();
  return (
    device.is_available !== false &&
    !isWaveLinuxManagedDevice(device) &&
    !name.endsWith(".monitor") &&
    !description.startsWith("monitor of ") &&
    !description.includes(" monitor")
  );
}

function isWaveLinuxManagedDevice(
  device: Pick<InputDevice, "id" | "name" | "is_virtual">,
): boolean {
  return (
    device.is_virtual &&
    (looksLikeWaveLinuxNode(device.id) || looksLikeWaveLinuxNode(device.name))
  );
}

function looksLikeWaveLinuxNode(value: string): boolean {
  return value.toLowerCase().includes("wavelinux");
}

function microphoneInputPriority(device: InputDevice): number {
  const text = `${device.id} ${device.name} ${device.description}`.toLowerCase();
  if (text.includes("usb")) return 60;
  if (text.includes("bluez") || text.includes("bluetooth")) return 30;
  if (
    text.includes("jack") ||
    text.includes("headset") ||
    text.includes("headphone") ||
    text.includes("linein") ||
    text.includes("line-in") ||
    text.includes("front mic") ||
    text.includes("rear mic")
  ) {
    return 50;
  }
  if (
    text.includes("built-in") ||
    text.includes("built in") ||
    text.includes("internal") ||
    text.includes("digital microphone") ||
    text.includes("dmic") ||
    text.includes("hda") ||
    text.includes("pci")
  ) {
    return 40;
  }
  if (text.includes("mic") || text.includes("microphone") || text.includes("analog")) return 35;
  return 1;
}
