use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

pub const CONFIG_VERSION: u32 = 1;
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
    #[error("cannot delete the last mix")]
    CannotDeleteLastMix,
    #[error("cannot delete the last channel")]
    CannotDeleteLastChannel,
    #[error("invalid volume: {0}")]
    InvalidVolume(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    System,
    Dark,
    Light,
}

impl Default for ThemeMode {
    fn default() -> Self {
        Self::System
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MixerSettings {
    pub theme: ThemeMode,
    pub start_at_login: bool,
    pub lock_default_input: bool,
    pub lock_default_output: bool,
    pub auto_check_updates: bool,
    pub auto_install_updates: bool,
    pub release_channel: ReleaseChannel,
}

impl Default for MixerSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            start_at_login: false,
            lock_default_input: false,
            lock_default_output: false,
            auto_check_updates: true,
            auto_install_updates: false,
            release_channel: ReleaseChannel::Stable,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChannel {
    Stable,
    Beta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MixerConfig {
    pub version: u32,
    pub mixes: Vec<Mix>,
    pub channels: Vec<Channel>,
    pub app_routes: Vec<AppRoute>,
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
            Channel::new_fixed("mic", "Mic", ChannelKind::Microphone),
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
        Ok(())
    }

    pub fn normalized(mut self) -> Result<Self, ModelError> {
        self.version = CONFIG_VERSION;
        for mix in &mut self.mixes {
            mix.volume = clamp_unit(mix.volume);
        }
        for channel in &mut self.channels {
            channel.ensure_mix_buses(&self.mixes);
            for bus in channel.mix_buses.values_mut() {
                bus.volume = clamp_unit(bus.volume);
            }
        }
        self.validate()?;
        Ok(self)
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
            channel.mix_buses.insert(mix.id.clone(), MixBus::default());
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
        self.settings.clone()
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

    pub fn set_mix_monitor_output(
        &mut self,
        mix_id: impl AsRef<str>,
        output: Option<DeviceId>,
    ) -> Result<Mix, ModelError> {
        let mix = self.mix_mut(mix_id.as_ref())?;
        mix.monitor_output = output.filter(|value| !value.trim().is_empty());
        Ok(mix.clone())
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
        let channel = self.channel_mut(channel_id.as_ref())?;
        channel.source_device = source_device.filter(|value| !value.trim().is_empty());
        Ok(channel.clone())
    }

    pub fn assign_app_to_channel(
        &mut self,
        channel_id: impl AsRef<str>,
        matcher: AppMatcher,
    ) -> Result<AppRoute, ModelError> {
        let channel_id = channel_id.as_ref();
        self.ensure_channel_exists(channel_id)?;
        let route = AppRoute {
            matcher,
            channel_id: channel_id.into(),
        };
        self.app_routes
            .retain(|existing| existing.matcher != route.matcher);
        self.app_routes.push(route.clone());
        Ok(route)
    }

    pub fn set_effect_chain(
        &mut self,
        channel_id: impl AsRef<str>,
        effects: Vec<EffectInstance>,
    ) -> Result<Channel, ModelError> {
        let channel = self.channel_mut(channel_id.as_ref())?;
        channel.effects = effects;
        Ok(channel.clone())
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
        effect.params.insert(param_id.into(), value);
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
    pub virtual_sink_name: String,
    pub virtual_source_name: String,
    pub monitor_output: Option<DeviceId>,
    pub volume: f32,
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
            volume: 1.0,
            muted: false,
        }
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
    pub virtual_sink_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_device: Option<DeviceId>,
    pub linked: bool,
    pub mix_buses: BTreeMap<MixId, MixBus>,
    pub app_matchers: Vec<AppMatcher>,
    pub effects: Vec<EffectInstance>,
}

impl Channel {
    pub fn new_fixed(id: &str, name: &str, kind: ChannelKind) -> Self {
        let safe = safe_node_id(id);
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            virtual_sink_name: format!("wavelinux_channel_{safe}"),
            source_device: None,
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
            self.mix_buses
                .entry(mix.id.clone())
                .or_insert_with(MixBus::default);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MixBus {
    pub volume: f32,
    pub muted: bool,
}

impl Default for MixBus {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppMatcher {
    pub app_id: Option<String>,
    pub binary: Option<String>,
    pub process_name: Option<String>,
    pub window_class: Option<String>,
}

impl AppMatcher {
    pub fn from_app_id(app_id: impl Into<String>) -> Self {
        Self {
            app_id: Some(app_id.into()),
            binary: None,
            process_name: None,
            window_class: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppRoute {
    pub matcher: AppMatcher,
    pub channel_id: ChannelId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectInstance {
    pub instance_id: EffectInstanceId,
    pub effect_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub bypassed: bool,
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
                "DeepFilterNet",
                "Neural noise suppression",
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
                vec![param(
                    "attenuation_limit_db",
                    "Reduction Limit",
                    0.0,
                    100.0,
                    100.0,
                    " dB",
                )],
            ),
            effect(
                "rnnoise",
                "Noise Suppression",
                "RNNoise speech noise suppression",
                PluginHint::Ladspa {
                    library_names: vec!["librnnoise_ladspa.so".into(), "rnnoise_ladspa.so".into()],
                },
                vec![
                    param("vad_threshold", "VAD Threshold", 0.0, 100.0, 50.0, "%"),
                    param("hold_ms", "Hold Open", 0.0, 2000.0, 200.0, " ms"),
                    param("lead_in_ms", "Lead-In", 0.0, 500.0, 0.0, " ms"),
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
                    param("threshold_db", "Threshold", -60.0, 0.0, -20.0, " dB"),
                    param("ratio", "Ratio", 1.0, 20.0, 4.0, ":1"),
                    param("attack_ms", "Attack", 0.1, 200.0, 5.0, " ms"),
                    param("release_ms", "Release", 5.0, 1000.0, 100.0, " ms"),
                    param("makeup_gain_db", "Makeup", 0.0, 24.0, 0.0, " dB"),
                ],
            ),
            effect(
                "gate",
                "Noise Gate",
                "Attenuate quiet room tone",
                PluginHint::Ladspa {
                    library_names: vec!["gate.so".into(), "noise_gate.so".into()],
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
                PluginHint::PipeWireBuiltin,
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
                preset("Natural 12 dB", &[("attenuation_limit_db", 12.0)]),
                preset("Medium 24 dB", &[("attenuation_limit_db", 24.0)]),
                preset("Full 100 dB", &[("attenuation_limit_db", 100.0)]),
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
pub struct DeviceInfo {
    pub id: DeviceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    pub name: String,
    pub description: String,
    pub is_default: bool,
    pub is_virtual: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppStream {
    pub id: AppStreamId,
    pub app_id: Option<String>,
    pub process_name: Option<String>,
    pub display_name: String,
    pub media_name: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeGraph {
    pub inputs: Vec<DeviceInfo>,
    pub outputs: Vec<DeviceInfo>,
    pub app_streams: Vec<AppStream>,
    pub meters: Vec<LevelMeter>,
    pub effect_availability: Vec<EffectAvailability>,
}

impl Default for RuntimeGraph {
    fn default() -> Self {
        Self {
            inputs: Vec::new(),
            outputs: Vec::new(),
            app_streams: Vec::new(),
            meters: Vec::new(),
            effect_availability: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineStatus {
    pub dry_run: bool,
    pub healthy: bool,
    pub message: String,
    pub last_refresh_unix: i64,
}

impl Default for EngineStatus {
    fn default() -> Self {
        Self {
            dry_run: false,
            healthy: true,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Scene {
    pub id: String,
    pub name: String,
    pub created_unix: i64,
    pub config: MixerConfig,
}

impl Scene {
    pub fn new(name: impl AsRef<str>, config: MixerConfig) -> Result<Self, ModelError> {
        let name = clean_name(name)?;
        Ok(Self {
            id: unique_slug(&name, std::iter::empty::<&str>()),
            name,
            created_unix: OffsetDateTime::now_utc().unix_timestamp(),
            config,
        })
    }
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

fn clean_name(name: impl AsRef<str>) -> Result<String, ModelError> {
    let name = name.as_ref().trim();
    if name.is_empty() {
        return Err(ModelError::InvalidName);
    }
    Ok(name.chars().take(64).collect())
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
        config.validate().unwrap();
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
        }
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
        config
            .create_channel("System", ChannelKind::System)
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
            .set_channel_volume("mic", "monitor", 1.2)
            .unwrap_err();
        assert!(matches!(err, ModelError::InvalidVolume(_)));
    }

    #[test]
    fn linked_channel_volume_updates_every_bus() {
        let mut config = MixerConfig::default();
        let custom = config.create_mix("Discord Mix").unwrap();
        config.set_channel_linked("mic", true).unwrap();
        config
            .set_channel_volume("mic", custom.id.clone(), 0.42)
            .unwrap();
        let mic = config
            .channels
            .iter()
            .find(|channel| channel.id == "mic")
            .unwrap();
        assert!(mic
            .mix_buses
            .values()
            .all(|bus| (bus.volume - 0.42).abs() < f32::EPSILON));
    }

    #[test]
    fn channel_input_can_be_assigned_and_cleared() {
        let mut config = MixerConfig::default();
        config
            .set_channel_input("mic", Some("alsa_input.usb_mic".into()))
            .unwrap();
        let mic = config
            .channels
            .iter()
            .find(|channel| channel.id == "mic")
            .unwrap();
        assert_eq!(mic.source_device.as_deref(), Some("alsa_input.usb_mic"));

        config.set_channel_input("mic", Some("".into())).unwrap();
        let mic = config
            .channels
            .iter()
            .find(|channel| channel.id == "mic")
            .unwrap();
        assert!(mic.source_device.is_none());
    }

    #[test]
    fn scene_captures_config() {
        let mut config = MixerConfig::default();
        config.create_mix("MicrophoneFX").unwrap();
        let scene = Scene::new("Streaming", config.clone()).unwrap();
        assert_eq!(scene.name, "Streaming");
        assert_eq!(scene.config.mixes.len(), config.mixes.len());
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
    fn settings_can_be_replaced() {
        let mut config = MixerConfig::default();
        let mut settings = config.settings.clone();
        settings.start_at_login = true;
        settings.theme = ThemeMode::Dark;
        let updated = config.set_settings(settings);
        assert!(updated.start_at_login);
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
}
