use super::*;

impl WaveLinuxEngine {
    pub fn assign_app_to_channel(
        &self,
        channel_id: String,
        matcher: AppMatcher,
    ) -> Result<AppRoute, EngineError> {
        self.update_config(|config| config.assign_app_to_channel(channel_id, matcher))?
    }

    pub fn remove_app_route(&self, matcher: AppMatcher) -> Result<Option<AppRoute>, EngineError> {
        self.update_config(|config| Ok(config.remove_app_route(matcher)))?
    }

    pub fn set_app_volume_preset(
        &self,
        matcher: AppMatcher,
        volume: f32,
    ) -> Result<AppVolumePreset, EngineError> {
        self.update_config(|config| config.set_app_volume_preset(matcher, volume))?
    }

    pub fn remove_app_volume_preset(
        &self,
        matcher: AppMatcher,
    ) -> Result<Option<AppVolumePreset>, EngineError> {
        self.update_config(|config| Ok(config.remove_app_volume_preset(matcher)))?
    }

    pub fn forget_app(&self, matcher: AppMatcher) -> Result<Option<KnownApp>, EngineError> {
        self.update_config(|config| Ok(config.forget_app(matcher)))?
    }

    pub fn restore_app(&self, matcher: AppMatcher) -> Result<Option<KnownApp>, EngineError> {
        self.update_config(|config| Ok(config.restore_app(matcher)))?
    }

    pub fn pin_app_identity(
        &self,
        matcher: AppMatcher,
        label: String,
    ) -> Result<KnownApp, EngineError> {
        self.update_config(|config| config.pin_app_identity(matcher, label))?
    }

    pub fn merge_app_identity(
        &self,
        source: AppMatcher,
        target: AppMatcher,
    ) -> Result<KnownApp, EngineError> {
        self.update_config(|config| config.merge_app_identity(source, target))?
    }

    pub fn reset_app_identity(&self, matcher: AppMatcher) -> Result<Option<KnownApp>, EngineError> {
        self.update_config(|config| Ok(config.reset_app_identity(matcher)))?
    }

    pub fn move_app_stream(
        &self,
        stream_id: String,
        channel_id: String,
    ) -> Result<CommandExecution, EngineError> {
        let saved_config = self.read_config()?.clone();
        let route_config = self.effective_config_for_audio_graph(&saved_config);
        let channel = route_config
            .channels
            .iter()
            .find(|channel| channel.id == channel_id)
            .cloned()
            .ok_or_else(|| ModelError::ChannelNotFound(channel_id.clone()))?;
        let command = plan_move_app_stream(&stream_id, &channel);
        if !self.audio_graph_running_cached() {
            return Ok(skipped_command(command));
        }

        let _audio_commands = self.lock_audio_commands()?;
        let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
        let output = ignore_stale_stream_command(output, &stream_id);
        if output.error.is_none() && !output.skipped {
            let level_outputs = self.apply_managed_route_levels(&route_config)?;
            self.log_command_executions("route.levels", &level_outputs);
        }
        Ok(output)
    }

    pub fn move_app_stream_to_default(
        &self,
        stream_id: String,
    ) -> Result<CommandExecution, EngineError> {
        let command = plan_move_app_stream_to_default(&stream_id);
        if !self.audio_graph_running_cached() {
            return Ok(skipped_command(command));
        }

        let _audio_commands = self.lock_audio_commands()?;
        let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
        Ok(ignore_stale_stream_command(output, &stream_id))
    }

    pub fn set_app_stream_volume(
        &self,
        stream_id: String,
        volume: f32,
    ) -> Result<CommandExecution, EngineError> {
        let command = plan_set_stream_volume(&stream_id, volume);
        if !self.audio_graph_running_cached() {
            return Ok(skipped_command(command));
        }

        let _audio_commands = self.lock_audio_commands()?;
        let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
        Ok(ignore_stale_stream_command(output, &stream_id))
    }

    pub fn set_app_stream_mute(
        &self,
        stream_id: String,
        muted: bool,
    ) -> Result<CommandExecution, EngineError> {
        let command = plan_set_stream_mute(&stream_id, muted);
        if !self.audio_graph_running_cached() {
            return Ok(skipped_command(command));
        }

        let _audio_commands = self.lock_audio_commands()?;
        let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
        Ok(ignore_stale_stream_command(output, &stream_id))
    }
}
