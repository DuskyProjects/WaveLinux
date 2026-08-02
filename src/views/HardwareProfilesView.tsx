import { useCallback, useEffect, useMemo, useState } from "react";
import { Cable, CircleAlert } from "lucide-react";
import { AppSelect, type SelectOption } from "../components/AppSelect";
import { EmptyState, Toggle, VolumeFader } from "../components/Controls";
import { invoke } from "../tauri";
import type {
  AppStateSnapshot,
  DeviceInfo,
  FallbackHardwareProfile,
  HardwareProfileSummary,
  HardwareProfileUiState,
} from "../types";

export function HardwareProfilesView({ state }: { state: AppStateSnapshot }) {
  const [hardwareProfiles, setHardwareProfiles] = useState<HardwareProfileUiState | null>(null);
  const [hardwareProfileError, setHardwareProfileError] = useState<string | null>(null);
  const fallbackProfile =
    hardwareProfiles?.fallback_profile ?? state.config.device_policy.fallback_hardware_profile;
  const fallbackSummary = useMemo(
    () => hardwareProfileSummaryFromFallback(fallbackProfile),
    [fallbackProfile],
  );
  const [selectedDeviceId, setSelectedDeviceId] = useState<string | null>(null);
  const [profileNameDraft, setProfileNameDraft] = useState(fallbackSummary.name);
  const hardwareDevices = useMemo(
    () => [
      ...state.graph.inputs
        .filter(isHardwareProfileDevice)
        .map((device) => ({ device, kind: "Input" })),
      ...state.graph.outputs
        .filter(isHardwareProfileDevice)
        .map((device) => ({ device, kind: "Output" })),
    ],
    [state.graph.inputs, state.graph.outputs],
  );
  const profileSummaries = useMemo(() => {
    const profilesById = new Map<string, HardwareProfileSummary>();
    for (const profile of hardwareProfiles?.profiles ?? []) {
      profilesById.set(profile.id, profile);
    }
    profilesById.set(fallbackSummary.id, fallbackSummary);
    return Array.from(profilesById.values());
  }, [fallbackSummary, hardwareProfiles?.profiles]);
  const profileById = useMemo(
    () => new Map(profileSummaries.map((profile) => [profile.id, profile])),
    [profileSummaries],
  );
  const profileOptions = useMemo(() => {
    const options: SelectOption[] = [{ value: "", label: "Auto match" }];
    for (const profile of profileSummaries) {
      options.push({ value: profile.id, label: hardwareProfileOptionLabel(profile) });
    }
    const missingAssignments = new Set(
      Object.values(hardwareProfiles?.assignments ?? {}).filter(
        (profileId) => !profileById.has(profileId),
      ),
    );
    for (const { device } of hardwareDevices) {
      if (device.matched_profile_id && !profileById.has(device.matched_profile_id)) {
        missingAssignments.add(device.matched_profile_id);
      }
    }
    for (const profileId of missingAssignments) {
      options.push({
        value: profileId,
        label: `Missing profile: ${profileId}`,
        disabled: true,
      });
    }
    return options;
  }, [hardwareDevices, hardwareProfiles?.assignments, profileById, profileSummaries]);
  const resolvedProfileIdForDevice = useCallback(
    (device: DeviceInfo) =>
      hardwareProfiles?.assignments[device.id] || device.matched_profile_id || fallbackProfile.id,
    [fallbackProfile.id, hardwareProfiles?.assignments],
  );
  const selectedDeviceEntry = useMemo(
    () =>
      hardwareDevices.find(({ device }) => device.id === selectedDeviceId) ??
      hardwareDevices[0] ??
      null,
    [hardwareDevices, selectedDeviceId],
  );
  const currentProfileId = selectedDeviceEntry
    ? resolvedProfileIdForDevice(selectedDeviceEntry.device)
    : fallbackProfile.id;
  const currentProfile = profileById.get(currentProfileId) ?? fallbackSummary;

  const loadHardwareProfiles = useCallback(async () => {
    try {
      const next = await invoke<HardwareProfileUiState>("list_hardware_profiles");
      setHardwareProfiles(next);
      setHardwareProfileError(null);
    } catch (error) {
      setHardwareProfileError(String(error));
    }
  }, []);

  useEffect(() => {
    void loadHardwareProfiles();
  }, [loadHardwareProfiles]);

  useEffect(() => {
    if (hardwareDevices.length === 0) {
      setSelectedDeviceId(null);
      return;
    }
    if (
      !selectedDeviceId ||
      !hardwareDevices.some(({ device }) => device.id === selectedDeviceId)
    ) {
      setSelectedDeviceId(hardwareDevices[0].device.id);
    }
  }, [hardwareDevices, selectedDeviceId]);

  useEffect(() => {
    setProfileNameDraft(currentProfile.name);
  }, [currentProfile.id, currentProfile.name]);

  const assignHardwareProfile = async (deviceId: string, profileId: string) => {
    try {
      const next = await invoke<HardwareProfileUiState>("set_device_hardware_profile", {
        deviceId,
        device_id: deviceId,
        profileId: profileId || null,
        profile_id: profileId || null,
      });
      setHardwareProfiles(next);
      setHardwareProfileError(null);
    } catch (error) {
      setHardwareProfileError(String(error));
    }
  };

  const updateCurrentProfile = async (profile: HardwareProfileSummary) => {
    try {
      const next = await invoke<HardwareProfileUiState>("set_hardware_profile_policy", {
        profileId: profile.id,
        profile_id: profile.id,
        name: profile.name,
        latencyPolicy: profile.latency_policy,
        latency_policy: profile.latency_policy,
        routingPolicy: profile.routing_policy,
        routing_policy: profile.routing_policy,
      });
      setHardwareProfiles(next);
      setHardwareProfileError(null);
    } catch (error) {
      setHardwareProfileError(String(error));
    }
  };

  const updateCurrentLatency = (
    key: keyof HardwareProfileSummary["latency_policy"],
    value: number,
  ) => {
    void updateCurrentProfile({
      ...currentProfile,
      latency_policy: {
        ...currentProfile.latency_policy,
        [key]: Math.round(value),
      },
    }).catch(() => undefined);
  };

  const updateCurrentRouting = (
    key: keyof HardwareProfileSummary["routing_policy"],
    value: number | boolean,
  ) => {
    void updateCurrentProfile({
      ...currentProfile,
      routing_policy: {
        ...currentProfile.routing_policy,
        [key]: typeof value === "number" ? Math.round(value) : value,
      },
    }).catch(() => undefined);
  };

  const commitCurrentProfileName = () => {
    const name = profileNameDraft.trim();
    if (!name || name === currentProfile.name) {
      setProfileNameDraft(currentProfile.name);
      return;
    }
    void updateCurrentProfile({ ...currentProfile, name }).catch(() => undefined);
  };

  return (
    <section className="panel single-panel">
      <div className="panel-header">
        <h2>Hardware Profiles</h2>
        <Cable size={18} />
      </div>
      {hardwareProfileError && (
        <div className="effect-warning">
          <CircleAlert size={15} />
          <span>{hardwareProfileError}</span>
        </div>
      )}
      <div className="hardware-profile-grid">
        <div className="profile-editor">
          <label className="field-label" htmlFor="profiles-current-profile-name">
            Current profile
          </label>
          <input
            className="text-field"
            id="profiles-current-profile-name"
            onBlur={commitCurrentProfileName}
            onChange={(event) => setProfileNameDraft(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") event.currentTarget.blur();
            }}
            value={profileNameDraft}
          />
          <div className="profile-editor-meta">
            <strong>{currentProfile.source}</strong>
            <span>{currentProfile.confidence}</span>
          </div>
          {selectedDeviceEntry && (
            <div className="profile-editor-meta">
              <strong>{selectedDeviceEntry.kind}</strong>
              <span>
                {selectedDeviceEntry.device.description || selectedDeviceEntry.device.name}
              </span>
            </div>
          )}
          <VolumeFader
            compact
            label="Stable"
            max={500}
            min={5}
            unit=" ms"
            value={currentProfile.latency_policy.stable_msec ?? 35}
            onChange={(value) => updateCurrentLatency("stable_msec", value)}
          />
          <VolumeFader
            compact
            label="Low latency"
            max={500}
            min={5}
            unit=" ms"
            value={currentProfile.latency_policy.low_latency_msec ?? 20}
            onChange={(value) => updateCurrentLatency("low_latency_msec", value)}
          />
          <VolumeFader
            compact
            label="Bluetooth floor"
            max={500}
            min={50}
            unit=" ms"
            value={currentProfile.latency_policy.bluetooth_floor_msec ?? 120}
            onChange={(value) => updateCurrentLatency("bluetooth_floor_msec", value)}
          />
          <VolumeFader
            compact
            label="Input priority"
            max={100}
            min={0}
            unit=""
            value={currentProfile.routing_policy.input_priority ?? 35}
            onChange={(value) => updateCurrentRouting("input_priority", value)}
          />
          <VolumeFader
            compact
            label="Output priority"
            max={100}
            min={0}
            unit=""
            value={currentProfile.routing_policy.output_priority ?? 30}
            onChange={(value) => updateCurrentRouting("output_priority", value)}
          />
          <Toggle
            label="Auto-select input"
            onChange={(value) => updateCurrentRouting("allow_auto_select_input", value)}
            value={currentProfile.routing_policy.allow_auto_select_input}
          />
          <Toggle
            label="Auto-select output"
            onChange={(value) => updateCurrentRouting("allow_auto_select_output", value)}
            value={currentProfile.routing_policy.allow_auto_select_output}
          />
          <Toggle
            label="Prefer wired input"
            onChange={(value) => updateCurrentRouting("prefer_non_bluetooth_input", value)}
            value={currentProfile.routing_policy.prefer_non_bluetooth_input}
          />
        </div>
        <div className="profile-device-list">
          {hardwareDevices.map(({ device, kind }) => {
            const assignment = hardwareProfiles?.assignments[device.id] ?? "";
            const resolvedProfileId = resolvedProfileIdForDevice(device);
            const activeProfile = profileById.get(resolvedProfileId);
            const selected = selectedDeviceEntry?.device.id === device.id;
            return (
              <div
                className={selected ? "profile-device-row selected" : "profile-device-row"}
                key={`${kind}-${device.id}`}
                onClick={() => setSelectedDeviceId(device.id)}
                onFocus={() => setSelectedDeviceId(device.id)}
              >
                <div>
                  <strong>{device.description || device.name}</strong>
                  <span>
                    {kind} · {device.bus ?? "unknown"} ·{" "}
                    {activeProfile?.name ?? device.matched_profile_id ?? "Auto"}
                  </span>
                </div>
                <AppSelect
                  ariaLabel={`${device.description || device.name} profile`}
                  disabled={!hardwareProfiles}
                  onChange={(value) =>
                    void assignHardwareProfile(device.id, value).catch(() => undefined)
                  }
                  options={profileOptions}
                  value={assignment || resolvedProfileId}
                />
              </div>
            );
          })}
          {hardwareDevices.length === 0 && <EmptyState label="No hardware devices detected" />}
        </div>
      </div>
    </section>
  );
}

function hardwareProfileOptionLabel(profile: HardwareProfileSummary): string {
  return `${profile.name} · ${profile.source}`;
}

function hardwareProfileSummaryFromFallback(
  profile: FallbackHardwareProfile,
): HardwareProfileSummary {
  return {
    id: profile.id,
    name: profile.name,
    source: "default",
    confidence: profile.confidence,
    latency_policy: profile.latency_policy,
    routing_policy: profile.routing_policy,
    bluetooth_mic_policy: profile.bluetooth_mic_policy,
  };
}

function isHardwareProfileDevice(device: DeviceInfo): boolean {
  if (device.is_virtual || device.bus === "virtual") return false;
  const text = [device.id, device.name, device.description].join(" ").toLowerCase();
  if (text.includes("wavelinux")) return false;
  if (text.includes(".monitor") || text.includes("monitor of")) return false;
  return true;
}
