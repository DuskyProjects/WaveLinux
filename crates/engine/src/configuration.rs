use super::*;

impl WaveLinuxEngine {
    pub fn create_mix(self: &Arc<Self>, name: String) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.create_mix(name))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(mix)
    }

    pub fn rename_mix(self: &Arc<Self>, mix_id: String, name: String) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.rename_mix(mix_id, name))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(mix)
    }

    pub fn move_mix(&self, mix_id: String, direction: i32) -> Result<Mix, EngineError> {
        self.update_config(|config| config.move_mix(mix_id, direction))?
    }

    pub fn delete_mix(self: &Arc<Self>, mix_id: String) -> Result<Mix, EngineError> {
        let removed = self.update_config(|config| config.delete_mix(mix_id))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(removed)
    }

    pub fn set_mix_icon(&self, mix_id: String, icon: Option<String>) -> Result<Mix, EngineError> {
        self.update_config(|config| config.set_mix_icon(mix_id, icon))?
    }

    pub fn set_channel_icon(
        &self,
        channel_id: String,
        icon: Option<String>,
    ) -> Result<Channel, EngineError> {
        self.update_config(|config| config.set_channel_icon(channel_id, icon))?
    }

    pub fn set_mix_monitor_output(
        self: &Arc<Self>,
        mix_id: String,
        output: Option<String>,
    ) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| {
            let mix = config.set_mix_monitor_output(mix_id, output)?;
            if mix.id == "monitor" {
                config.settings.monitor_follows_default_output = false;
            }
            Ok(mix)
        })??;
        let _ = self.repair_audio_graph_if_running();
        Ok(mix)
    }

    pub fn set_mix_outputs(
        self: &Arc<Self>,
        mix_id: String,
        outputs: Vec<String>,
    ) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.set_mix_outputs(mix_id, outputs))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(mix)
    }

    pub fn create_channel(
        self: &Arc<Self>,
        name: String,
        kind: ChannelKind,
    ) -> Result<Channel, EngineError> {
        let channel = self.update_config(|config| config.create_channel(name, kind))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn rename_channel(
        self: &Arc<Self>,
        channel_id: String,
        name: String,
    ) -> Result<Channel, EngineError> {
        let channel = self.update_config(|config| config.rename_channel(channel_id, name))??;
        let _ = self.rebuild_effect_chain_configs();
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn move_channel(&self, channel_id: String, direction: i32) -> Result<Channel, EngineError> {
        self.update_config(|config| config.move_channel(channel_id, direction))?
    }

    pub fn delete_channel(self: &Arc<Self>, channel_id: String) -> Result<Channel, EngineError> {
        let removed = self.update_config(|config| config.delete_channel(channel_id))??;
        let _ = self.rebuild_effect_chain_configs();
        let _ = self.repair_audio_graph_if_running();
        Ok(removed)
    }

    pub fn set_channel_linked(
        &self,
        channel_id: String,
        linked: bool,
    ) -> Result<Channel, EngineError> {
        self.update_config(|config| config.set_channel_linked(channel_id, linked))?
    }

    pub fn set_settings(
        self: &Arc<Self>,
        settings: MixerSettings,
    ) -> Result<MixerSettings, EngineError> {
        self.apply_start_at_login(settings.start_at_login)?;
        let (settings, audio_graph_needs_repair) = self.update_config(|config| {
            let previous = config.settings.clone();
            let settings = config.set_settings(settings);
            Ok((
                settings.clone(),
                settings_affect_audio_graph(&previous, &settings),
            ))
        })??;
        if audio_graph_needs_repair {
            let _ = self.repair_audio_graph_if_running();
        }
        Ok(settings)
    }
}
