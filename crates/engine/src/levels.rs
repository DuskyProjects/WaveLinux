use super::*;

impl WaveLinuxEngine {
    pub fn set_mix_volume(&self, mix_id: String, volume: f32) -> Result<Mix, EngineError> {
        let graph_running = self.audio_graph_running_cached();
        let audio_commands = if graph_running {
            Some(self.lock_audio_commands()?)
        } else {
            None
        };
        let mix = self.update_config(|config| config.set_mix_volume(mix_id, volume))??;
        self.log_engine_event(
            "level.mix",
            format!(
                "mix={} volume={:.3} graph_running={}",
                mix.id, mix.volume, graph_running
            ),
        );
        if graph_running {
            let output = if mix_uses_persistent_audio_core(&mix) {
                self.execute_native_mix_control_unlocked(
                    "set_mix_master",
                    &mix.id,
                    None,
                    serde_json::json!({ "volume": mix.volume }),
                )
            } else {
                command_execution(self.pw.execute(plan_pw_set_mix_volume(&mix, mix.volume)))
            };
            self.log_command_executions("level.mix", &[output]);
        }
        drop(audio_commands);
        Ok(mix)
    }

    pub fn set_mix_mute(&self, mix_id: String, muted: bool) -> Result<Mix, EngineError> {
        let graph_running = self.audio_graph_running_cached();
        let audio_commands = if graph_running {
            Some(self.lock_audio_commands()?)
        } else {
            None
        };
        let mix = self.update_config(|config| config.set_mix_mute(mix_id, muted))??;
        self.log_engine_event(
            "level.mix",
            format!(
                "mix={} muted={} graph_running={}",
                mix.id, mix.muted, graph_running
            ),
        );
        if graph_running {
            let output = if mix_uses_persistent_audio_core(&mix) {
                self.execute_native_mix_control_unlocked(
                    "set_mix_master",
                    &mix.id,
                    None,
                    serde_json::json!({ "muted": mix.muted }),
                )
            } else {
                command_execution(self.pw.execute(plan_pw_set_mix_mute(&mix, mix.muted)))
            };
            self.log_command_executions("level.mix", &[output]);
        }
        drop(audio_commands);
        Ok(mix)
    }

    pub fn set_channel_volume(
        &self,
        channel_id: String,
        mix_id: String,
        volume: f32,
    ) -> Result<wavelinux_model::MixBus, EngineError> {
        let graph_running = self.audio_graph_running_cached();
        let audio_commands = if graph_running {
            Some(self.lock_audio_commands()?)
        } else {
            None
        };
        let (bus, channel) = self.update_config(|config| {
            let bus = config.set_channel_volume(channel_id.clone(), mix_id.clone(), volume)?;
            let channel = config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .cloned()
                .ok_or_else(|| ModelError::ChannelNotFound(channel_id.clone()))?;
            Ok((bus, channel))
        })??;

        self.log_engine_event(
            "level.channel",
            format!(
                "channel={} mix={} volume={:.3} linked={} graph_running={}",
                channel.id, mix_id, bus.volume, channel.linked, graph_running
            ),
        );
        if !graph_running {
            return Ok(bus);
        }

        let mut outputs = Vec::new();
        if channel.linked {
            for (linked_mix_id, linked_bus) in &channel.mix_buses {
                if !linked_bus.enabled {
                    continue;
                }
                outputs.extend(self.execute_channel_bus_volume_unlocked(
                    &channel.id,
                    linked_mix_id,
                    linked_bus.volume,
                ));
            }
        } else if bus.enabled {
            outputs.extend(self.execute_channel_bus_volume_unlocked(
                &channel.id,
                &mix_id,
                bus.volume,
            ));
        }
        self.log_command_executions("level.channel", &outputs);
        drop(audio_commands);
        Ok(bus)
    }

    pub fn set_channel_mute(
        &self,
        channel_id: String,
        mix_id: String,
        muted: bool,
    ) -> Result<wavelinux_model::MixBus, EngineError> {
        let graph_running = self.audio_graph_running_cached();
        let audio_commands = if graph_running {
            Some(self.lock_audio_commands()?)
        } else {
            None
        };
        let bus = self.update_config(|config| {
            config.set_channel_mute(channel_id.clone(), mix_id.clone(), muted)
        })??;
        self.log_engine_event(
            "level.channel",
            format!(
                "channel={} mix={} muted={} graph_running={}",
                channel_id, mix_id, bus.muted, graph_running
            ),
        );
        if !graph_running {
            return Ok(bus);
        }

        let outputs = if bus.enabled {
            self.execute_channel_bus_mute_unlocked(&channel_id, &mix_id, bus.muted)
        } else {
            Vec::new()
        };
        self.log_command_executions("level.channel", &outputs);
        drop(audio_commands);
        Ok(bus)
    }

    pub fn set_channel_bus_enabled(
        self: &Arc<Self>,
        channel_id: String,
        mix_id: String,
        enabled: bool,
    ) -> Result<wavelinux_model::MixBus, EngineError> {
        let graph_running = self.audio_graph_running_cached();
        let audio_commands = if graph_running && graph_prefix() == "wavelinux6" {
            Some(self.lock_audio_commands()?)
        } else {
            None
        };
        let bus = self.update_config(|config| {
            config.set_channel_bus_enabled(channel_id.clone(), mix_id.clone(), enabled)
        })??;
        if graph_running && graph_prefix() == "wavelinux6" {
            let output = self.execute_native_mix_control_unlocked(
                "set_mix_bus",
                &mix_id,
                Some(&channel_id),
                serde_json::json!({ "enabled": bus.enabled }),
            );
            self.log_command_executions("level.channel", &[output]);
        } else {
            let _ = self.repair_audio_graph_if_running();
        }
        drop(audio_commands);
        Ok(bus)
    }
}
