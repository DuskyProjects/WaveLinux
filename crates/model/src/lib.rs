use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const CONFIG_VERSION: u32 = 9;
pub const MAX_MIXES: usize = 5;
pub const MAX_SOFTWARE_CHANNELS: usize = 8;
pub const MAX_HARDWARE_INPUTS: usize = 4;
pub const MAX_CHANNELS: usize = MAX_SOFTWARE_CHANNELS + MAX_HARDWARE_INPUTS;
pub const SAMPLE_RATE_HZ: u32 = 48_000;
pub const BIT_DEPTH: u16 = 24;
pub const CHANNEL_LAYOUT: &str = "stereo";

pub type MixId = String;
pub type ChannelId = String;
pub type DeviceId = String;
pub type AppStreamId = String;
pub type EffectInstanceId = String;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ModelError {
    #[error("mix limit reached ({MAX_MIXES})")]
    MixLimitReached,
    #[error(
        "channel limit reached ({MAX_SOFTWARE_CHANNELS} software + {MAX_HARDWARE_INPUTS} hardware)"
    )]
    ChannelLimitReached,
    #[error("mix not found: {0}")]
    MixNotFound(String),
    #[error("channel not found: {0}")]
    ChannelNotFound(String),
    #[error("effect not found: {0}")]
    EffectNotFound(String),
    #[error("duplicate mix name: {0}")]
    DuplicateMixName(String),
    #[error("duplicate channel name: {0}")]
    DuplicateChannelName(String),
    #[error("invalid name")]
    InvalidName,
    #[error("invalid app matcher")]
    InvalidMatcher,
    #[error("cannot delete the last mix")]
    CannotDeleteLastMix,
    #[error("cannot delete the last channel")]
    CannotDeleteLastChannel,
    #[error("invalid volume: {0}")]
    InvalidVolume(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MixerSettings {
    pub theme: ThemeMode,
    pub start_at_login: bool,
    #[serde(default)]
    pub keep_running_in_tray: bool,
    #[serde(default)]
    pub restore_audio_graph_on_launch: bool,
    #[serde(default = "default_true")]
    pub monitor_follows_default_output: bool,
    pub lock_default_input: bool,
    pub lock_default_output: bool,
    #[serde(default)]
    pub low_latency_mic_monitoring: bool,
    #[serde(default)]
    pub hardware_direct_mic_monitoring: bool,
    #[serde(default)]
    pub stream_sync_delay_msec: u16,
    #[serde(default)]
    pub monitor_sync_delay_msec: u16,
    pub auto_check_updates: bool,
    pub auto_install_updates: bool,
    pub release_channel: ReleaseChannel,
    #[serde(default)]
    pub optimization_mode: OptimizationMode,
    #[serde(default, skip_serializing)]
    pub runtime_latency_policy: Option<LatencyPolicy>,
}

impl Default for MixerSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            start_at_login: false,
            keep_running_in_tray: true,
            restore_audio_graph_on_launch: false,
            monitor_follows_default_output: true,
            lock_default_input: false,
            lock_default_output: false,
            low_latency_mic_monitoring: false,
            hardware_direct_mic_monitoring: false,
            stream_sync_delay_msec: 0,
            monitor_sync_delay_msec: 0,
            auto_check_updates: true,
            auto_install_updates: false,
            release_channel: ReleaseChannel::Stable,
            optimization_mode: OptimizationMode::Performance,
            runtime_latency_policy: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChannel {
    Stable,
    Beta,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationMode {
    #[default]
    Performance,
    Safe,
    Advisory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MixerConfig {
    pub version: u32,
    pub mixes: Vec<Mix>,
    pub channels: Vec<Channel>,
    pub app_routes: Vec<AppRoute>,
    pub app_volume_presets: Vec<AppVolumePreset>,
    pub app_history: Vec<KnownApp>,
    pub app_identity_overrides: Vec<AppIdentityOverride>,
    pub app_label_overrides: Vec<AppLabelOverride>,
    pub device_policy: DevicePolicy,
    pub streamer_devices: StreamerDevicesConfig,
    pub settings: MixerSettings,
    pub audio: AudioSpec,
}

impl Default for MixerConfig {
    fn default() -> Self {
        let mixes = vec![
            Mix::new_fixed("monitor", "Monitor"),
            Mix::new_fixed("stream", "Stream"),
        ];

        let mut channels = vec![
            Channel::new_fixed("hardware_in", "Input", ChannelKind::Generic),
            Channel::new_fixed("system", "System", ChannelKind::System),
            Channel::new_fixed("game", "Game", ChannelKind::Application),
            Channel::new_fixed("chat", "Chat", ChannelKind::Application),
            Channel::new_fixed("music", "Music", ChannelKind::Application),
            Channel::new_fixed("browser", "Browser", ChannelKind::Application),
            Channel::new_fixed("sfx", "SFX", ChannelKind::Soundboard),
        ];

        for channel in &mut channels {
            channel.ensure_mix_buses(&mixes);
        }

        Self {
            version: CONFIG_VERSION,
            mixes,
            channels,
            app_routes: Vec::new(),
            app_volume_presets: Vec::new(),
            app_history: Vec::new(),
            app_identity_overrides: Vec::new(),
            app_label_overrides: Vec::new(),
            device_policy: DevicePolicy::default(),
            streamer_devices: StreamerDevicesConfig::default(),
            settings: MixerSettings::default(),
            audio: AudioSpec::default(),
        }
    }
}

impl MixerConfig {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.version == 0 {
            return Err(ModelError::InvalidConfig("version must be non-zero".into()));
        }
        if self.mixes.is_empty() {
            return Err(ModelError::InvalidConfig(
                "at least one mix is required".into(),
            ));
        }
        if self.mixes.len() > MAX_MIXES {
            return Err(ModelError::MixLimitReached);
        }
        if self.channels.len() > MAX_CHANNELS
            || self.software_channel_count() > MAX_SOFTWARE_CHANNELS
            || self.hardware_input_count() > MAX_HARDWARE_INPUTS
        {
            return Err(ModelError::ChannelLimitReached);
        }
        ensure_unique_names(self.mixes.iter().map(|mix| mix.name.as_str()), "mix")?;
        ensure_unique_names(
            self.channels.iter().map(|channel| channel.name.as_str()),
            "channel",
        )?;
        for channel in &self.channels {
            for mix in &self.mixes {
                if !channel.mix_buses.contains_key(&mix.id) {
                    return Err(ModelError::InvalidConfig(format!(
                        "channel '{}' missing bus for mix '{}'",
                        channel.name, mix.name
                    )));
                }
            }
        }
        for route in &self.app_routes {
            self.ensure_channel_exists(&route.channel_id)?;
            if route.matcher.is_empty() {
                return Err(ModelError::InvalidMatcher);
            }
        }
        for preset in &self.app_volume_presets {
            if preset.matcher.is_empty() {
                return Err(ModelError::InvalidMatcher);
            }
            valid_unit(preset.volume)?;
        }
        for app in &self.app_history {
            if app.matcher.is_empty() {
                return Err(ModelError::InvalidMatcher);
            }
        }
        for override_rule in &self.app_identity_overrides {
            if override_rule.source.is_empty() || override_rule.target.is_empty() {
                return Err(ModelError::InvalidMatcher);
            }
        }
        for label in &self.app_label_overrides {
            if label.matcher.is_empty() {
                return Err(ModelError::InvalidMatcher);
            }
            if label.label.trim().is_empty() {
                return Err(ModelError::InvalidName);
            }
        }
        let catalog = EffectCatalog::default();
        for channel in &self.channels {
            validate_effect_chain(&channel.effects, &catalog)?;
        }
        Ok(())
    }

    pub fn normalized(mut self) -> Result<Self, ModelError> {
        let previous_version = self.version;
        self.version = CONFIG_VERSION;
        if previous_version < 4 {
            self.settings.lock_default_output = false;
        }
        self.settings.keep_running_in_tray = true;
        self.settings.stream_sync_delay_msec =
            clamp_sync_delay_msec(self.settings.stream_sync_delay_msec);
        self.settings.monitor_sync_delay_msec =
            clamp_sync_delay_msec(self.settings.monitor_sync_delay_msec);
        self.migrate_legacy_mic_channel();
        self.normalize_hardware_input_name();
        self.ensure_system_channel();
        for mix in &mut self.mixes {
            mix.ensure_virtual_node_names();
            mix.normalize_outputs();
            mix.icon = clean_optional_icon(mix.icon.take());
            mix.volume = clamp_unit(mix.volume);
        }
        for channel in &mut self.channels {
            channel.ensure_virtual_node_name();
            channel.icon = clean_optional_icon(channel.icon.take());
            channel.source_device = clean_optional_device_id(channel.source_device.take());
            if channel.kind.uses_hardware_slot() {
                channel.input_mode = ChannelInputMode::SumMono;
            }
            channel.ensure_mix_buses(&self.mixes);
            for bus in channel.mix_buses.values_mut() {
                bus.volume = clamp_unit(bus.volume);
            }
        }
        if previous_version < 5 {
            unmute_hardware_monitor_bus(&mut self.channels);
        }
        let catalog = EffectCatalog::default();
        for channel in &mut self.channels {
            channel.effects =
                normalize_effect_chain(std::mem::take(&mut channel.effects), &catalog, true)?;
        }
        self.normalize_app_identity();
        self.normalize_device_policy();
        self.normalize_streamer_devices();
        self.normalize_app_routes();
        self.normalize_app_volume_presets();
        self.normalize_app_history();
        self.validate()?;
        Ok(self)
    }

    fn normalize_app_routes(&mut self) {
        let valid_channels = self
            .channels
            .iter()
            .map(|channel| channel.id.clone())
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        self.app_routes = self
            .app_routes
            .drain(..)
            .filter_map(|mut route| {
                if !valid_channels.contains(&route.channel_id) {
                    return None;
                }
                route.matcher = route.matcher.normalized()?;
                let key = app_matcher_key(&route.matcher);
                seen.insert(key).then_some(route)
            })
            .collect();
    }

    fn normalize_app_history(&mut self) {
        let mut seen = BTreeSet::new();
        self.app_history = self
            .app_history
            .drain(..)
            .filter_map(|mut app| {
                app.matcher = app.matcher.normalized()?;
                app.display_name = clean_app_display_name(&app.display_name)
                    .unwrap_or_else(|| matcher_display_name(&app.matcher));
                app.media_name = clean_optional_label(app.media_name);
                let key = app_matcher_key(&app.matcher);
                seen.insert(key).then_some(app)
            })
            .collect();
        self.app_history.sort_by(|left, right| {
            right
                .last_seen_unix
                .cmp(&left.last_seen_unix)
                .then_with(|| left.display_name.cmp(&right.display_name))
        });
    }

    fn normalize_app_identity(&mut self) {
        let mut seen_overrides = BTreeSet::new();
        self.app_identity_overrides = self
            .app_identity_overrides
            .drain(..)
            .filter_map(|override_rule| {
                let source = override_rule.source.normalized()?;
                let target = override_rule.target.normalized()?;
                if source == target {
                    return None;
                }
                let key = app_matcher_key(&source);
                seen_overrides
                    .insert(key)
                    .then_some(AppIdentityOverride { source, target })
            })
            .collect();

        let mut seen_labels = BTreeSet::new();
        self.app_label_overrides = self
            .app_label_overrides
            .drain(..)
            .filter_map(|label| {
                let matcher = label.matcher.normalized()?;
                let label = clean_app_display_name(&label.label)?;
                let key = app_matcher_key(&matcher);
                seen_labels
                    .insert(key)
                    .then_some(AppLabelOverride { matcher, label })
            })
            .collect();
    }

    fn normalize_device_policy(&mut self) {
        self.device_policy.preferred_input =
            clean_optional_matcher(self.device_policy.preferred_input.take());
        self.device_policy.preferred_output =
            clean_optional_matcher(self.device_policy.preferred_output.take());
        self.device_policy.restorable_input =
            clean_optional_matcher(self.device_policy.restorable_input.take());
        self.device_policy.restorable_output =
            clean_optional_matcher(self.device_policy.restorable_output.take());
        self.device_policy.hardware_profile_assignments = self
            .device_policy
            .hardware_profile_assignments
            .iter()
            .filter_map(|(device_id, profile_id)| {
                let device_id = clean_optional_device_id(Some(device_id.clone()))?;
                let profile_id = clean_optional_profile_id(Some(profile_id.clone()))?;
                Some((device_id, profile_id))
            })
            .collect();
        self.device_policy.fallback_hardware_profile = self
            .device_policy
            .fallback_hardware_profile
            .clone()
            .normalized();
    }

    fn normalize_streamer_devices(&mut self) {
        self.streamer_devices = std::mem::take(&mut self.streamer_devices).normalized();
    }

    fn normalize_app_volume_presets(&mut self) {
        let mut seen = BTreeSet::new();
        self.app_volume_presets = self
            .app_volume_presets
            .drain(..)
            .filter_map(|mut preset| {
                preset.matcher = preset.matcher.normalized()?;
                preset.volume = clamp_unit(preset.volume);
                let key = app_matcher_key(&preset.matcher);
                seen.insert(key).then_some(preset)
            })
            .collect();
    }

    fn migrate_legacy_mic_channel(&mut self) {
        if self
            .channels
            .iter()
            .any(|channel| channel.id == "hardware_in")
        {
            return;
        }

        let Some(channel) = self.channels.iter_mut().find(|channel| {
            channel.id == "mic"
                || (channel.kind == ChannelKind::Microphone
                    && channel.name.eq_ignore_ascii_case("mic"))
                || (channel.kind == ChannelKind::Microphone
                    && channel.name.eq_ignore_ascii_case("microphone"))
        }) else {
            return;
        };

        let old_id = channel.id.clone();
        channel.id = "hardware_in".into();
        channel.name = "Input".into();
        channel.kind = ChannelKind::Generic;
        channel.virtual_sink_name = "wavelinux_channel_hardware_in".into();

        for route in &mut self.app_routes {
            if route.channel_id == old_id {
                route.channel_id = channel.id.clone();
            }
        }
    }

    fn normalize_hardware_input_name(&mut self) {
        for channel in &mut self.channels {
            if channel.id != "hardware_in" {
                continue;
            }
            let name = channel.name.trim();
            if name.is_empty()
                || name.eq_ignore_ascii_case("hardware in")
                || name.eq_ignore_ascii_case("hardware input")
            {
                channel.name = "Input".into();
            }
        }
    }

    fn ensure_system_channel(&mut self) {
        if self
            .channels
            .iter()
            .any(|channel| channel.kind == ChannelKind::System || channel.id == "system")
        {
            return;
        }
        if self.software_channel_count() >= MAX_SOFTWARE_CHANNELS
            || self.channels.len() >= MAX_CHANNELS
        {
            return;
        }

        let mut system = Channel::new_fixed("system", "System", ChannelKind::System);
        system.ensure_mix_buses(&self.mixes);
        let insert_at = self
            .channels
            .iter()
            .position(|channel| !channel.kind.uses_hardware_slot())
            .unwrap_or(self.channels.len());
        self.channels.insert(insert_at, system);
    }

    pub fn create_mix(&mut self, name: impl AsRef<str>) -> Result<Mix, ModelError> {
        if self.mixes.len() >= MAX_MIXES {
            return Err(ModelError::MixLimitReached);
        }
        let name = clean_name(name)?;
        if self.mix_name_exists(&name) {
            return Err(ModelError::DuplicateMixName(name));
        }
        let id = unique_slug(&name, self.mixes.iter().map(|mix| mix.id.as_str()));
        let mix = Mix::new_fixed(&id, &name);
        self.mixes.push(mix.clone());
        for channel in &mut self.channels {
            channel.mix_buses.insert(mix.id.clone(), MixBus::disabled());
        }
        Ok(mix)
    }

    pub fn rename_mix(
        &mut self,
        mix_id: impl AsRef<str>,
        name: impl AsRef<str>,
    ) -> Result<Mix, ModelError> {
        let mix_id = mix_id.as_ref();
        let name = clean_name(name)?;
        if self
            .mixes
            .iter()
            .any(|mix| mix.id != mix_id && mix.name.eq_ignore_ascii_case(&name))
        {
            return Err(ModelError::DuplicateMixName(name));
        }
        let mix = self
            .mixes
            .iter_mut()
            .find(|mix| mix.id == mix_id)
            .ok_or_else(|| ModelError::MixNotFound(mix_id.into()))?;
        mix.name = name;
        Ok(mix.clone())
    }

    pub fn move_mix(&mut self, mix_id: impl AsRef<str>, direction: i32) -> Result<Mix, ModelError> {
        let mix_id = mix_id.as_ref();
        let index = self
            .mixes
            .iter()
            .position(|mix| mix.id == mix_id)
            .ok_or_else(|| ModelError::MixNotFound(mix_id.into()))?;
        let target = offset_index(index, direction, self.mixes.len());
        if target == index {
            return Ok(self.mixes[index].clone());
        }
        let mix = self.mixes.remove(index);
        self.mixes.insert(target, mix.clone());
        Ok(mix)
    }

    pub fn delete_mix(&mut self, mix_id: impl AsRef<str>) -> Result<Mix, ModelError> {
        if self.mixes.len() <= 1 {
            return Err(ModelError::CannotDeleteLastMix);
        }
        let mix_id = mix_id.as_ref();
        let index = self
            .mixes
            .iter()
            .position(|mix| mix.id == mix_id)
            .ok_or_else(|| ModelError::MixNotFound(mix_id.into()))?;
        let removed = self.mixes.remove(index);
        for channel in &mut self.channels {
            channel.mix_buses.remove(&removed.id);
        }
        Ok(removed)
    }

    pub fn create_channel(
        &mut self,
        name: impl AsRef<str>,
        kind: ChannelKind,
    ) -> Result<Channel, ModelError> {
        if self.channels.len() >= MAX_CHANNELS
            || (kind.uses_hardware_slot() && self.hardware_input_count() >= MAX_HARDWARE_INPUTS)
            || (!kind.uses_hardware_slot()
                && self.software_channel_count() >= MAX_SOFTWARE_CHANNELS)
        {
            return Err(ModelError::ChannelLimitReached);
        }
        let name = clean_name(name)?;
        if self.channel_name_exists(&name) {
            return Err(ModelError::DuplicateChannelName(name));
        }
        let id = unique_slug(
            &name,
            self.channels.iter().map(|channel| channel.id.as_str()),
        );
        let mut channel = Channel::new_fixed(&id, &name, kind);
        channel.ensure_mix_buses(&self.mixes);
        self.channels.push(channel.clone());
        Ok(channel)
    }

    pub fn rename_channel(
        &mut self,
        channel_id: impl AsRef<str>,
        name: impl AsRef<str>,
    ) -> Result<Channel, ModelError> {
        let channel_id = channel_id.as_ref();
        let name = clean_name(name)?;
        if self
            .channels
            .iter()
            .any(|channel| channel.id != channel_id && channel.name.eq_ignore_ascii_case(&name))
        {
            return Err(ModelError::DuplicateChannelName(name));
        }
        let channel = self.channel_mut(channel_id)?;
        channel.name = name;
        Ok(channel.clone())
    }

    pub fn move_channel(
        &mut self,
        channel_id: impl AsRef<str>,
        direction: i32,
    ) -> Result<Channel, ModelError> {
        let channel_id = channel_id.as_ref();
        let index = self
            .channels
            .iter()
            .position(|channel| channel.id == channel_id)
            .ok_or_else(|| ModelError::ChannelNotFound(channel_id.into()))?;
        let target = offset_index(index, direction, self.channels.len());
        if target == index {
            return Ok(self.channels[index].clone());
        }
        let channel = self.channels.remove(index);
        self.channels.insert(target, channel.clone());
        Ok(channel)
    }

    pub fn delete_channel(&mut self, channel_id: impl AsRef<str>) -> Result<Channel, ModelError> {
        if self.channels.len() <= 1 {
            return Err(ModelError::CannotDeleteLastChannel);
        }
        let channel_id = channel_id.as_ref();
        let index = self
            .channels
            .iter()
            .position(|channel| channel.id == channel_id)
            .ok_or_else(|| ModelError::ChannelNotFound(channel_id.into()))?;
        let removed = self.channels.remove(index);
        self.app_routes
            .retain(|route| route.channel_id != removed.id);
        Ok(removed)
    }

    pub fn set_settings(&mut self, settings: MixerSettings) -> MixerSettings {
        self.settings = settings;
        self.settings.keep_running_in_tray = true;
        self.settings.stream_sync_delay_msec =
            clamp_sync_delay_msec(self.settings.stream_sync_delay_msec);
        self.settings.monitor_sync_delay_msec =
            clamp_sync_delay_msec(self.settings.monitor_sync_delay_msec);
        self.settings.clone()
    }

    pub fn set_channel_input_mode(
        &mut self,
        channel_id: impl AsRef<str>,
        _input_mode: ChannelInputMode,
    ) -> Result<Channel, ModelError> {
        let channel = self.channel_mut(channel_id.as_ref())?;
        if !channel.kind.uses_hardware_slot() {
            return Err(ModelError::InvalidConfig(format!(
                "{} is not a hardware input channel",
                channel.name
            )));
        }
        channel.input_mode = ChannelInputMode::SumMono;
        Ok(channel.clone())
    }

    pub fn set_mix_volume(
        &mut self,
        mix_id: impl AsRef<str>,
        volume: f32,
    ) -> Result<Mix, ModelError> {
        let mix = self.mix_mut(mix_id.as_ref())?;
        mix.volume = valid_unit(volume)?;
        Ok(mix.clone())
    }

    pub fn set_mix_mute(
        &mut self,
        mix_id: impl AsRef<str>,
        muted: bool,
    ) -> Result<Mix, ModelError> {
        let mix = self.mix_mut(mix_id.as_ref())?;
        mix.muted = muted;
        Ok(mix.clone())
    }

    pub fn set_mix_icon(
        &mut self,
        mix_id: impl AsRef<str>,
        icon: Option<String>,
    ) -> Result<Mix, ModelError> {
        let icon = clean_optional_icon(icon);
        let mix = self.mix_mut(mix_id.as_ref())?;
        mix.icon = icon;
        Ok(mix.clone())
    }

    pub fn set_channel_icon(
        &mut self,
        channel_id: impl AsRef<str>,
        icon: Option<String>,
    ) -> Result<Channel, ModelError> {
        let icon = clean_optional_icon(icon);
        let channel = self.channel_mut(channel_id.as_ref())?;
        channel.icon = icon;
        Ok(channel.clone())
    }

    pub fn set_mix_monitor_output(
        &mut self,
        mix_id: impl AsRef<str>,
        output: Option<DeviceId>,
    ) -> Result<Mix, ModelError> {
        let output = clean_optional_matcher(output);
        let mix = {
            let mix = self.mix_mut(mix_id.as_ref())?;
            mix.set_outputs(output.iter().cloned().collect());
            mix.clone()
        };
        if mix.id == "monitor" {
            self.device_policy.preferred_output = output;
        }
        Ok(mix)
    }

    pub fn set_mix_outputs(
        &mut self,
        mix_id: impl AsRef<str>,
        outputs: Vec<DeviceId>,
    ) -> Result<Mix, ModelError> {
        let outputs = clean_output_devices(outputs);
        let mix = {
            let mix = self.mix_mut(mix_id.as_ref())?;
            mix.set_outputs(outputs.clone());
            mix.clone()
        };
        if mix.id == "monitor" {
            self.device_policy.preferred_output = mix.monitor_output.clone();
        }
        Ok(mix)
    }

    pub fn set_channel_volume(
        &mut self,
        channel_id: impl AsRef<str>,
        mix_id: impl AsRef<str>,
        volume: f32,
    ) -> Result<MixBus, ModelError> {
        let channel_id = channel_id.as_ref();
        let mix_id = mix_id.as_ref();
        self.ensure_mix_exists(mix_id)?;
        let volume = valid_unit(volume)?;
        let channel = self.channel_mut(channel_id)?;
        if !channel.mix_buses.contains_key(mix_id) {
            return Err(ModelError::MixNotFound(mix_id.into()));
        }
        if channel.linked {
            for bus in channel.mix_buses.values_mut() {
                bus.volume = volume;
            }
        } else if let Some(bus) = channel.mix_buses.get_mut(mix_id) {
            bus.volume = volume;
        }
        Ok(channel
            .mix_buses
            .get(mix_id)
            .expect("bus was checked before update")
            .clone())
    }

    pub fn set_channel_mute(
        &mut self,
        channel_id: impl AsRef<str>,
        mix_id: impl AsRef<str>,
        muted: bool,
    ) -> Result<MixBus, ModelError> {
        let channel_id = channel_id.as_ref();
        let mix_id = mix_id.as_ref();
        self.ensure_mix_exists(mix_id)?;
        let channel = self.channel_mut(channel_id)?;
        let bus = channel
            .mix_buses
            .get_mut(mix_id)
            .ok_or_else(|| ModelError::MixNotFound(mix_id.into()))?;
        bus.muted = muted;
        Ok(bus.clone())
    }

    pub fn set_channel_bus_enabled(
        &mut self,
        channel_id: impl AsRef<str>,
        mix_id: impl AsRef<str>,
        enabled: bool,
    ) -> Result<MixBus, ModelError> {
        let channel_id = channel_id.as_ref();
        let mix_id = mix_id.as_ref();
        self.ensure_mix_exists(mix_id)?;
        let channel = self.channel_mut(channel_id)?;
        let bus = channel
            .mix_buses
            .get_mut(mix_id)
            .ok_or_else(|| ModelError::MixNotFound(mix_id.into()))?;
        bus.enabled = enabled;
        Ok(bus.clone())
    }

    pub fn set_channel_linked(
        &mut self,
        channel_id: impl AsRef<str>,
        linked: bool,
    ) -> Result<Channel, ModelError> {
        let channel = self.channel_mut(channel_id.as_ref())?;
        channel.linked = linked;
        Ok(channel.clone())
    }

    pub fn set_channel_input(
        &mut self,
        channel_id: impl AsRef<str>,
        source_device: Option<DeviceId>,
    ) -> Result<Channel, ModelError> {
        let source_device = clean_optional_device_id(source_device);
        let channel = {
            let channel = self.channel_mut(channel_id.as_ref())?;
            channel.source_device = source_device.clone();
            channel.clone()
        };
        if channel.kind.uses_hardware_slot() {
            self.device_policy.preferred_input = source_device;
        }
        Ok(channel)
    }

    pub fn assign_app_to_channel(
        &mut self,
        channel_id: impl AsRef<str>,
        matcher: AppMatcher,
    ) -> Result<AppRoute, ModelError> {
        let channel_id = channel_id.as_ref();
        self.ensure_channel_exists(channel_id)?;
        let matcher = matcher.normalized().ok_or(ModelError::InvalidMatcher)?;
        let matcher = self.resolve_app_matcher(&matcher);
        let route = AppRoute {
            matcher,
            channel_id: channel_id.into(),
        };
        self.app_routes
            .retain(|existing| !saved_matchers_conflict(&existing.matcher, &route.matcher));
        self.app_routes.push(route.clone());
        Ok(route)
    }

    pub fn remove_app_route(&mut self, matcher: AppMatcher) -> Option<AppRoute> {
        let matcher = matcher.normalized()?;
        let resolved = self.resolve_app_matcher(&matcher);
        let index = self.app_routes.iter().position(|route| {
            saved_matchers_conflict(&route.matcher, &matcher)
                || saved_matchers_conflict(&route.matcher, &resolved)
        })?;
        Some(self.app_routes.remove(index))
    }

    pub fn set_app_volume_preset(
        &mut self,
        matcher: AppMatcher,
        volume: f32,
    ) -> Result<AppVolumePreset, ModelError> {
        let matcher = matcher.normalized().ok_or(ModelError::InvalidMatcher)?;
        let matcher = self.resolve_app_matcher(&matcher);
        let preset = AppVolumePreset {
            matcher,
            volume: valid_unit(volume)?,
        };
        self.app_volume_presets
            .retain(|existing| !saved_matchers_conflict(&existing.matcher, &preset.matcher));
        self.app_volume_presets.push(preset.clone());
        Ok(preset)
    }

    pub fn remove_app_volume_preset(&mut self, matcher: AppMatcher) -> Option<AppVolumePreset> {
        let matcher = matcher.normalized()?;
        let resolved = self.resolve_app_matcher(&matcher);
        let index = self.app_volume_presets.iter().position(|preset| {
            saved_matchers_conflict(&preset.matcher, &matcher)
                || saved_matchers_conflict(&preset.matcher, &resolved)
        })?;
        Some(self.app_volume_presets.remove(index))
    }

    pub fn remember_app_stream(
        &mut self,
        stream: &AppStream,
        seen_unix: i64,
    ) -> Result<Option<KnownApp>, ModelError> {
        let Some(raw_matcher) = AppMatcher::from_stream(stream) else {
            return Ok(None);
        };
        let matcher = self.resolve_app_matcher(&raw_matcher);
        let display_name = self
            .label_for_matcher(&matcher)
            .or_else(|| clean_app_display_name(&stream.display_name))
            .unwrap_or_else(|| matcher_display_name(&matcher));
        let media_name = clean_optional_label(stream.media_name.clone())
            .filter(|value| !is_generic_media_name(value));
        let Some(existing) = self
            .app_history
            .iter_mut()
            .find(|app| app.matcher == matcher)
        else {
            let app = KnownApp {
                matcher,
                display_name,
                media_name,
                last_seen_unix: seen_unix,
                forgotten: false,
            };
            self.app_history.push(app.clone());
            self.normalize_app_history();
            return Ok(Some(app));
        };

        let mut changed = false;
        if existing.display_name != display_name {
            existing.display_name = display_name;
            changed = true;
        }
        if existing.media_name != media_name {
            existing.media_name = media_name;
            changed = true;
        }
        if seen_unix.saturating_sub(existing.last_seen_unix) >= 60 {
            existing.last_seen_unix = seen_unix;
            changed = true;
        }
        if changed {
            let app = existing.clone();
            self.normalize_app_history();
            Ok(Some(app))
        } else {
            Ok(None)
        }
    }

    pub fn forget_app(&mut self, matcher: AppMatcher) -> Option<KnownApp> {
        let matcher = matcher.normalized()?;
        let resolved = self.resolve_app_matcher(&matcher);
        self.app_routes.retain(|route| {
            !app_matchers_overlap(&route.matcher, &matcher)
                && !app_matchers_overlap(&route.matcher, &resolved)
        });
        self.app_volume_presets.retain(|preset| {
            !app_matchers_overlap(&preset.matcher, &matcher)
                && !app_matchers_overlap(&preset.matcher, &resolved)
        });
        let app = self.app_history.iter_mut().find(|app| {
            app_matchers_overlap(&app.matcher, &matcher)
                || app_matchers_overlap(&app.matcher, &resolved)
        })?;
        app.forgotten = true;
        Some(app.clone())
    }

    pub fn restore_app(&mut self, matcher: AppMatcher) -> Option<KnownApp> {
        let matcher = matcher.normalized()?;
        let resolved = self.resolve_app_matcher(&matcher);
        let app = self.app_history.iter_mut().find(|app| {
            app_matchers_overlap(&app.matcher, &matcher)
                || app_matchers_overlap(&app.matcher, &resolved)
        })?;
        app.forgotten = false;
        Some(app.clone())
    }

    pub fn pin_app_identity(
        &mut self,
        matcher: AppMatcher,
        label: impl AsRef<str>,
    ) -> Result<KnownApp, ModelError> {
        let matcher = matcher.normalized().ok_or(ModelError::InvalidMatcher)?;
        let matcher = self.resolve_app_matcher(&matcher);
        let label = clean_app_display_name(label.as_ref()).ok_or(ModelError::InvalidName)?;
        self.app_label_overrides
            .retain(|existing| !saved_matchers_conflict(&existing.matcher, &matcher));
        self.app_label_overrides.push(AppLabelOverride {
            matcher: matcher.clone(),
            label: label.clone(),
        });

        let app = self.upsert_known_app(matcher, label, None, 0, false);
        self.normalize_app_identity();
        Ok(app)
    }

    pub fn merge_app_identity(
        &mut self,
        source: AppMatcher,
        target: AppMatcher,
    ) -> Result<KnownApp, ModelError> {
        let source = source.normalized().ok_or(ModelError::InvalidMatcher)?;
        let target = target.normalized().ok_or(ModelError::InvalidMatcher)?;
        if app_matchers_overlap(&source, &target) {
            return Err(ModelError::InvalidMatcher);
        }
        let target = self.resolve_app_matcher(&target);
        let label = self
            .label_for_matcher(&target)
            .or_else(|| {
                self.app_history
                    .iter()
                    .find(|app| app_matchers_overlap(&app.matcher, &target))
                    .map(|app| app.display_name.clone())
            })
            .unwrap_or_else(|| matcher_display_name(&target));

        self.app_identity_overrides
            .retain(|existing| !app_matchers_overlap(&existing.source, &source));
        self.app_identity_overrides.push(AppIdentityOverride {
            source: source.clone(),
            target: target.clone(),
        });

        for route in &mut self.app_routes {
            if app_matchers_overlap(&route.matcher, &source) {
                route.matcher = target.clone();
            }
        }
        for preset in &mut self.app_volume_presets {
            if app_matchers_overlap(&preset.matcher, &source) {
                preset.matcher = target.clone();
            }
        }
        for app in &mut self.app_history {
            if app_matchers_overlap(&app.matcher, &source) {
                app.forgotten = true;
            }
        }

        let app = self.upsert_known_app(target, label, None, 0, false);
        self.normalize_app_identity();
        self.normalize_app_routes();
        self.normalize_app_volume_presets();
        self.normalize_app_history();
        Ok(app)
    }

    pub fn reset_app_identity(&mut self, matcher: AppMatcher) -> Option<KnownApp> {
        let matcher = matcher.normalized()?;
        self.app_identity_overrides.retain(|override_rule| {
            !app_matchers_overlap(&override_rule.source, &matcher)
                && !app_matchers_overlap(&override_rule.target, &matcher)
        });
        self.app_label_overrides
            .retain(|label| !app_matchers_overlap(&label.matcher, &matcher));

        let app = self
            .app_history
            .iter_mut()
            .find(|app| app_matchers_overlap(&app.matcher, &matcher))?;
        app.display_name = matcher_display_name(&app.matcher);
        app.forgotten = false;
        Some(app.clone())
    }

    pub fn set_preferred_output(&mut self, output: Option<DeviceId>) {
        self.device_policy.preferred_output = clean_optional_matcher(output);
    }

    pub fn set_restorable_input(&mut self, input: Option<DeviceId>) {
        self.device_policy.restorable_input = clean_optional_matcher(input);
    }

    pub fn set_restorable_output(&mut self, output: Option<DeviceId>) {
        self.device_policy.restorable_output = clean_optional_matcher(output);
    }

    pub fn set_input_fallback_active(&mut self, active: bool) {
        self.device_policy.active_input_fallback = active;
    }

    pub fn set_output_fallback_active(&mut self, active: bool) {
        self.device_policy.active_output_fallback = active;
    }

    pub fn resolve_app_matcher(&self, matcher: &AppMatcher) -> AppMatcher {
        self.app_identity_overrides
            .iter()
            .filter(|override_rule| app_matchers_overlap(&override_rule.source, matcher))
            .max_by_key(|override_rule| app_matcher_specificity(&override_rule.source))
            .map(|override_rule| override_rule.target.clone())
            .unwrap_or_else(|| matcher.clone())
    }

    pub fn label_for_matcher(&self, matcher: &AppMatcher) -> Option<String> {
        self.app_label_overrides
            .iter()
            .filter(|label| app_matchers_overlap(&label.matcher, matcher))
            .max_by_key(|label| app_matcher_specificity(&label.matcher))
            .map(|label| label.label.clone())
    }

    fn upsert_known_app(
        &mut self,
        matcher: AppMatcher,
        display_name: String,
        media_name: Option<String>,
        seen_unix: i64,
        forgotten: bool,
    ) -> KnownApp {
        if let Some(app) = self
            .app_history
            .iter_mut()
            .find(|app| app_matchers_overlap(&app.matcher, &matcher))
        {
            app.display_name = display_name;
            if media_name.is_some() {
                app.media_name = media_name;
            }
            if seen_unix > 0 {
                app.last_seen_unix = seen_unix;
            }
            app.forgotten = forgotten;
            return app.clone();
        }

        let app = KnownApp {
            matcher,
            display_name,
            media_name,
            last_seen_unix: seen_unix,
            forgotten,
        };
        self.app_history.push(app.clone());
        app
    }

    pub fn set_effect_chain(
        &mut self,
        channel_id: impl AsRef<str>,
        effects: Vec<EffectInstance>,
    ) -> Result<Channel, ModelError> {
        let catalog = EffectCatalog::default();
        let effects = normalize_effect_chain(effects, &catalog, false)?;
        let channel = self.channel_mut(channel_id.as_ref())?;
        channel.effects = effects;
        Ok(channel.clone())
    }

    pub fn ensure_streamer_binding_profiles(
        &mut self,
        profiles: Vec<StreamerBindingProfile>,
    ) -> StreamerDevicesConfig {
        for profile in profiles {
            let Some(profile) = profile.normalized() else {
                continue;
            };
            self.streamer_devices
                .profiles
                .entry(profile.device_id.clone())
                .or_insert(profile);
        }
        self.normalize_streamer_devices();
        self.streamer_devices.clone()
    }

    pub fn set_streamer_device_enabled(
        &mut self,
        device_id: impl Into<String>,
        enabled: bool,
    ) -> Result<StreamerDevicesConfig, ModelError> {
        let device_id = clean_streamer_id(device_id.into()).ok_or(ModelError::InvalidName)?;
        let profile = self
            .streamer_devices
            .profiles
            .entry(device_id.clone())
            .or_insert_with(|| StreamerBindingProfile::new(device_id));
        profile.enabled = enabled;
        profile.safe_preset = false;
        self.normalize_streamer_devices();
        Ok(self.streamer_devices.clone())
    }

    pub fn set_streamer_binding_profile(
        &mut self,
        profile: StreamerBindingProfile,
    ) -> Result<StreamerBindingProfile, ModelError> {
        let profile = profile.normalized().ok_or(ModelError::InvalidName)?;
        self.streamer_devices
            .profiles
            .insert(profile.device_id.clone(), profile.clone());
        self.normalize_streamer_devices();
        Ok(profile)
    }

    pub fn set_effect_param(
        &mut self,
        channel_id: impl AsRef<str>,
        instance_id: impl AsRef<str>,
        param_id: impl AsRef<str>,
        value: f32,
    ) -> Result<Channel, ModelError> {
        let instance_id = instance_id.as_ref();
        let param_id = param_id.as_ref();
        let channel = self.channel_mut(channel_id.as_ref())?;
        let effect = channel
            .effects
            .iter_mut()
            .find(|effect| effect.instance_id == instance_id)
            .ok_or_else(|| ModelError::EffectNotFound(instance_id.into()))?;
        let catalog = EffectCatalog::default();
        let definition = effect_definition(&catalog, &effect.effect_id)?;
        let param = effect_param_definition(definition, param_id)?;
        effect
            .params
            .insert(param_id.into(), clamp_param_value(value, param));
        Ok(channel.clone())
    }

    pub fn bypass_effect(
        &mut self,
        channel_id: impl AsRef<str>,
        instance_id: impl AsRef<str>,
        bypassed: bool,
    ) -> Result<Channel, ModelError> {
        let instance_id = instance_id.as_ref();
        let channel = self.channel_mut(channel_id.as_ref())?;
        let effect = channel
            .effects
            .iter_mut()
            .find(|effect| effect.instance_id == instance_id)
            .ok_or_else(|| ModelError::EffectNotFound(instance_id.into()))?;
        effect.bypassed = bypassed;
        if !bypassed {
            channel.effects = keep_one_single_instance_effect_per_channel(
                std::mem::take(&mut channel.effects),
                Some(instance_id),
            );
        }
        Ok(channel.clone())
    }

    pub fn channel_mut(&mut self, channel_id: &str) -> Result<&mut Channel, ModelError> {
        self.channels
            .iter_mut()
            .find(|channel| channel.id == channel_id)
            .ok_or_else(|| ModelError::ChannelNotFound(channel_id.into()))
    }

    pub fn mix_mut(&mut self, mix_id: &str) -> Result<&mut Mix, ModelError> {
        self.mixes
            .iter_mut()
            .find(|mix| mix.id == mix_id)
            .ok_or_else(|| ModelError::MixNotFound(mix_id.into()))
    }

    pub fn ensure_mix_exists(&self, mix_id: &str) -> Result<(), ModelError> {
        if self.mixes.iter().any(|mix| mix.id == mix_id) {
            Ok(())
        } else {
            Err(ModelError::MixNotFound(mix_id.into()))
        }
    }

    pub fn ensure_channel_exists(&self, channel_id: &str) -> Result<(), ModelError> {
        if self.channels.iter().any(|channel| channel.id == channel_id) {
            Ok(())
        } else {
            Err(ModelError::ChannelNotFound(channel_id.into()))
        }
    }

    pub fn software_channel_count(&self) -> usize {
        self.channels
            .iter()
            .filter(|channel| !channel.kind.uses_hardware_slot())
            .count()
    }

    pub fn hardware_input_count(&self) -> usize {
        self.channels
            .iter()
            .filter(|channel| channel.kind.uses_hardware_slot())
            .count()
    }

    fn mix_name_exists(&self, name: &str) -> bool {
        self.mixes
            .iter()
            .any(|mix| mix.name.eq_ignore_ascii_case(name))
    }

    fn channel_name_exists(&self, name: &str) -> bool {
        self.channels
            .iter()
            .any(|channel| channel.name.eq_ignore_ascii_case(name))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AudioSpec {
    pub sample_rate_hz: u32,
    pub bit_depth: u16,
    pub channel_layout: String,
    pub mono_inputs_to_stereo: bool,
}

impl Default for AudioSpec {
    fn default() -> Self {
        Self {
            sample_rate_hz: SAMPLE_RATE_HZ,
            bit_depth: BIT_DEPTH,
            channel_layout: CHANNEL_LAYOUT.into(),
            mono_inputs_to_stereo: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Mix {
    pub id: MixId,
    pub name: String,
    #[serde(default)]
    pub virtual_sink_name: String,
    #[serde(default)]
    pub virtual_source_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_output: Option<DeviceId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_devices: Vec<DeviceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default = "default_unit_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
}

impl Mix {
    pub fn new_fixed(id: &str, name: &str) -> Self {
        let safe = safe_node_id(id);
        Self {
            id: id.into(),
            name: name.into(),
            virtual_sink_name: format!("wavelinux_mix_{safe}"),
            virtual_source_name: format!("wavelinux_mix_{safe}_source"),
            monitor_output: None,
            output_devices: Vec::new(),
            icon: None,
            volume: 1.0,
            muted: false,
        }
    }

    fn ensure_virtual_node_names(&mut self) {
        let safe = safe_node_id(&self.id);
        if self.virtual_sink_name.trim().is_empty() {
            self.virtual_sink_name = format!("wavelinux_mix_{safe}");
        }
        if self.virtual_source_name.trim().is_empty() {
            self.virtual_source_name = format!("wavelinux_mix_{safe}_source");
        }
    }

    pub fn outputs(&self) -> Vec<DeviceId> {
        let outputs = clean_output_devices(self.output_devices.clone());
        if outputs.is_empty() {
            self.monitor_output.iter().cloned().collect()
        } else {
            outputs
        }
    }

    pub fn set_outputs(&mut self, outputs: Vec<DeviceId>) {
        self.output_devices = clean_output_devices(outputs);
        self.monitor_output = self.output_devices.first().cloned();
    }

    fn normalize_outputs(&mut self) {
        let mut outputs = clean_output_devices(std::mem::take(&mut self.output_devices));
        if outputs.is_empty() {
            if let Some(output) = clean_optional_matcher(self.monitor_output.take()) {
                outputs.push(output);
            }
        }
        self.set_outputs(outputs);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Microphone,
    Application,
    Soundboard,
    System,
    Generic,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelInputMode {
    #[default]
    Stereo,
    MonoLeft,
    MonoRight,
    SumMono,
    SwapLr,
}

impl ChannelInputMode {
    pub fn id(self) -> &'static str {
        match self {
            Self::Stereo => "stereo",
            Self::MonoLeft => "mono_left",
            Self::MonoRight => "mono_right",
            Self::SumMono => "sum_mono",
            Self::SwapLr => "swap_lr",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Stereo => "Stereo",
            Self::MonoLeft => "Mono left to stereo",
            Self::MonoRight => "Mono right to stereo",
            Self::SumMono => "Sum to mono",
            Self::SwapLr => "Swap left/right",
        }
    }

    pub fn channel_map(self) -> &'static str {
        match self {
            Self::Stereo => "front-left,front-right",
            Self::MonoLeft => "front-left,front-left",
            Self::MonoRight => "front-right,front-right",
            Self::SumMono => "mono",
            Self::SwapLr => "front-right,front-left",
        }
    }

    pub fn channels(self) -> u8 {
        match self {
            Self::SumMono => 1,
            Self::Stereo | Self::MonoLeft | Self::MonoRight | Self::SwapLr => 2,
        }
    }
}

impl ChannelKind {
    pub fn uses_hardware_slot(&self) -> bool {
        matches!(self, Self::Microphone | Self::Generic)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub kind: ChannelKind,
    #[serde(default)]
    pub virtual_sink_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_device: Option<DeviceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub input_mode: ChannelInputMode,
    #[serde(default)]
    pub linked: bool,
    #[serde(default)]
    pub mix_buses: BTreeMap<MixId, MixBus>,
    #[serde(default)]
    pub app_matchers: Vec<AppMatcher>,
    #[serde(default)]
    pub effects: Vec<EffectInstance>,
}

impl Channel {
    pub fn new_fixed(id: &str, name: &str, kind: ChannelKind) -> Self {
        let safe = safe_node_id(id);
        let input_mode = if kind.uses_hardware_slot() {
            ChannelInputMode::SumMono
        } else {
            ChannelInputMode::Stereo
        };
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            virtual_sink_name: format!("wavelinux_channel_{safe}"),
            source_device: None,
            icon: None,
            input_mode,
            linked: false,
            mix_buses: BTreeMap::new(),
            app_matchers: Vec::new(),
            effects: Vec::new(),
        }
    }

    pub fn ensure_mix_buses(&mut self, mixes: &[Mix]) {
        let valid: BTreeSet<_> = mixes.iter().map(|mix| mix.id.clone()).collect();
        self.mix_buses.retain(|mix_id, _| valid.contains(mix_id));
        for mix in mixes {
            self.mix_buses.entry(mix.id.clone()).or_default();
        }
    }

    fn ensure_virtual_node_name(&mut self) {
        if self.virtual_sink_name.trim().is_empty() {
            self.virtual_sink_name = format!("wavelinux_channel_{}", safe_node_id(&self.id));
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MixBus {
    #[serde(default = "default_unit_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for MixBus {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
            enabled: true,
        }
    }
}

impl MixBus {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppMatcher {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_name: Option<String>,
}

impl AppMatcher {
    pub fn from_app_id(app_id: impl Into<String>) -> Self {
        Self {
            app_id: Some(app_id.into()),
            binary: None,
            process_name: None,
            window_class: None,
            media_name: None,
        }
    }

    pub fn from_process_name(process_name: impl Into<String>) -> Self {
        Self {
            app_id: None,
            binary: None,
            process_name: Some(process_name.into()),
            window_class: None,
            media_name: None,
        }
    }

    pub fn from_binary(binary: impl Into<String>) -> Self {
        Self {
            app_id: None,
            binary: Some(binary.into()),
            process_name: None,
            window_class: None,
            media_name: None,
        }
    }

    pub fn from_window_class(window_class: impl Into<String>) -> Self {
        Self {
            app_id: None,
            binary: None,
            process_name: None,
            window_class: Some(window_class.into()),
            media_name: None,
        }
    }

    pub fn from_media_name(media_name: impl Into<String>) -> Self {
        Self {
            app_id: None,
            binary: None,
            process_name: None,
            window_class: None,
            media_name: Some(media_name.into()),
        }
    }

    pub fn from_stream(stream: &AppStream) -> Option<Self> {
        let mut app_id = stream.app_id.clone();
        let mut binary = stream
            .binary
            .clone()
            .or_else(|| stream.process_name.clone());
        let mut process_name = stream.process_name.clone();
        let window_class = stream.window_class.clone();
        if app_id.is_none() && binary.is_none() && process_name.is_none() && window_class.is_none()
        {
            process_name = stable_stream_display_name(stream);
            binary = process_name.clone();
            app_id = process_name.clone();
        }

        Self {
            app_id,
            binary,
            process_name,
            window_class,
            media_name: stream.media_name.clone(),
        }
        .normalized()
    }

    pub fn is_empty(&self) -> bool {
        self.app_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
            && self
                .binary
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            && self
                .process_name
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            && self
                .window_class
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            && self
                .media_name
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
    }

    fn normalized(self) -> Option<Self> {
        let mut matcher = Self {
            app_id: clean_optional_matcher(self.app_id),
            binary: clean_optional_matcher(self.binary),
            process_name: clean_optional_matcher(self.process_name),
            window_class: clean_optional_matcher(self.window_class),
            media_name: clean_optional_matcher(self.media_name),
        };
        if !matcher_should_keep_media_name(&matcher) {
            matcher.media_name = None;
        }
        (!matcher.is_empty()).then_some(matcher)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppRoute {
    pub matcher: AppMatcher,
    pub channel_id: ChannelId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppVolumePreset {
    pub matcher: AppMatcher,
    #[serde(default = "default_unit_volume")]
    pub volume: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnownApp {
    pub matcher: AppMatcher,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_name: Option<String>,
    pub last_seen_unix: i64,
    #[serde(default)]
    pub forgotten: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppIdentityOverride {
    pub source: AppMatcher,
    pub target: AppMatcher,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppLabelOverride {
    pub matcher: AppMatcher,
    pub label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DevicePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_input: Option<DeviceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_output: Option<DeviceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restorable_input: Option<DeviceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restorable_output: Option<DeviceId>,
    #[serde(default)]
    pub active_input_fallback: bool,
    #[serde(default)]
    pub active_output_fallback: bool,
    #[serde(default)]
    pub hardware_profile_assignments: BTreeMap<DeviceId, String>,
    #[serde(default)]
    pub fallback_hardware_profile: FallbackHardwareProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StreamerDevicesConfig {
    pub version: u32,
    pub profiles: BTreeMap<String, StreamerBindingProfile>,
}

impl StreamerDevicesConfig {
    pub fn normalized(mut self) -> Self {
        self.version = STREAMER_DEVICES_CONFIG_VERSION;
        self.profiles = self
            .profiles
            .into_values()
            .filter_map(StreamerBindingProfile::normalized)
            .map(|profile| (profile.device_id.clone(), profile))
            .collect();
        self
    }
}

impl Default for StreamerDevicesConfig {
    fn default() -> Self {
        Self {
            version: STREAMER_DEVICES_CONFIG_VERSION,
            profiles: BTreeMap::new(),
        }
    }
}

const STREAMER_DEVICES_CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum StreamerDeviceFamily {
    StreamDeck,
    Rode,
    GoXlr,
    MidiSurface,
    Loupedeck,
    XKeys,
    UnknownSupported,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum StreamerTransport {
    Hid,
    Midi,
    AudioProfile,
    Bridge,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamerPermissionStatus {
    Ready,
    PermissionDenied,
    Busy,
    MissingRuntime,
    UnsupportedProtocol,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StreamerDeviceCapabilities {
    pub buttons: bool,
    pub dials: bool,
    pub faders: bool,
    pub pads: bool,
    pub display_feedback: bool,
    pub midi_feedback: bool,
    pub audio_endpoint: bool,
}

impl Default for StreamerDeviceCapabilities {
    fn default() -> Self {
        Self {
            buttons: false,
            dials: false,
            faders: false,
            pads: false,
            display_feedback: false,
            midi_feedback: false,
            audio_endpoint: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamerControlKind {
    Button,
    Dial,
    Fader,
    Pad,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StreamerDeviceSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub family: StreamerDeviceFamily,
    pub transport: StreamerTransport,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub capabilities: StreamerDeviceCapabilities,
    pub connected: bool,
    pub enabled: bool,
    pub permission_status: StreamerPermissionStatus,
    pub matched_profile_id: Option<String>,
    pub source: String,
    pub message: String,
}

impl Default for StreamerDeviceSummary {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            family: StreamerDeviceFamily::UnknownSupported,
            transport: StreamerTransport::AudioProfile,
            vendor_id: None,
            product_id: None,
            capabilities: StreamerDeviceCapabilities::default(),
            connected: false,
            enabled: true,
            permission_status: StreamerPermissionStatus::UnsupportedProtocol,
            matched_profile_id: None,
            source: String::new(),
            message: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StreamerBindingProfile {
    pub device_id: String,
    pub family: Option<StreamerDeviceFamily>,
    pub name: String,
    pub enabled: bool,
    pub safe_preset: bool,
    pub bindings: Vec<StreamerBinding>,
}

impl StreamerBindingProfile {
    pub fn new(device_id: String) -> Self {
        Self {
            device_id,
            family: None,
            name: "Streamer Device".into(),
            enabled: true,
            safe_preset: false,
            bindings: Vec::new(),
        }
    }

    fn normalized(mut self) -> Option<Self> {
        self.device_id = clean_streamer_id(self.device_id)?;
        self.name = clean_app_display_name(&self.name).unwrap_or_else(|| "Streamer Device".into());
        self.bindings = self
            .bindings
            .into_iter()
            .filter_map(StreamerBinding::normalized)
            .collect();
        Some(self)
    }
}

impl Default for StreamerBindingProfile {
    fn default() -> Self {
        Self::new(String::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StreamerBinding {
    pub control_id: String,
    pub label: String,
    pub control_kind: StreamerControlKind,
    pub action: StreamerAction,
}

impl StreamerBinding {
    fn normalized(mut self) -> Option<Self> {
        self.control_id = clean_streamer_id(self.control_id)?;
        self.label = clean_app_display_name(&self.label).unwrap_or_else(|| self.control_id.clone());
        self.action = self.action.without_audio_lifecycle();
        Some(self)
    }
}

impl Default for StreamerBinding {
    fn default() -> Self {
        Self {
            control_id: String::new(),
            label: String::new(),
            control_kind: StreamerControlKind::Unknown,
            action: StreamerAction::Noop,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamerAction {
    Noop,
    MixMuteToggle {
        mix_id: MixId,
    },
    MixVolumeSet {
        mix_id: MixId,
        volume: f32,
    },
    MixVolumeSetFromControl {
        mix_id: MixId,
    },
    MixVolumeAdjust {
        mix_id: MixId,
        delta: f32,
    },
    ChannelMuteToggle {
        channel_id: ChannelId,
        mix_id: MixId,
    },
    ChannelBusEnabledToggle {
        channel_id: ChannelId,
        mix_id: MixId,
    },
    ChannelVolumeSet {
        channel_id: ChannelId,
        mix_id: MixId,
        volume: f32,
    },
    ChannelVolumeSetFromControl {
        channel_id: ChannelId,
        mix_id: MixId,
    },
    ChannelVolumeAdjust {
        channel_id: ChannelId,
        mix_id: MixId,
        delta: f32,
    },
    EffectBypassToggle {
        channel_id: ChannelId,
        instance_id: EffectInstanceId,
    },
    StartOrRepairAudio,
    CleanupAudioGraph,
    CleanupStaleAudioGraph,
}

impl Default for StreamerAction {
    fn default() -> Self {
        Self::Noop
    }
}

impl StreamerAction {
    fn without_audio_lifecycle(self) -> Self {
        match self {
            Self::StartOrRepairAudio | Self::CleanupAudioGraph => Self::Noop,
            action => action,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StreamerLearnResult {
    pub device_id: String,
    pub control_id: Option<String>,
    pub control_kind: StreamerControlKind,
    pub message: String,
}

impl Default for StreamerLearnResult {
    fn default() -> Self {
        Self {
            device_id: String::new(),
            control_id: None,
            control_kind: StreamerControlKind::Unknown,
            message: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StreamerActionResult {
    pub performed: bool,
    pub message: String,
}

impl Default for StreamerActionResult {
    fn default() -> Self {
        Self {
            performed: false,
            message: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct FallbackHardwareProfile {
    pub id: String,
    pub name: String,
    pub latency_policy: LatencyPolicy,
    pub routing_policy: RoutingPolicy,
    pub bluetooth_mic_policy: BluetoothMicPolicy,
    pub confidence: ProfileConfidence,
}

impl FallbackHardwareProfile {
    pub fn normalized(mut self) -> Self {
        self.id = clean_optional_profile_id(Some(self.id))
            .unwrap_or_else(default_fallback_hardware_profile_id);
        self.name = clean_app_display_name(&self.name)
            .unwrap_or_else(|| default_fallback_hardware_profile_name().to_string());
        normalize_latency_policy(&mut self.latency_policy);
        if self.id == default_fallback_hardware_profile_id()
            && self.name == default_fallback_hardware_profile_name()
            && matches!(
                self.latency_policy,
                LatencyPolicy {
                    stable_msec: Some(35),
                    low_latency_msec: Some(20),
                    bluetooth_floor_msec: Some(120 | 180),
                }
            )
        {
            self.latency_policy = FallbackHardwareProfile::default().latency_policy;
        }
        normalize_routing_policy(&mut self.routing_policy);
        if self.bluetooth_mic_policy != BluetoothMicPolicy::NeverIfHfp {
            self.bluetooth_mic_policy = BluetoothMicPolicy::NeverIfHfp;
        }
        self
    }
}

impl Default for FallbackHardwareProfile {
    fn default() -> Self {
        Self {
            id: default_fallback_hardware_profile_id(),
            name: default_fallback_hardware_profile_name().into(),
            latency_policy: LatencyPolicy {
                stable_msec: Some(80),
                low_latency_msec: Some(60),
                bluetooth_floor_msec: Some(240),
            },
            routing_policy: RoutingPolicy {
                input_priority: Some(35),
                output_priority: Some(30),
                allow_auto_select_input: true,
                allow_auto_select_output: true,
                prefer_non_bluetooth_input: true,
            },
            bluetooth_mic_policy: BluetoothMicPolicy::NeverIfHfp,
            confidence: ProfileConfidence::Low,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectInstance {
    #[serde(default)]
    pub instance_id: EffectInstanceId,
    pub effect_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub bypassed: bool,
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
}

impl EffectInstance {
    pub fn new(effect_id: impl Into<String>) -> Self {
        Self {
            instance_id: Uuid::new_v4().to_string(),
            effect_id: effect_id.into(),
            name: None,
            bypassed: false,
            params: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub plugin_hint: PluginHint,
    pub params: Vec<EffectParamDefinition>,
    pub presets: Vec<EffectPreset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PluginHint {
    PipeWireBuiltin,
    Ladspa { library_names: Vec<String> },
    Lv2 { uri_hint: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectParamDefinition {
    pub id: String,
    pub label: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectPreset {
    pub name: String,
    pub values: BTreeMap<String, f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectCatalog {
    pub effects: Vec<EffectDefinition>,
    pub preferred_order: Vec<String>,
}

impl Default for EffectCatalog {
    fn default() -> Self {
        let mut effects = vec![
            effect(
                "deepfilternet",
                "DeepFilterNet3",
                "DeepFilterNet3 neural noise suppression",
                PluginHint::Ladspa {
                    library_names: vec![
                        "libdeep_filter_ladspa.so".into(),
                        "deep_filter_ladspa.so".into(),
                        "libdeepfilternet_ladspa.so".into(),
                        "deepfilternet_ladspa.so".into(),
                        "libdeep_filter_net_ladspa.so".into(),
                        "deep_filter_net_ladspa.so".into(),
                    ],
                },
                vec![
                    param("input_trim_db", "Input Trim", -24.0, 0.0, -6.0, " dB"),
                    param("output_makeup_db", "Output Makeup", 0.0, 18.0, 6.0, " dB"),
                    param(
                        "attenuation_limit_db",
                        "Reduction Limit",
                        0.0,
                        100.0,
                        18.0,
                        " dB",
                    ),
                    param(
                        "min_processing_threshold_db",
                        "Min Threshold",
                        -15.0,
                        35.0,
                        -15.0,
                        " dB",
                    ),
                    param(
                        "max_erb_processing_threshold_db",
                        "Max ERB Threshold",
                        -15.0,
                        35.0,
                        30.0,
                        " dB",
                    ),
                    param(
                        "max_df_processing_threshold_db",
                        "Max DF Threshold",
                        -15.0,
                        35.0,
                        20.0,
                        " dB",
                    ),
                    param(
                        "min_processing_buffer_frames",
                        "Min Buffer",
                        0.0,
                        10.0,
                        8.0,
                        " frames",
                    ),
                    param("post_filter_beta", "Post Filter Beta", 0.0, 0.05, 0.0, ""),
                ],
            ),
            effect(
                "rnnoise",
                "Noise Suppression",
                "RNNoise speech noise suppression",
                PluginHint::Ladspa {
                    library_names: vec!["librnnoise_ladspa.so".into(), "rnnoise_ladspa.so".into()],
                },
                vec![
                    param("vad_threshold", "VAD Threshold", 0.0, 99.0, 50.0, "%"),
                    param("hold_ms", "Hold Open", 0.0, 1000.0, 200.0, " ms"),
                    param("lead_in_ms", "Lead-In", 0.0, 200.0, 0.0, " ms"),
                ],
            ),
            effect(
                "highpass",
                "High-Pass Filter",
                "Rumble removal",
                PluginHint::PipeWireBuiltin,
                vec![param("frequency_hz", "Cutoff", 20.0, 500.0, 80.0, " Hz")],
            ),
            effect(
                "eq",
                "3-Band EQ",
                "Low, mid, and high tone shaping",
                PluginHint::PipeWireBuiltin,
                vec![
                    param("low_freq_hz", "Low Freq", 40.0, 400.0, 120.0, " Hz"),
                    param("low_gain_db", "Low Gain", -12.0, 12.0, 0.0, " dB"),
                    param("mid_freq_hz", "Mid Freq", 300.0, 4000.0, 1000.0, " Hz"),
                    param("mid_gain_db", "Mid Gain", -12.0, 12.0, 0.0, " dB"),
                    param("high_freq_hz", "High Freq", 2000.0, 12000.0, 6000.0, " Hz"),
                    param("high_gain_db", "High Gain", -12.0, 12.0, 0.0, " dB"),
                ],
            ),
            effect(
                "compressor",
                "Compressor",
                "Dynamic range control",
                PluginHint::Ladspa {
                    library_names: vec!["sc4_1882.so".into(), "compressor.so".into()],
                },
                vec![
                    param("threshold_db", "Threshold", -30.0, 0.0, -20.0, " dB"),
                    param("ratio", "Ratio", 1.0, 20.0, 4.0, ":1"),
                    param("attack_ms", "Attack", 1.5, 200.0, 5.0, " ms"),
                    param("release_ms", "Release", 5.0, 800.0, 100.0, " ms"),
                    param("makeup_gain_db", "Makeup", 0.0, 24.0, 0.0, " dB"),
                ],
            ),
            effect(
                "gate",
                "Noise Gate",
                "Attenuate quiet room tone",
                PluginHint::Ladspa {
                    library_names: vec!["gate_1410.so".into()],
                },
                vec![
                    param("threshold_db", "Threshold", -80.0, 0.0, -40.0, " dB"),
                    param("attack_ms", "Attack", 0.1, 100.0, 2.5, " ms"),
                    param("hold_ms", "Hold", 0.0, 500.0, 10.0, " ms"),
                    param("release_ms", "Release", 10.0, 2000.0, 200.0, " ms"),
                    param("range_db", "Range", -80.0, 0.0, -40.0, " dB"),
                ],
            ),
            effect(
                "limiter",
                "Limiter",
                "Brick-wall peak ceiling",
                PluginHint::Ladspa {
                    library_names: vec![
                        "fast_lookahead_limiter_1913.so".into(),
                        "hard_limiter_1413.so".into(),
                    ],
                },
                vec![
                    param("input_gain_db", "Input Gain", -20.0, 20.0, 0.0, " dB"),
                    param("ceiling_db", "Ceiling", -20.0, 0.0, -1.0, " dB"),
                ],
            ),
        ];

        set_presets(
            &mut effects,
            "deepfilternet",
            vec![
                preset(
                    "Balanced Voice",
                    &[
                        ("input_trim_db", -6.0),
                        ("output_makeup_db", 6.0),
                        ("attenuation_limit_db", 18.0),
                        ("min_processing_threshold_db", -15.0),
                        ("max_erb_processing_threshold_db", 30.0),
                        ("max_df_processing_threshold_db", 20.0),
                        ("min_processing_buffer_frames", 8.0),
                        ("post_filter_beta", 0.0),
                    ],
                ),
                preset(
                    "Natural Voice",
                    &[
                        ("input_trim_db", -3.0),
                        ("output_makeup_db", 3.0),
                        ("attenuation_limit_db", 12.0),
                        ("min_processing_threshold_db", -15.0),
                        ("max_erb_processing_threshold_db", 30.0),
                        ("max_df_processing_threshold_db", 10.0),
                        ("min_processing_buffer_frames", 6.0),
                        ("post_filter_beta", 0.0),
                    ],
                ),
                preset(
                    "Noisy Room",
                    &[
                        ("input_trim_db", -6.0),
                        ("output_makeup_db", 6.0),
                        ("attenuation_limit_db", 70.0),
                        ("min_processing_threshold_db", -15.0),
                        ("max_erb_processing_threshold_db", 30.0),
                        ("max_df_processing_threshold_db", 20.0),
                        ("min_processing_buffer_frames", 8.0),
                        ("post_filter_beta", 0.0),
                    ],
                ),
            ],
        );
        set_presets(
            &mut effects,
            "rnnoise",
            vec![
                preset(
                    "Gentle",
                    &[
                        ("vad_threshold", 25.0),
                        ("hold_ms", 250.0),
                        ("lead_in_ms", 0.0),
                    ],
                ),
                preset(
                    "Broadcast",
                    &[
                        ("vad_threshold", 50.0),
                        ("hold_ms", 200.0),
                        ("lead_in_ms", 0.0),
                    ],
                ),
                preset(
                    "Aggressive",
                    &[
                        ("vad_threshold", 75.0),
                        ("hold_ms", 150.0),
                        ("lead_in_ms", 0.0),
                    ],
                ),
            ],
        );
        set_presets(
            &mut effects,
            "highpass",
            vec![
                preset("Voice 80 Hz", &[("frequency_hz", 80.0)]),
                preset("Rumble 120 Hz", &[("frequency_hz", 120.0)]),
                preset("Music 40 Hz", &[("frequency_hz", 40.0)]),
            ],
        );
        set_presets(
            &mut effects,
            "eq",
            vec![
                preset(
                    "Flat",
                    &[
                        ("low_freq_hz", 120.0),
                        ("low_gain_db", 0.0),
                        ("mid_freq_hz", 1000.0),
                        ("mid_gain_db", 0.0),
                        ("high_freq_hz", 6000.0),
                        ("high_gain_db", 0.0),
                    ],
                ),
                preset(
                    "Broadcast Voice",
                    &[
                        ("low_freq_hz", 120.0),
                        ("low_gain_db", -2.0),
                        ("mid_freq_hz", 2500.0),
                        ("mid_gain_db", 2.0),
                        ("high_freq_hz", 8000.0),
                        ("high_gain_db", 1.5),
                    ],
                ),
                preset(
                    "Warm Music",
                    &[
                        ("low_freq_hz", 100.0),
                        ("low_gain_db", 2.0),
                        ("mid_freq_hz", 800.0),
                        ("mid_gain_db", -1.0),
                        ("high_freq_hz", 10000.0),
                        ("high_gain_db", 2.0),
                    ],
                ),
            ],
        );
        set_presets(
            &mut effects,
            "compressor",
            vec![
                preset(
                    "Gentle 2:1",
                    &[
                        ("threshold_db", -20.0),
                        ("ratio", 2.0),
                        ("attack_ms", 10.0),
                        ("release_ms", 120.0),
                        ("makeup_gain_db", 2.0),
                    ],
                ),
                preset(
                    "Broadcast 4:1",
                    &[
                        ("threshold_db", -18.0),
                        ("ratio", 4.0),
                        ("attack_ms", 5.0),
                        ("release_ms", 100.0),
                        ("makeup_gain_db", 3.0),
                    ],
                ),
                preset(
                    "Streaming 6:1",
                    &[
                        ("threshold_db", -16.0),
                        ("ratio", 6.0),
                        ("attack_ms", 3.0),
                        ("release_ms", 80.0),
                        ("makeup_gain_db", 4.0),
                    ],
                ),
            ],
        );
        set_presets(
            &mut effects,
            "gate",
            vec![
                preset(
                    "Soft -60 dB",
                    &[
                        ("threshold_db", -60.0),
                        ("range_db", -20.0),
                        ("attack_ms", 5.0),
                        ("hold_ms", 20.0),
                        ("release_ms", 200.0),
                    ],
                ),
                preset(
                    "Room mic -40 dB",
                    &[
                        ("threshold_db", -40.0),
                        ("range_db", -40.0),
                        ("attack_ms", 2.5),
                        ("hold_ms", 10.0),
                        ("release_ms", 120.0),
                    ],
                ),
                preset(
                    "Noisy mic -30 dB",
                    &[
                        ("threshold_db", -30.0),
                        ("range_db", -50.0),
                        ("attack_ms", 1.0),
                        ("hold_ms", 10.0),
                        ("release_ms", 80.0),
                    ],
                ),
            ],
        );
        set_presets(
            &mut effects,
            "limiter",
            vec![
                preset(
                    "Gentle -3 dB",
                    &[("input_gain_db", 0.0), ("ceiling_db", -3.0)],
                ),
                preset(
                    "Broadcast -1 dB",
                    &[("input_gain_db", 0.0), ("ceiling_db", -1.0)],
                ),
                preset(
                    "Loud -0.5 dB",
                    &[("input_gain_db", 3.0), ("ceiling_db", -0.5)],
                ),
            ],
        );

        Self {
            effects,
            preferred_order: vec![
                "deepfilternet".into(),
                "rnnoise".into(),
                "highpass".into(),
                "eq".into(),
                "compressor".into(),
                "gate".into(),
                "limiter".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HardwareProfile {
    pub id: String,
    pub name: String,
    pub revision: u32,
    pub matches: Vec<HardwareProfileMatch>,
    pub capabilities: DeviceCapabilities,
    pub latency_policy: LatencyPolicy,
    pub routing_policy: RoutingPolicy,
    pub bluetooth_mic_policy: BluetoothMicPolicy,
    pub codec_policy: CodecPolicy,
    pub confidence: ProfileConfidence,
    pub quirks: Vec<String>,
    pub source_notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintainer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HardwareProfileSummary {
    pub id: String,
    pub name: String,
    pub source: String,
    pub confidence: ProfileConfidence,
    pub latency_policy: LatencyPolicy,
    pub routing_policy: RoutingPolicy,
    pub bluetooth_mic_policy: BluetoothMicPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HardwareProfileUiState {
    pub profiles: Vec<HardwareProfileSummary>,
    pub assignments: BTreeMap<DeviceId, String>,
    pub fallback_profile: FallbackHardwareProfile,
}

impl Default for HardwareProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            revision: 1,
            matches: Vec::new(),
            capabilities: DeviceCapabilities::default(),
            latency_policy: LatencyPolicy::default(),
            routing_policy: RoutingPolicy::default(),
            bluetooth_mic_policy: BluetoothMicPolicy::NeverIfHfp,
            codec_policy: CodecPolicy::default(),
            confidence: ProfileConfidence::Low,
            quirks: Vec::new(),
            source_notes: Vec::new(),
            maintainer: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct HardwareProfileMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus: Option<DeviceBus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    #[serde(default)]
    pub node_name_contains: Vec<String>,
    #[serde(default)]
    pub description_contains: Vec<String>,
    #[serde(default)]
    pub property_contains: Vec<String>,
    #[serde(default)]
    pub driver_contains: Vec<String>,
    #[serde(default)]
    pub bluetooth_modalias_contains: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DeviceBus {
    Usb,
    Bluetooth,
    Pci,
    Platform,
    Virtual,
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DeviceCapabilities {
    pub input: bool,
    pub output: bool,
    pub duplex: bool,
    pub bluetooth_a2dp: bool,
    pub bluetooth_hfp: bool,
    pub duplex_a2dp: bool,
    pub usb_audio_class: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u8>,
    #[serde(default)]
    pub sample_rates_hz: Vec<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LatencyPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_msec: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low_latency_msec: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluetooth_floor_msec: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RoutingPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_priority: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_priority: Option<u8>,
    pub allow_auto_select_input: bool,
    pub allow_auto_select_output: bool,
    pub prefer_non_bluetooth_input: bool,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            input_priority: None,
            output_priority: None,
            allow_auto_select_input: true,
            allow_auto_select_output: true,
            prefer_non_bluetooth_input: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum BluetoothMicPolicy {
    #[default]
    NeverIfHfp,
    AllowExplicitCallMode,
    AllowDuplexA2dpIfSupported,
    AdvisoryOnly,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CodecPolicy {
    #[serde(default)]
    pub preferred_a2dp_codecs: Vec<String>,
    #[serde(default)]
    pub avoid_codecs: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub latency_floor_msec: BTreeMap<String, u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ldac_quality: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ProfileConfidence {
    #[default]
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceInfo {
    pub id: DeviceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    pub name: String,
    pub description: String,
    #[serde(default = "default_true")]
    pub is_available: bool,
    pub is_default: bool,
    pub is_virtual: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus: Option<DeviceBus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alsa_card: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alsa_device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluetooth_modalias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_codec: Option<String>,
    #[serde(default)]
    pub pipewire_properties: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_profile_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_confidence: Option<ProfileConfidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_latency_policy: Option<LatencyPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_routing_policy: Option<RoutingPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_bluetooth_mic_policy: Option<BluetoothMicPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppStream {
    pub id: AppStreamId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_class: Option<String>,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_channel_id: Option<ChannelId>,
    pub volume: f32,
    pub muted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LevelMeter {
    pub node_id: String,
    pub peak_left: f32,
    pub peak_right: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectAvailability {
    pub effect_id: String,
    pub available: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RuntimeGraph {
    pub inputs: Vec<DeviceInfo>,
    pub outputs: Vec<DeviceInfo>,
    pub app_streams: Vec<AppStream>,
    pub meters: Vec<LevelMeter>,
    pub effect_availability: Vec<EffectAvailability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineStatus {
    pub dry_run: bool,
    pub healthy: bool,
    pub audio_graph_running: bool,
    pub message: String,
    pub last_refresh_unix: i64,
}

impl Default for EngineStatus {
    fn default() -> Self {
        Self {
            dry_run: false,
            healthy: true,
            audio_graph_running: false,
            message: "Ready".into(),
            last_refresh_unix: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppStateSnapshot {
    pub config: MixerConfig,
    pub graph: RuntimeGraph,
    pub diagnostics: Vec<Diagnostic>,
    pub engine: EngineStatus,
    pub catalog: EffectCatalog,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Diagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub action: Option<String>,
}

pub fn clamp_unit(value: f32) -> f32 {
    if !value.is_finite() {
        1.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

pub fn percent_to_unit(percent: f32) -> f32 {
    clamp_unit(percent / 100.0)
}

pub fn unit_to_percent(value: f32) -> u8 {
    (clamp_unit(value) * 100.0).round() as u8
}

pub fn clamp_sync_delay_msec(value: u16) -> u16 {
    value.min(250)
}

pub fn safe_node_id(value: &str) -> String {
    let slug = slugify(value);
    if slug.is_empty() {
        "node".into()
    } else {
        slug
    }
}

fn valid_unit(value: f32) -> Result<f32, ModelError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(ModelError::InvalidVolume(value.to_string()))
    }
}

fn default_unit_volume() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

fn default_fallback_hardware_profile_id() -> String {
    "default.generic-audio".into()
}

fn default_fallback_hardware_profile_name() -> &'static str {
    "Default Generic Audio"
}

fn normalize_latency_policy(policy: &mut LatencyPolicy) {
    policy.stable_msec = policy.stable_msec.map(|value| value.clamp(5, 500));
    policy.low_latency_msec = policy.low_latency_msec.map(|value| value.clamp(5, 500));
    policy.bluetooth_floor_msec = policy
        .bluetooth_floor_msec
        .map(|value| value.clamp(50, 500));
}

fn normalize_routing_policy(policy: &mut RoutingPolicy) {
    policy.input_priority = policy.input_priority.map(|value| value.min(100));
    policy.output_priority = policy.output_priority.map(|value| value.min(100));
}

fn offset_index(index: usize, direction: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let target = index as i64 + i64::from(direction);
    target.clamp(0, len.saturating_sub(1) as i64) as usize
}

fn clean_name(name: impl AsRef<str>) -> Result<String, ModelError> {
    let name = name.as_ref().trim();
    if name.is_empty() {
        return Err(ModelError::InvalidName);
    }
    Ok(name.chars().take(64).collect())
}

fn clean_optional_matcher(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn clean_optional_device_id(value: Option<String>) -> Option<String> {
    clean_optional_matcher(value).filter(|value| value != "@DEFAULT_SOURCE@")
}

fn clean_output_devices(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter_map(|value| clean_optional_matcher(Some(value)))
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn clean_optional_icon(value: Option<String>) -> Option<String> {
    let value = clean_optional_matcher(value)?;
    let value = value
        .chars()
        .take(32)
        .collect::<String>()
        .to_ascii_lowercase();
    value
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        .then_some(value)
}

fn clean_optional_profile_id(value: Option<String>) -> Option<String> {
    clean_optional_matcher(value).map(|value| value.chars().take(128).collect())
}

fn clean_streamer_id(value: String) -> Option<String> {
    clean_optional_matcher(Some(value)).map(|value| value.chars().take(160).collect())
}

fn clean_optional_label(value: Option<String>) -> Option<String> {
    value.and_then(|value| clean_app_display_name(&value))
}

fn stable_stream_display_name(stream: &AppStream) -> Option<String> {
    let display_name = clean_optional_matcher(Some(stream.display_name.clone()))?;
    (!display_name.to_ascii_lowercase().starts_with("stream ")).then_some(display_name)
}

fn clean_app_display_name(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.chars().take(96).collect())
}

fn matcher_display_name(matcher: &AppMatcher) -> String {
    matcher
        .app_id
        .as_deref()
        .or(matcher.process_name.as_deref())
        .or(matcher.binary.as_deref())
        .or(matcher.window_class.as_deref())
        .or(matcher.media_name.as_deref())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "Unknown app".into())
}

fn matcher_should_keep_media_name(matcher: &AppMatcher) -> bool {
    let Some(media_name) = matcher.media_name.as_deref() else {
        return false;
    };
    if media_name.trim().is_empty() || is_generic_media_name(media_name) {
        return false;
    }

    let identity_values = [
        matcher.app_id.as_deref(),
        matcher.binary.as_deref(),
        matcher.process_name.as_deref(),
        matcher.window_class.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.trim().is_empty())
    .map(str::to_ascii_lowercase)
    .collect::<Vec<_>>();

    if identity_values.is_empty() {
        return true;
    }

    identity_values.iter().any(|value| {
        [
            "ferdium", "electron", "chromium", "chrome", "brave", "vivaldi", "webapp", "web-app",
        ]
        .iter()
        .any(|needle| value.contains(needle))
    })
}

fn is_generic_media_name(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    if value == "stream" {
        return true;
    }
    if value
        .strip_prefix("stream ")
        .is_some_and(|suffix| suffix.chars().all(|ch| ch.is_ascii_digit()))
    {
        return true;
    }

    matches!(
        value.as_str(),
        "audio-src" | "audio src" | "audio" | "playback" | "output" | "input"
    )
}

fn app_matcher_key(matcher: &AppMatcher) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        matcher.app_id.as_deref().unwrap_or_default(),
        matcher.binary.as_deref().unwrap_or_default(),
        matcher.process_name.as_deref().unwrap_or_default(),
        matcher.window_class.as_deref().unwrap_or_default(),
        matcher.media_name.as_deref().unwrap_or_default()
    )
}

fn app_matchers_overlap(left: &AppMatcher, right: &AppMatcher) -> bool {
    matcher_matches_matcher(left, right) || matcher_matches_matcher(right, left)
}

fn saved_matchers_conflict(existing: &AppMatcher, incoming: &AppMatcher) -> bool {
    if existing == incoming {
        return true;
    }
    if !app_matchers_overlap(existing, incoming) {
        return false;
    }

    let existing_has_media = existing
        .media_name
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let incoming_has_media = incoming
        .media_name
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if !existing_has_media && !incoming_has_media {
        return true;
    }

    let existing_keeps_media = matcher_should_keep_media_name(existing);
    let incoming_keeps_media = matcher_should_keep_media_name(incoming);
    if !existing_keeps_media && !incoming_keeps_media {
        return true;
    }

    existing_keeps_media
        && incoming_keeps_media
        && existing
            .media_name
            .as_deref()
            .is_some_and(|existing_media| {
                incoming
                    .media_name
                    .as_deref()
                    .is_some_and(|incoming_media| {
                        existing_media.eq_ignore_ascii_case(incoming_media)
                    })
            })
}

fn app_matcher_specificity(matcher: &AppMatcher) -> usize {
    [
        matcher.app_id.as_deref(),
        matcher.binary.as_deref(),
        matcher.process_name.as_deref(),
        matcher.window_class.as_deref(),
        matcher.media_name.as_deref(),
    ]
    .into_iter()
    .filter(|value| value.is_some_and(|value| !value.trim().is_empty()))
    .count()
}

fn matcher_matches_matcher(pattern: &AppMatcher, candidate: &AppMatcher) -> bool {
    let fields = [
        (&pattern.app_id, &candidate.app_id),
        (&pattern.binary, &candidate.binary),
        (&pattern.process_name, &candidate.process_name),
        (&pattern.window_class, &candidate.window_class),
        (&pattern.media_name, &candidate.media_name),
    ];
    let mut has_pattern = false;
    for (pattern, candidate) in fields {
        let Some(pattern) = pattern.as_deref().filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        has_pattern = true;
        let Some(candidate) = candidate.as_deref() else {
            return false;
        };
        if !pattern.eq_ignore_ascii_case(candidate) {
            return false;
        }
    }
    has_pattern
}

fn normalize_effect_chain(
    effects: Vec<EffectInstance>,
    catalog: &EffectCatalog,
    drop_unknown: bool,
) -> Result<Vec<EffectInstance>, ModelError> {
    let mut normalized = Vec::new();
    for mut effect in effects {
        effect.effect_id = effect.effect_id.trim().to_string();
        let definition = match catalog
            .effects
            .iter()
            .find(|definition| definition.id == effect.effect_id)
        {
            Some(definition) => definition,
            None if drop_unknown => continue,
            None => return Err(ModelError::EffectNotFound(effect.effect_id)),
        };

        effect.instance_id = effect.instance_id.trim().to_string();
        if effect.instance_id.is_empty() {
            effect.instance_id = Uuid::new_v4().to_string();
        }
        effect.name = clean_optional_name(effect.name);
        effect.params = normalize_effect_params(effect.params, definition);
        normalized.push(effect);
    }
    Ok(keep_one_single_instance_effect_per_channel(
        normalized, None,
    ))
}

fn keep_one_single_instance_effect_per_channel(
    effects: Vec<EffectInstance>,
    preferred_instance_id: Option<&str>,
) -> Vec<EffectInstance> {
    let mut single_instance_indexes: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, effect) in effects.iter().enumerate() {
        if let Some(group) = single_instance_effect_group(&effect.effect_id) {
            single_instance_indexes
                .entry(group.to_string())
                .or_default()
                .push(index);
        }
    }

    if single_instance_indexes.is_empty() {
        return effects;
    }

    let mut keep_indexes = BTreeSet::new();
    for indexes in single_instance_indexes.values() {
        let preferred = preferred_instance_id.and_then(|instance_id| {
            indexes
                .iter()
                .copied()
                .find(|index| effects[*index].instance_id == instance_id)
        });
        let active = indexes
            .iter()
            .rev()
            .copied()
            .find(|index| !effects[*index].bypassed);
        if let Some(index) = preferred.or(active).or_else(|| indexes.last().copied()) {
            keep_indexes.insert(index);
        }
    }

    effects
        .into_iter()
        .enumerate()
        .filter_map(|(index, effect)| {
            (single_instance_effect_group(&effect.effect_id).is_none()
                || keep_indexes.contains(&index))
            .then_some(effect)
        })
        .collect()
}

fn single_instance_effect_group(effect_id: &str) -> Option<&'static str> {
    match effect_id {
        "deepfilternet" | "rnnoise" => Some("noise_suppression"),
        "highpass" => Some("highpass"),
        "eq" => Some("eq"),
        "compressor" => Some("compressor"),
        "gate" => Some("gate"),
        "limiter" => Some("limiter"),
        _ => None,
    }
}

fn validate_effect_chain(
    effects: &[EffectInstance],
    catalog: &EffectCatalog,
) -> Result<(), ModelError> {
    for effect in effects {
        if effect.instance_id.trim().is_empty() {
            return Err(ModelError::InvalidConfig(
                "effect instance id must not be empty".into(),
            ));
        }
        let definition = effect_definition(catalog, &effect.effect_id)?;
        for key in effect.params.keys() {
            effect_param_definition(definition, key)?;
        }
        for param in &definition.params {
            let value = effect.params.get(&param.id).ok_or_else(|| {
                ModelError::InvalidConfig(format!(
                    "{} effect missing '{}' parameter",
                    definition.name, param.label
                ))
            })?;
            if !value.is_finite() || *value < param.min || *value > param.max {
                return Err(ModelError::InvalidConfig(format!(
                    "{} effect parameter '{}' is out of range",
                    definition.name, param.label
                )));
            }
        }
    }
    Ok(())
}

fn normalize_effect_params(
    mut params: BTreeMap<String, f32>,
    definition: &EffectDefinition,
) -> BTreeMap<String, f32> {
    migrate_deepfilternet_capture_profile(&mut params, definition);
    definition
        .params
        .iter()
        .map(|param| {
            let value = params.get(&param.id).copied().unwrap_or(param.default);
            (param.id.clone(), clamp_param_value(value, param))
        })
        .collect()
}

fn migrate_deepfilternet_capture_profile(
    params: &mut BTreeMap<String, f32>,
    definition: &EffectDefinition,
) {
    if definition.id != "deepfilternet" {
        return;
    }

    let old_conservative_profile = [
        ("input_trim_db", -12.0),
        ("output_makeup_db", 6.0),
        ("attenuation_limit_db", 24.0),
        ("min_processing_threshold_db", -10.0),
        ("max_erb_processing_threshold_db", 30.0),
        ("max_df_processing_threshold_db", 0.0),
        ("min_processing_buffer_frames", 8.0),
        ("post_filter_beta", 0.0),
    ];

    if !old_conservative_profile
        .iter()
        .all(|(id, expected)| param_matches(params, id, *expected))
    {
        return;
    }

    for (id, value) in [
        ("input_trim_db", -6.0),
        ("output_makeup_db", 6.0),
        ("attenuation_limit_db", 18.0),
        ("min_processing_threshold_db", -15.0),
        ("max_erb_processing_threshold_db", 30.0),
        ("max_df_processing_threshold_db", 20.0),
        ("min_processing_buffer_frames", 8.0),
        ("post_filter_beta", 0.0),
    ] {
        params.insert(id.into(), value);
    }
}

fn param_matches(params: &BTreeMap<String, f32>, id: &str, expected: f32) -> bool {
    match params.get(id) {
        Some(value) => (*value - expected).abs() <= 0.001,
        None => false,
    }
}

fn effect_definition<'a>(
    catalog: &'a EffectCatalog,
    effect_id: &str,
) -> Result<&'a EffectDefinition, ModelError> {
    catalog
        .effects
        .iter()
        .find(|definition| definition.id == effect_id)
        .ok_or_else(|| ModelError::EffectNotFound(effect_id.into()))
}

fn effect_param_definition<'a>(
    definition: &'a EffectDefinition,
    param_id: &str,
) -> Result<&'a EffectParamDefinition, ModelError> {
    definition
        .params
        .iter()
        .find(|param| param.id == param_id)
        .ok_or_else(|| {
            ModelError::InvalidConfig(format!(
                "{} effect has no '{}' parameter",
                definition.name, param_id
            ))
        })
}

fn clamp_param_value(value: f32, param: &EffectParamDefinition) -> f32 {
    if value.is_finite() {
        value.clamp(param.min, param.max)
    } else {
        param.default
    }
}

fn clean_optional_name(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().chars().take(64).collect::<String>())
        .filter(|value| !value.is_empty())
}

fn ensure_unique_names<'a>(
    names: impl Iterator<Item = &'a str>,
    kind: &str,
) -> Result<(), ModelError> {
    let mut seen = BTreeSet::new();
    for name in names {
        let key = name.to_lowercase();
        if !seen.insert(key) {
            return Err(ModelError::InvalidConfig(format!(
                "duplicate {kind} name: {name}"
            )));
        }
    }
    Ok(())
}

fn unique_slug<'a>(name: &str, existing: impl Iterator<Item = &'a str>) -> String {
    let base = slugify(name);
    let base = if base.is_empty() { "item".into() } else { base };
    let existing: BTreeSet<_> = existing.map(|value| value.to_string()).collect();
    if !existing.contains(&base) {
        return base;
    }
    for suffix in 2..=999 {
        let candidate = format!("{base}_{suffix}");
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    format!("{base}_{}", Uuid::new_v4().simple())
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_sep = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            slug.push('_');
            last_was_sep = true;
        }
    }
    slug.trim_matches('_').to_string()
}

fn unmute_hardware_monitor_bus(channels: &mut [Channel]) {
    for channel in channels
        .iter_mut()
        .filter(|channel| channel.kind.uses_hardware_slot())
    {
        if let Some(bus) = channel.mix_buses.get_mut("monitor") {
            bus.volume = 1.0;
            bus.muted = false;
        }
    }
}

fn effect(
    id: &str,
    name: &str,
    description: &str,
    plugin_hint: PluginHint,
    params: Vec<EffectParamDefinition>,
) -> EffectDefinition {
    let mut preset_values = BTreeMap::new();
    for param in &params {
        preset_values.insert(param.id.clone(), param.default);
    }
    EffectDefinition {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        plugin_hint,
        params,
        presets: vec![EffectPreset {
            name: "Default".into(),
            values: preset_values,
        }],
    }
}

fn set_presets(effects: &mut [EffectDefinition], effect_id: &str, presets: Vec<EffectPreset>) {
    if let Some(effect) = effects.iter_mut().find(|effect| effect.id == effect_id) {
        effect.presets = presets;
    }
}

fn preset(name: &str, values: &[(&str, f32)]) -> EffectPreset {
    EffectPreset {
        name: name.into(),
        values: values
            .iter()
            .map(|(key, value)| ((*key).into(), *value))
            .collect(),
    }
}

fn param(
    id: &str,
    label: &str,
    min: f32,
    max: f32,
    default: f32,
    unit: &str,
) -> EffectParamDefinition {
    EffectParamDefinition {
        id: id.into(),
        label: label.into(),
        min,
        max,
        default,
        unit: unit.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_wave_link_v1_bounds() {
        let config = MixerConfig::default();
        assert_eq!(config.audio.sample_rate_hz, SAMPLE_RATE_HZ);
        assert!(config.mixes.len() <= MAX_MIXES);
        assert!(config.channels.len() <= MAX_CHANNELS);
        assert!(config.software_channel_count() <= MAX_SOFTWARE_CHANNELS);
        assert!(config.hardware_input_count() <= MAX_HARDWARE_INPUTS);
        assert_eq!(config.channels[0].id, "hardware_in");
        assert_eq!(config.channels[0].kind, ChannelKind::Generic);
        assert!(config
            .channels
            .iter()
            .any(|channel| channel.id == "system" && channel.kind == ChannelKind::System));
        config.validate().unwrap();
    }

    #[test]
    fn legacy_default_fallback_profile_migrates_to_safe_latency() {
        let raw = r#"
        {
          "device_policy": {
            "fallback_hardware_profile": {
              "id": "default.generic-audio",
              "name": "Default Generic Audio",
              "latency_policy": {
                "stable_msec": 35,
                "low_latency_msec": 20,
                "bluetooth_floor_msec": 120
              },
              "routing_policy": {
                "input_priority": 35,
                "output_priority": 30,
                "allow_auto_select_input": true,
                "allow_auto_select_output": true,
                "prefer_non_bluetooth_input": true
              },
              "bluetooth_mic_policy": "never_if_hfp",
              "confidence": "low"
            }
          }
        }
        "#;

        let config: MixerConfig = serde_json::from_str(raw).unwrap();
        let config = config.normalized().unwrap();

        assert_eq!(
            config
                .device_policy
                .fallback_hardware_profile
                .latency_policy,
            FallbackHardwareProfile::default().latency_policy
        );
    }

    #[test]
    fn legacy_partial_config_deserializes_and_repairs_defaults() {
        let raw = r#"
        {
          "version": 1,
          "mixes": [
            {"id": "monitor", "name": "Monitor"}
          ],
          "channels": [
            {
              "id": "hardware_in",
              "name": "Hardware In",
              "kind": "generic",
              "mix_buses": {
                "monitor": {}
              }
            }
          ],
          "app_routes": [],
          "settings": {},
          "audio": {}
        }
        "#;

        let config: MixerConfig = serde_json::from_str(raw).unwrap();
        let config = config.normalized().unwrap();
        assert!(config.settings.auto_check_updates);
        assert!(!config.settings.restore_audio_graph_on_launch);
        assert!(!config.settings.lock_default_output);
        assert_eq!(config.audio.sample_rate_hz, SAMPLE_RATE_HZ);
        assert_eq!(
            config.mixes[0].virtual_source_name,
            "wavelinux_mix_monitor_source"
        );
        assert_eq!(
            config.channels[0].virtual_sink_name,
            "wavelinux_channel_hardware_in"
        );
        assert_eq!(config.channels[0].name, "Input");
        assert_eq!(config.channels[0].mix_buses["monitor"].volume, 1.0);
        assert!(!config.channels[0].mix_buses["monitor"].muted);
    }

    #[test]
    fn version_three_config_disables_default_output_lock() {
        let mut config = MixerConfig {
            version: 3,
            ..MixerConfig::default()
        };
        config.settings.lock_default_output = true;

        let config = config.normalized().unwrap();

        assert_eq!(config.version, CONFIG_VERSION);
        assert!(!config.settings.lock_default_output);
    }

    #[test]
    fn streamer_bindings_migrate_audio_lifecycle_actions_to_noop() {
        let raw = r#"
        {
          "streamer_devices": {
            "profiles": {
              "deck": {
                "device_id": "deck",
                "name": "Stream Deck",
                "bindings": [
                  {
                    "control_id": "hid:button:1",
                    "label": "Start",
                    "control_kind": "button",
                    "action": { "kind": "start_or_repair_audio" }
                  },
                  {
                    "control_id": "hid:button:2",
                    "label": "Legacy cleanup",
                    "control_kind": "button",
                    "action": { "kind": "cleanup_audio_graph" }
                  },
                  {
                    "control_id": "hid:button:3",
                    "label": "Prune",
                    "control_kind": "button",
                    "action": { "kind": "cleanup_stale_audio_graph" }
                  }
                ]
              }
            }
          }
        }
        "#;

        let config: MixerConfig = serde_json::from_str(raw).unwrap();
        let config = config.normalized().unwrap();
        let profile = config.streamer_devices.profiles.get("deck").unwrap();

        assert!(matches!(profile.bindings[0].action, StreamerAction::Noop));
        assert!(matches!(profile.bindings[1].action, StreamerAction::Noop));
        assert!(matches!(
            profile.bindings[2].action,
            StreamerAction::CleanupStaleAudioGraph
        ));
    }

    #[test]
    fn legacy_mic_channel_migrates_to_generic_hardware_input() {
        let mut config = MixerConfig::default();
        let hardware = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        hardware.id = "mic".into();
        hardware.name = "Mic".into();
        hardware.kind = ChannelKind::Microphone;
        hardware.virtual_sink_name = "wavelinux_channel_mic".into();
        config.app_routes.push(AppRoute {
            matcher: AppMatcher::from_app_id("voice-recorder"),
            channel_id: "mic".into(),
        });

        let config = config.normalized().unwrap();
        let hardware = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        assert_eq!(hardware.name, "Input");
        assert_eq!(hardware.kind, ChannelKind::Generic);
        assert_eq!(hardware.virtual_sink_name, "wavelinux_channel_hardware_in");
        assert_eq!(config.app_routes[0].channel_id, "hardware_in");
        assert!(!config.channels.iter().any(|channel| channel.id == "mic"));
    }

    #[test]
    fn mix_limit_is_enforced() {
        let mut config = MixerConfig::default();
        config.create_mix("MicrophoneFX").unwrap();
        config.create_mix("Podcast").unwrap();
        config.create_mix("Discord Mix").unwrap();
        let err = config.create_mix("Sixth").unwrap_err();
        assert_eq!(err, ModelError::MixLimitReached);
    }

    #[test]
    fn new_mix_adds_bus_to_every_channel() {
        let mut config = MixerConfig::default();
        let mix = config.create_mix("MicrophoneFX").unwrap();
        for channel in &config.channels {
            assert!(channel.mix_buses.contains_key(&mix.id));
            assert!(!channel.mix_buses[&mix.id].enabled);
        }
    }

    #[test]
    fn mix_outputs_migrate_legacy_monitor_output() {
        let mut mix = Mix::new_fixed("monitor", "Monitor");
        mix.monitor_output = Some("alsa_output.speakers".into());
        mix.normalize_outputs();

        assert_eq!(mix.outputs(), vec!["alsa_output.speakers"]);
        assert_eq!(mix.output_devices, vec!["alsa_output.speakers"]);
        assert_eq!(mix.monitor_output.as_deref(), Some("alsa_output.speakers"));
    }

    #[test]
    fn mix_icons_are_normalized_and_can_be_cleared() {
        let mut config = MixerConfig::default();

        let mix = config
            .set_mix_icon("monitor", Some("Radio".into()))
            .unwrap();
        assert_eq!(mix.icon.as_deref(), Some("radio"));

        let mix = config
            .set_mix_icon("monitor", Some("../not-a-token".into()))
            .unwrap();
        assert_eq!(mix.icon, None);

        config.mixes[0].icon = Some("Chat".into());
        config.mixes[1].icon = Some("bad/icon".into());
        let config = config.normalized().unwrap();
        assert_eq!(config.mixes[0].icon.as_deref(), Some("chat"));
        assert_eq!(config.mixes[1].icon, None);
    }

    #[test]
    fn channel_icons_are_normalized_and_can_be_cleared() {
        let mut config = MixerConfig::default();

        let channel = config
            .set_channel_icon("music", Some("Music".into()))
            .unwrap();
        assert_eq!(channel.icon.as_deref(), Some("music"));

        let channel = config
            .set_channel_icon("music", Some("../not-a-token".into()))
            .unwrap();
        assert_eq!(channel.icon, None);

        config.channels[0].icon = Some("Mic".into());
        config.channels[1].icon = Some("bad/icon".into());
        let config = config.normalized().unwrap();
        assert_eq!(config.channels[0].icon.as_deref(), Some("mic"));
        assert_eq!(config.channels[1].icon, None);
    }

    #[test]
    fn app_volume_presets_dedupe_and_clamp() {
        let mut config = MixerConfig::default();
        let matcher = AppMatcher::from_app_id("spotify");
        config.set_app_volume_preset(matcher.clone(), 0.45).unwrap();
        config.set_app_volume_preset(matcher.clone(), 1.0).unwrap();

        assert_eq!(config.app_volume_presets.len(), 1);
        assert_eq!(config.app_volume_presets[0].volume, 1.0);
        assert!(config.remove_app_volume_preset(matcher).is_some());
        assert!(config.app_volume_presets.is_empty());
    }

    #[test]
    fn app_history_remembers_forgets_and_restores_streams() {
        let mut config = MixerConfig::default();
        let stream = AppStream {
            id: "42".into(),
            app_id: Some("firefox".into()),
            binary: Some("firefox".into()),
            process_name: Some("firefox".into()),
            window_class: Some("firefox".into()),
            display_name: " Firefox ".into(),
            media_name: Some(" YouTube ".into()),
            routed_channel_id: None,
            volume: 0.75,
            muted: false,
        };

        let remembered = config.remember_app_stream(&stream, 100).unwrap().unwrap();
        assert_eq!(remembered.display_name, "Firefox");
        assert_eq!(config.app_history[0].media_name.as_deref(), Some("YouTube"));
        assert!(config.remember_app_stream(&stream, 120).unwrap().is_none());

        config
            .assign_app_to_channel("browser", remembered.matcher.clone())
            .unwrap();
        config
            .set_app_volume_preset(remembered.matcher.clone(), 0.42)
            .unwrap();
        assert!(config.forget_app(remembered.matcher.clone()).is_some());
        assert!(config.app_history[0].forgotten);
        assert!(config.app_routes.is_empty());
        assert!(config.app_volume_presets.is_empty());
        assert!(config.restore_app(remembered.matcher).is_some());
        assert!(!config.app_history[0].forgotten);
    }

    #[test]
    fn app_history_ignores_generic_numbered_stream_media_names() {
        let mut config = MixerConfig::default();
        let mut stream = AppStream {
            id: "chrome-1".into(),
            app_id: Some("chromium".into()),
            binary: Some("chromium".into()),
            process_name: Some("chromium".into()),
            window_class: Some("chromium".into()),
            display_name: "Chromium".into(),
            media_name: Some("Stream 701".into()),
            routed_channel_id: None,
            volume: 1.0,
            muted: false,
        };

        let remembered = config.remember_app_stream(&stream, 100).unwrap().unwrap();
        assert!(remembered.matcher.media_name.is_none());
        assert!(config.app_history[0].media_name.is_none());

        stream.media_name = Some("Stream 702".into());
        assert!(config.remember_app_stream(&stream, 120).unwrap().is_none());
        assert_eq!(config.app_history.len(), 1);
        assert!(config.app_history[0].media_name.is_none());
    }

    #[test]
    fn normal_app_stream_matchers_drop_media_name_and_dedupe_saved_routes() {
        let mut config = MixerConfig::default();
        let spotify_stream = AppStream {
            id: "spotify-1".into(),
            app_id: Some("spotify".into()),
            binary: Some("spotify".into()),
            process_name: Some("spotify".into()),
            window_class: Some("spotify".into()),
            display_name: "Spotify".into(),
            media_name: Some("audio-src".into()),
            routed_channel_id: None,
            volume: 1.0,
            muted: false,
        };

        let matcher = AppMatcher::from_stream(&spotify_stream).unwrap();
        assert_eq!(matcher.app_id.as_deref(), Some("spotify"));
        assert!(matcher.media_name.is_none());

        config.app_routes.push(AppRoute {
            matcher: AppMatcher {
                app_id: Some("spotify".into()),
                binary: Some("spotify".into()),
                process_name: Some("spotify".into()),
                window_class: Some("spotify".into()),
                media_name: Some("audio-src".into()),
            },
            channel_id: "browser".into(),
        });
        config.assign_app_to_channel("music", matcher).unwrap();

        assert_eq!(config.app_routes.len(), 1);
        assert_eq!(config.app_routes[0].channel_id, "music");
        assert!(config.app_routes[0].matcher.media_name.is_none());
    }

    #[test]
    fn stream_matchers_prefer_stable_display_name_over_generic_web_audio_media() {
        let stream = AppStream {
            id: "spotify-1".into(),
            app_id: None,
            binary: None,
            process_name: None,
            window_class: None,
            display_name: "Spotify".into(),
            media_name: Some("audio-src".into()),
            routed_channel_id: None,
            volume: 1.0,
            muted: false,
        };

        let matcher = AppMatcher::from_stream(&stream).unwrap();

        assert_eq!(matcher.app_id.as_deref(), Some("Spotify"));
        assert_eq!(matcher.process_name.as_deref(), Some("Spotify"));
        assert!(matcher.media_name.is_none());
    }

    #[test]
    fn app_identity_overrides_rewrite_routes_and_raw_removal() {
        let mut config = MixerConfig::default();
        let raw = AppMatcher::from_process_name("Discord");
        let canonical = AppMatcher::from_app_id("com.discordapp.Discord");

        config
            .assign_app_to_channel("chat", raw.clone())
            .expect("route raw app");
        config
            .set_app_volume_preset(raw.clone(), 0.42)
            .expect("preset raw app");
        let pinned = config
            .pin_app_identity(raw.clone(), "Voice Chat")
            .expect("pin identity");
        assert_eq!(pinned.display_name, "Voice Chat");
        assert_eq!(
            config.label_for_matcher(&raw).as_deref(),
            Some("Voice Chat")
        );

        let merged = config
            .merge_app_identity(raw.clone(), canonical.clone())
            .expect("merge identity");
        assert_eq!(merged.matcher, canonical);
        assert_eq!(config.resolve_app_matcher(&raw), canonical);
        assert!(config
            .app_routes
            .iter()
            .any(|route| route.matcher == canonical && route.channel_id == "chat"));
        assert!(config.app_volume_presets.iter().any(
            |preset| preset.matcher == canonical && (preset.volume - 0.42).abs() < f32::EPSILON
        ));

        assert!(config.remove_app_route(raw.clone()).is_some());
        assert!(config.remove_app_volume_preset(raw).is_some());
        assert!(config.app_routes.is_empty());
        assert!(config.app_volume_presets.is_empty());
    }

    #[test]
    fn pinned_stream_labels_persist_for_wrapper_app_media() {
        let mut config = MixerConfig::default();
        let slack_stream = AppStream {
            id: "1".into(),
            app_id: Some("ferdium".into()),
            binary: Some("ferdium".into()),
            process_name: Some("ferdium".into()),
            window_class: Some("Ferdium".into()),
            display_name: "Ferdium".into(),
            media_name: Some("Slack".into()),
            routed_channel_id: None,
            volume: 0.75,
            muted: false,
        };
        let discord_stream = AppStream {
            media_name: Some("Discord".into()),
            ..slack_stream.clone()
        };
        let slack_matcher = AppMatcher::from_stream(&slack_stream).unwrap();
        let discord_matcher = AppMatcher::from_stream(&discord_stream).unwrap();

        config
            .pin_app_identity(AppMatcher::from_app_id("ferdium"), "All Ferdium")
            .unwrap();
        config
            .pin_app_identity(slack_matcher.clone(), "Work Slack")
            .unwrap();
        let remembered = config
            .remember_app_stream(&slack_stream, 100)
            .unwrap()
            .unwrap();
        assert_eq!(remembered.display_name, "Work Slack");
        assert_eq!(
            config.label_for_matcher(&slack_matcher).as_deref(),
            Some("Work Slack")
        );
        assert_eq!(
            config.label_for_matcher(&discord_matcher).as_deref(),
            Some("All Ferdium")
        );

        let remembered = config
            .remember_app_stream(&slack_stream, 200)
            .unwrap()
            .unwrap();
        assert_eq!(remembered.display_name, "Work Slack");
        assert!(config
            .remember_app_stream(&discord_stream, 200)
            .unwrap()
            .is_some());
        assert!(config
            .app_history
            .iter()
            .any(|app| app.display_name == "All Ferdium"
                && app.media_name.as_deref() == Some("Discord")));
    }

    #[test]
    fn device_policy_tracks_selected_hardware_and_monitor_devices() {
        let mut config = MixerConfig::default();

        config
            .set_channel_input("hardware_in", Some("alsa_input.usb_interface".into()))
            .unwrap();
        assert_eq!(
            config.device_policy.preferred_input.as_deref(),
            Some("alsa_input.usb_interface")
        );

        config
            .set_mix_monitor_output("monitor", Some("alsa_output.usb_headphones".into()))
            .unwrap();
        assert_eq!(
            config.device_policy.preferred_output.as_deref(),
            Some("alsa_output.usb_headphones")
        );
    }

    #[test]
    fn mixes_can_be_reordered_without_losing_buses() {
        let mut config = MixerConfig::default();
        let podcast = config.create_mix("Podcast").unwrap();
        let moved = config.move_mix(&podcast.id, -10).unwrap();
        assert_eq!(moved.id, podcast.id);
        assert_eq!(config.mixes[0].id, podcast.id);
        assert!(config
            .channels
            .iter()
            .all(|channel| channel.mix_buses.contains_key(&podcast.id)));

        config.move_mix(&podcast.id, 10).unwrap();
        assert_eq!(config.mixes.last().unwrap().id, podcast.id);
    }

    #[test]
    fn channel_limits_are_enforced_by_wave_link_3_kind() {
        let mut config = MixerConfig::default();

        config
            .create_channel("Podcast", ChannelKind::Application)
            .unwrap();
        config
            .create_channel("Alerts", ChannelKind::Soundboard)
            .unwrap();
        let err = config
            .create_channel("Ninth Software Channel", ChannelKind::Application)
            .unwrap_err();
        assert_eq!(err, ModelError::ChannelLimitReached);

        config
            .create_channel("Mic 2", ChannelKind::Microphone)
            .unwrap();
        config
            .create_channel("Capture Card", ChannelKind::Generic)
            .unwrap();
        config
            .create_channel("Interface", ChannelKind::Microphone)
            .unwrap();
        assert_eq!(config.hardware_input_count(), MAX_HARDWARE_INPUTS);

        let err = config
            .create_channel("Fifth Input", ChannelKind::Microphone)
            .unwrap_err();
        assert_eq!(err, ModelError::ChannelLimitReached);
        assert_eq!(config.channels.len(), MAX_CHANNELS);
    }

    #[test]
    fn channel_volume_rejects_out_of_range_values() {
        let mut config = MixerConfig::default();
        let err = config
            .set_channel_volume("hardware_in", "monitor", 1.2)
            .unwrap_err();
        assert!(matches!(err, ModelError::InvalidVolume(_)));
    }

    #[test]
    fn linked_channel_volume_updates_every_bus() {
        let mut config = MixerConfig::default();
        let custom = config.create_mix("Discord Mix").unwrap();
        config.set_channel_linked("hardware_in", true).unwrap();
        config
            .set_channel_volume("hardware_in", custom.id.clone(), 0.42)
            .unwrap();
        let hardware_in = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        assert!(hardware_in
            .mix_buses
            .values()
            .all(|bus| (bus.volume - 0.42).abs() < f32::EPSILON));
    }

    #[test]
    fn channel_input_can_be_assigned_and_cleared() {
        let mut config = MixerConfig::default();
        config
            .set_channel_input("hardware_in", Some("alsa_input.usb_interface".into()))
            .unwrap();
        let hardware_in = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        assert_eq!(
            hardware_in.source_device.as_deref(),
            Some("alsa_input.usb_interface")
        );

        config
            .set_channel_input("hardware_in", Some("".into()))
            .unwrap();
        let hardware_in = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        assert!(hardware_in.source_device.is_none());
    }

    #[test]
    fn legacy_default_source_sentinel_is_normalized_to_auto_input() {
        let mut config = MixerConfig::default();
        config
            .set_channel_input("hardware_in", Some("@DEFAULT_SOURCE@".into()))
            .unwrap();
        let hardware_in = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        assert!(hardware_in.source_device.is_none());

        let mut config = MixerConfig::default();
        config.channels[0].source_device = Some("@DEFAULT_SOURCE@".into());
        let config = config.normalized().unwrap();
        assert!(config.channels[0].source_device.is_none());
    }

    #[test]
    fn channel_input_mode_is_hardware_only() {
        let mut config = MixerConfig::default();
        let hardware_in = config
            .set_channel_input_mode("hardware_in", ChannelInputMode::MonoLeft)
            .unwrap();
        assert_eq!(hardware_in.input_mode, ChannelInputMode::SumMono);
        assert_eq!(ChannelInputMode::SumMono.channels(), 1);
        assert_eq!(ChannelInputMode::SumMono.channel_map(), "mono");

        let err = config
            .set_channel_input_mode("music", ChannelInputMode::MonoRight)
            .unwrap_err();
        assert!(matches!(err, ModelError::InvalidConfig(_)));
    }

    #[test]
    fn hardware_input_modes_normalize_to_mono() {
        let mut config = MixerConfig::default();
        config.channels[0].input_mode = ChannelInputMode::Stereo;
        let config = config.normalized().unwrap();
        assert_eq!(config.channels[0].input_mode, ChannelInputMode::SumMono);
    }

    #[test]
    fn channels_can_be_reordered_with_routes_intact() {
        let mut config = MixerConfig::default();
        config
            .assign_app_to_channel("music", AppMatcher::from_app_id("spotify"))
            .unwrap();

        let moved = config.move_channel("music", -10).unwrap();
        assert_eq!(moved.id, "music");
        assert_eq!(config.channels[0].id, "music");
        assert_eq!(config.app_routes[0].channel_id, "music");

        config.move_channel("music", 10).unwrap();
        assert_eq!(config.channels.last().unwrap().id, "music");
    }

    #[test]
    fn effect_catalog_contains_open_replacements() {
        let catalog = EffectCatalog::default();
        let ids: BTreeSet<_> = catalog
            .effects
            .iter()
            .map(|effect| effect.id.as_str())
            .collect();
        assert!(ids.contains("rnnoise"));
        assert!(ids.contains("deepfilternet"));
        assert!(ids.contains("limiter"));
    }

    #[test]
    fn effect_catalog_matches_known_plugin_control_ranges() {
        let catalog = EffectCatalog::default();
        let deepfilter = catalog
            .effects
            .iter()
            .find(|effect| effect.id == "deepfilternet")
            .unwrap();
        assert!(
            deepfilter
                .params
                .iter()
                .any(|param| param.id == "post_filter_beta"
                    && (param.max - 0.05).abs() < f32::EPSILON)
        );
        assert!(deepfilter
            .params
            .iter()
            .any(|param| param.id == "input_trim_db"
                && (param.max - 0.0).abs() < f32::EPSILON
                && (param.default + 6.0).abs() < f32::EPSILON));
        assert!(deepfilter
            .params
            .iter()
            .any(|param| param.id == "attenuation_limit_db"
                && (param.max - 100.0).abs() < f32::EPSILON
                && (param.default - 18.0).abs() < f32::EPSILON));
        assert!(deepfilter
            .params
            .iter()
            .any(|param| param.id == "min_processing_threshold_db"
                && (param.min + 15.0).abs() < f32::EPSILON
                && (param.default + 15.0).abs() < f32::EPSILON));
        assert!(deepfilter
            .params
            .iter()
            .any(|param| param.id == "max_df_processing_threshold_db"
                && (param.max - 35.0).abs() < f32::EPSILON
                && (param.default - 20.0).abs() < f32::EPSILON));
        assert!(deepfilter
            .params
            .iter()
            .any(|param| param.id == "min_processing_buffer_frames"
                && (param.min - 0.0).abs() < f32::EPSILON
                && (param.default - 8.0).abs() < f32::EPSILON));

        let compressor = catalog
            .effects
            .iter()
            .find(|effect| effect.id == "compressor")
            .unwrap();
        let threshold = compressor
            .params
            .iter()
            .find(|param| param.id == "threshold_db")
            .unwrap();
        assert_eq!(threshold.min, -30.0);
    }

    #[test]
    fn effect_catalog_contains_wavelinux_31_presets() {
        let catalog = EffectCatalog::default();
        let rnnoise = catalog
            .effects
            .iter()
            .find(|effect| effect.id == "rnnoise")
            .unwrap();
        assert!(rnnoise
            .presets
            .iter()
            .any(|preset| preset.name == "Broadcast"));
        let eq = catalog
            .effects
            .iter()
            .find(|effect| effect.id == "eq")
            .unwrap();
        assert!(eq
            .presets
            .iter()
            .any(|preset| preset.name == "Broadcast Voice"));
    }

    #[test]
    fn effect_chains_are_normalized_to_catalog_ranges() {
        let mut config = MixerConfig::default();
        let mut limiter = EffectInstance::new(" limiter ");
        limiter.instance_id = " limiter-1 ".into();
        limiter.name = Some("  Broadcast limiter  ".into());
        limiter.params.insert("input_gain_db".into(), 999.0);
        limiter.params.insert("stale_param".into(), 12.0);

        let channel = config
            .set_effect_chain("hardware_in", vec![limiter])
            .unwrap();
        let effect = &channel.effects[0];
        assert_eq!(effect.instance_id, "limiter-1");
        assert_eq!(effect.effect_id, "limiter");
        assert_eq!(effect.name.as_deref(), Some("Broadcast limiter"));
        assert_eq!(effect.params.get("input_gain_db"), Some(&20.0));
        assert_eq!(effect.params.get("ceiling_db"), Some(&-1.0));
        assert!(!effect.params.contains_key("stale_param"));
    }

    #[test]
    fn deepfilternet_conservative_defaults_migrate_to_balanced_voice() {
        let mut config = MixerConfig::default();
        let mut deepfilter = EffectInstance::new("deepfilternet");
        for (id, value) in [
            ("input_trim_db", -12.0),
            ("output_makeup_db", 6.0),
            ("attenuation_limit_db", 24.0),
            ("min_processing_threshold_db", -10.0),
            ("max_erb_processing_threshold_db", 30.0),
            ("max_df_processing_threshold_db", 0.0),
            ("min_processing_buffer_frames", 8.0),
            ("post_filter_beta", 0.0),
        ] {
            deepfilter.params.insert(id.into(), value);
        }

        let channel = config
            .set_effect_chain("hardware_in", vec![deepfilter])
            .unwrap();
        let params = &channel.effects[0].params;

        assert_eq!(params.get("input_trim_db"), Some(&-6.0));
        assert_eq!(params.get("output_makeup_db"), Some(&6.0));
        assert_eq!(params.get("attenuation_limit_db"), Some(&18.0));
        assert_eq!(params.get("min_processing_threshold_db"), Some(&-15.0));
        assert_eq!(params.get("max_erb_processing_threshold_db"), Some(&30.0));
        assert_eq!(params.get("max_df_processing_threshold_db"), Some(&20.0));
        assert_eq!(params.get("min_processing_buffer_frames"), Some(&8.0));
    }

    #[test]
    fn deepfilternet_custom_settings_are_not_migrated() {
        let mut config = MixerConfig::default();
        let mut deepfilter = EffectInstance::new("deepfilternet");
        for (id, value) in [
            ("input_trim_db", -9.0),
            ("output_makeup_db", 6.0),
            ("attenuation_limit_db", 24.0),
            ("min_processing_threshold_db", -10.0),
            ("max_erb_processing_threshold_db", 30.0),
            ("max_df_processing_threshold_db", 0.0),
            ("min_processing_buffer_frames", 8.0),
            ("post_filter_beta", 0.0),
        ] {
            deepfilter.params.insert(id.into(), value);
        }

        let channel = config
            .set_effect_chain("hardware_in", vec![deepfilter])
            .unwrap();
        let params = &channel.effects[0].params;

        assert_eq!(params.get("input_trim_db"), Some(&-9.0));
        assert_eq!(params.get("attenuation_limit_db"), Some(&24.0));
        assert_eq!(params.get("max_df_processing_threshold_db"), Some(&0.0));
    }

    #[test]
    fn unknown_effects_are_rejected_or_dropped_during_repair() {
        let mut config = MixerConfig::default();
        let err = config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("missing")])
            .unwrap_err();
        assert_eq!(err, ModelError::EffectNotFound("missing".into()));

        config.channels[0]
            .effects
            .push(EffectInstance::new("missing"));
        let mut gate = EffectInstance::new("gate");
        gate.params.insert("threshold_db".into(), -999.0);
        gate.params.insert("release_ms".into(), f32::INFINITY);
        config.channels[0].effects.push(gate);

        let config = config.normalized().unwrap();
        let effects = &config.channels[0].effects;
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].effect_id, "gate");
        assert_eq!(effects[0].params.get("threshold_db"), Some(&-80.0));
        assert_eq!(effects[0].params.get("release_ms"), Some(&200.0));
    }

    #[test]
    fn duplicate_realtime_noise_suppressors_keep_last_active_instance() {
        let mut config = MixerConfig::default();
        let mut first = EffectInstance::new("deepfilternet");
        first.instance_id = "deepfilter-default".into();
        first.params.insert("attenuation_limit_db".into(), 100.0);
        let mut second = EffectInstance::new("deepfilternet");
        second.instance_id = "deepfilter-natural".into();
        second.params.insert("attenuation_limit_db".into(), 12.0);

        let channel = config
            .set_effect_chain("hardware_in", vec![first, second])
            .unwrap();
        let effects = &channel.effects;

        assert_eq!(effects.len(), 1);
        assert!(!effects[0].bypassed);
        assert_eq!(effects[0].instance_id, "deepfilter-natural");
        assert_eq!(effects[0].params.get("attenuation_limit_db"), Some(&12.0));
    }

    #[test]
    fn mixed_realtime_noise_suppressors_keep_last_active_instance() {
        let mut config = MixerConfig::default();
        let mut deepfilter = EffectInstance::new("deepfilternet");
        deepfilter.instance_id = "deepfilter".into();
        let mut rnnoise = EffectInstance::new("rnnoise");
        rnnoise.instance_id = "rnnoise".into();

        let channel = config
            .set_effect_chain("hardware_in", vec![deepfilter, rnnoise])
            .unwrap();
        let effects = &channel.effects;

        assert_eq!(effects.len(), 1);
        assert!(!effects[0].bypassed);
        assert_eq!(effects[0].effect_id, "rnnoise");
        assert_eq!(effects[0].instance_id, "rnnoise");
    }

    #[test]
    fn mixed_realtime_noise_suppressor_enable_removes_other_suppressors() {
        let mut config = MixerConfig::default();
        let mut deepfilter = EffectInstance::new("deepfilternet");
        deepfilter.instance_id = "deepfilter".into();
        deepfilter.bypassed = true;
        let mut rnnoise = EffectInstance::new("rnnoise");
        rnnoise.instance_id = "rnnoise".into();
        config.channels[0].effects = vec![deepfilter, rnnoise];

        let channel = config
            .bypass_effect("hardware_in", "deepfilter", false)
            .unwrap();
        let effects = &channel.effects;

        assert_eq!(effects.len(), 1);
        assert!(!effects[0].bypassed);
        assert_eq!(effects[0].effect_id, "deepfilternet");
        assert_eq!(effects[0].instance_id, "deepfilter");
    }

    #[test]
    fn duplicate_standard_effects_keep_last_active_instance() {
        let mut config = MixerConfig::default();
        let mut first = EffectInstance::new("eq");
        first.instance_id = "eq-old".into();
        first.params.insert("mid_gain_db".into(), -3.0);
        let mut second = EffectInstance::new("eq");
        second.instance_id = "eq-new".into();
        second.params.insert("mid_gain_db".into(), 2.0);

        let channel = config
            .set_effect_chain("hardware_in", vec![first, second])
            .unwrap();
        let effects = &channel.effects;

        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].instance_id, "eq-new");
        assert_eq!(effects[0].params.get("mid_gain_db"), Some(&2.0));
    }

    #[test]
    fn enabling_duplicate_realtime_noise_suppressor_removes_other_instances() {
        let mut config = MixerConfig::default();
        let mut first = EffectInstance::new("deepfilternet");
        first.instance_id = "deepfilter-default".into();
        first.bypassed = true;
        let mut second = EffectInstance::new("deepfilternet");
        second.instance_id = "deepfilter-natural".into();
        config.channels[0].effects = vec![first, second];

        let channel = config
            .bypass_effect("hardware_in", "deepfilter-default", false)
            .unwrap();
        let effects = &channel.effects;

        assert_eq!(effects.len(), 1);
        assert!(!effects[0].bypassed);
        assert_eq!(effects[0].instance_id, "deepfilter-default");
    }

    #[test]
    fn settings_can_be_replaced() {
        let mut config = MixerConfig::default();
        let mut settings = config.settings.clone();
        settings.start_at_login = true;
        settings.keep_running_in_tray = true;
        settings.restore_audio_graph_on_launch = true;
        settings.theme = ThemeMode::Dark;
        let updated = config.set_settings(settings);
        assert!(updated.start_at_login);
        assert!(updated.keep_running_in_tray);
        assert!(updated.restore_audio_graph_on_launch);
        assert_eq!(config.settings.theme, ThemeMode::Dark);
    }

    #[test]
    fn delete_channel_removes_routes() {
        let mut config = MixerConfig::default();
        config
            .assign_app_to_channel("chat", AppMatcher::from_app_id("discord"))
            .unwrap();
        let removed = config.delete_channel("chat").unwrap();
        assert_eq!(removed.id, "chat");
        assert!(config.app_routes.is_empty());
    }

    #[test]
    fn app_routes_can_be_removed_by_matcher() {
        let mut config = MixerConfig::default();
        let matcher = AppMatcher::from_app_id("spotify");
        config
            .assign_app_to_channel("music", matcher.clone())
            .unwrap();

        let removed = config.remove_app_route(matcher.clone()).unwrap();
        assert_eq!(removed.channel_id, "music");
        assert!(config.app_routes.is_empty());
        assert_eq!(config.remove_app_route(matcher), None);
    }

    #[test]
    fn app_matchers_are_trimmed_and_must_not_be_empty() {
        let mut config = MixerConfig::default();
        let route = config
            .assign_app_to_channel("chat", AppMatcher::from_process_name(" Discord "))
            .unwrap();
        assert_eq!(route.matcher.process_name.as_deref(), Some("Discord"));

        let err = config
            .assign_app_to_channel("chat", AppMatcher::from_app_id("   "))
            .unwrap_err();
        assert_eq!(err, ModelError::InvalidMatcher);
    }

    #[test]
    fn normalized_config_drops_invalid_app_routes() {
        let mut config = MixerConfig::default();
        config.app_routes.push(AppRoute {
            matcher: AppMatcher::from_app_id(""),
            channel_id: "music".into(),
        });
        config.app_routes.push(AppRoute {
            matcher: AppMatcher::from_binary("spotify"),
            channel_id: "missing".into(),
        });
        config.app_routes.push(AppRoute {
            matcher: AppMatcher::from_window_class("Spotify"),
            channel_id: "music".into(),
        });

        let config = config.normalized().unwrap();
        assert_eq!(config.app_routes.len(), 1);
        assert_eq!(
            config.app_routes[0].matcher.window_class.as_deref(),
            Some("Spotify")
        );
    }
}
