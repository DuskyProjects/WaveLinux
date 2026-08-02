use super::*;

impl WaveLinuxEngine {
    pub fn set_channel_input(
        self: &Arc<Self>,
        channel_id: String,
        source_device: Option<String>,
    ) -> Result<Channel, EngineError> {
        let source_device = self.sanitize_hardware_input_for_bluetooth_a2dp(source_device);
        let channel =
            self.update_config(|config| config.set_channel_input(channel_id, source_device))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn set_hardware_input_device(
        self: &Arc<Self>,
        channel_id: String,
        source_device: Option<String>,
    ) -> Result<Channel, EngineError> {
        self.set_channel_input(channel_id, source_device)
    }

    pub fn set_channel_input_mode(
        self: &Arc<Self>,
        channel_id: String,
        input_mode: ChannelInputMode,
    ) -> Result<Channel, EngineError> {
        let channel =
            self.update_config(|config| config.set_channel_input_mode(channel_id, input_mode))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn restore_device(self: &Arc<Self>, kind: String) -> Result<MixerConfig, EngineError> {
        let normalized_kind = kind.trim().to_ascii_lowercase();
        let config = self.update_config(|config| {
            match normalized_kind.as_str() {
                "input" | "source" => {
                    let source = config.device_policy.restorable_input.clone();
                    if let Some(source) = source {
                        if let Some(channel) = config
                            .channels
                            .iter_mut()
                            .find(|channel| channel.kind.uses_hardware_slot())
                        {
                            channel.source_device = Some(source.clone());
                            config.device_policy.preferred_input = Some(source);
                        }
                    }
                    config.device_policy.restorable_input = None;
                    config.device_policy.active_input_fallback = false;
                }
                "output" | "sink" => {
                    let output = config.device_policy.restorable_output.clone();
                    if let Some(output) = output {
                        let mix_index = config
                            .mixes
                            .iter()
                            .position(|mix| mix.id == "monitor")
                            .or_else(|| (!config.mixes.is_empty()).then_some(0));
                        if let Some(mix_index) = mix_index {
                            let mix = &mut config.mixes[mix_index];
                            mix.set_outputs(vec![output.clone()]);
                            config.device_policy.preferred_output = Some(output);
                        }
                    }
                    config.device_policy.restorable_output = None;
                    config.device_policy.active_output_fallback = false;
                }
                _ => return Err(ModelError::InvalidName),
            }
            Ok(config.clone())
        })??;
        let _ = self.repair_audio_graph_if_running();
        Ok(config)
    }

    pub fn list_hardware_profiles(&self) -> Result<HardwareProfileUiState, EngineError> {
        let catalog = self.hardware_profiles()?;
        let config = self.read_config()?.clone();
        Ok(hardware_profile_ui_state(&catalog, &config.device_policy))
    }

    pub fn streamer_devices_config(&self) -> Result<StreamerDevicesConfig, EngineError> {
        Ok(self.read_config()?.streamer_devices.clone())
    }

    pub fn ensure_streamer_binding_profiles(
        &self,
        profiles: Vec<StreamerBindingProfile>,
    ) -> Result<StreamerDevicesConfig, EngineError> {
        self.update_config(|config| Ok(config.ensure_streamer_binding_profiles(profiles)))?
    }

    pub fn set_streamer_device_enabled(
        &self,
        device_id: String,
        enabled: bool,
    ) -> Result<StreamerDevicesConfig, EngineError> {
        self.update_config(|config| config.set_streamer_device_enabled(device_id, enabled))?
    }

    pub fn set_streamer_binding_profile(
        &self,
        profile: StreamerBindingProfile,
    ) -> Result<StreamerBindingProfile, EngineError> {
        self.update_config(|config| config.set_streamer_binding_profile(profile))?
    }

    pub fn set_device_hardware_profile(
        &self,
        device_id: String,
        profile_id: Option<String>,
    ) -> Result<HardwareProfileUiState, EngineError> {
        let device_id = device_id.trim().to_string();
        if device_id.is_empty() {
            return Err(ModelError::InvalidName.into());
        }
        let profile_id = profile_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(profile_id) = profile_id.as_deref() {
            let config = self.read_config()?.clone();
            let catalog = self.hardware_profiles()?;
            if profile_id != config.device_policy.fallback_hardware_profile.id
                && !catalog
                    .profiles
                    .iter()
                    .any(|entry| entry.profile.id == profile_id)
            {
                return Err(ModelError::InvalidConfig(format!(
                    "unknown hardware profile: {profile_id}"
                ))
                .into());
            }
        }

        self.update_config(|config| {
            if let Some(profile_id) = profile_id.clone() {
                config
                    .device_policy
                    .hardware_profile_assignments
                    .insert(device_id.clone(), profile_id);
            } else {
                config
                    .device_policy
                    .hardware_profile_assignments
                    .remove(&device_id);
            }
            Ok(())
        })??;
        self.log_engine_event(
            "hardware.profile.assignment",
            format!(
                "device={} profile={}",
                device_id,
                profile_id.as_deref().unwrap_or("auto")
            ),
        );
        let _ = self.refresh_runtime();
        self.list_hardware_profiles()
    }

    pub fn set_fallback_hardware_profile(
        &self,
        fallback_profile: FallbackHardwareProfile,
    ) -> Result<HardwareProfileUiState, EngineError> {
        let fallback_profile = fallback_profile.normalized();
        let fallback_id = fallback_profile.id.clone();
        self.update_config(|config| {
            let old_id = config.device_policy.fallback_hardware_profile.id.clone();
            config.device_policy.fallback_hardware_profile = fallback_profile.clone();
            if old_id != fallback_id {
                for assigned_profile_id in config
                    .device_policy
                    .hardware_profile_assignments
                    .values_mut()
                {
                    if *assigned_profile_id == old_id {
                        *assigned_profile_id = fallback_id.clone();
                    }
                }
            }
            Ok(())
        })??;
        self.log_engine_event(
            "hardware.profile.fallback",
            format!("profile={} name={}", fallback_id, fallback_profile.name),
        );
        let _ = self.refresh_runtime();
        self.list_hardware_profiles()
    }

    pub fn set_hardware_profile_policy(
        &self,
        profile_id: String,
        name: Option<String>,
        latency_policy: LatencyPolicy,
        routing_policy: RoutingPolicy,
    ) -> Result<HardwareProfileUiState, EngineError> {
        let profile_id = clean_profile_id(profile_id)?;
        let name = name.and_then(clean_optional_profile_name);
        let config = self.read_config()?.clone();
        if profile_id == config.device_policy.fallback_hardware_profile.id {
            let mut fallback_profile = config.device_policy.fallback_hardware_profile.clone();
            if let Some(name) = name {
                fallback_profile.name = name;
            }
            fallback_profile.latency_policy = normalized_profile_latency(latency_policy);
            fallback_profile.routing_policy = normalized_profile_routing(routing_policy);
            return self.set_fallback_hardware_profile(fallback_profile);
        }

        let catalog = self.hardware_profiles()?;
        let mut profile = hardware_profile_by_id(&catalog, &profile_id)
            .cloned()
            .ok_or_else(|| {
                ModelError::InvalidConfig(format!("unknown hardware profile: {profile_id}"))
            })?;
        if let Some(name) = name {
            profile.name = name;
        }
        profile.latency_policy = normalized_profile_latency(latency_policy);
        profile.routing_policy = normalized_profile_routing(routing_policy);
        profile.revision = profile.revision.saturating_add(1).max(1);
        let path = self.write_local_hardware_profile_override(&profile)?;
        self.reload_hardware_profiles_cache()?;
        self.log_engine_event(
            "hardware.profile.override",
            format!("profile={} path={}", profile.id, path.display()),
        );
        let _ = self.refresh_runtime();
        self.list_hardware_profiles()
    }
}
