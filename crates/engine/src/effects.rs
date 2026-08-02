use super::*;

impl WaveLinuxEngine {
    pub fn set_effect_chain(
        self: &Arc<Self>,
        channel_id: String,
        effects: Vec<EffectInstance>,
    ) -> Result<Channel, EngineError> {
        let channel =
            self.update_config(|config| config.set_effect_chain(channel_id, effects))??;
        self.schedule_effect_graph_sync(channel.id.clone());
        Ok(channel)
    }

    pub fn set_effect_param(
        self: &Arc<Self>,
        channel_id: String,
        instance_id: String,
        param_id: String,
        value: f32,
    ) -> Result<Channel, EngineError> {
        let channel = self.update_config(|config| {
            config.set_effect_param(channel_id, instance_id, param_id, value)
        })??;
        self.schedule_effect_graph_sync(channel.id.clone());
        Ok(channel)
    }

    pub fn bypass_effect(
        self: &Arc<Self>,
        channel_id: String,
        instance_id: String,
        bypassed: bool,
    ) -> Result<Channel, EngineError> {
        let channel =
            self.update_config(|config| config.bypass_effect(channel_id, instance_id, bypassed))??;
        self.schedule_effect_graph_sync(channel.id.clone());
        Ok(channel)
    }

    pub fn set_channel_effects_enabled(
        self: &Arc<Self>,
        channel_id: String,
        enabled: bool,
    ) -> Result<Channel, EngineError> {
        let channel =
            self.update_config(|config| config.set_channel_effects_enabled(channel_id, enabled))??;
        self.schedule_effect_graph_sync(channel.id.clone());
        Ok(channel)
    }
}
