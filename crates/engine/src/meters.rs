use super::*;

impl WaveLinuxEngine {
    pub fn observe_meters(&self) -> Result<Vec<LevelMeter>, EngineError> {
        let audio_graph_running = {
            let runtime = self.read_runtime()?;
            runtime.status.audio_graph_running && !self.stop.load(Ordering::SeqCst)
        };
        let target_revision = MeterTargetRevision::new(self.revisions(), audio_graph_running);
        if graph_prefix() != "wavelinux6" {
            let cached_update = self
                .meter_supervisor
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?
                .snapshot_for_revision(target_revision, true);
            if let Some(update) = cached_update {
                self.log_meter_supervisor_update(&update);
                let meters = update.meters;
                let mut runtime = self.write_runtime()?;
                if runtime.status.audio_graph_running {
                    runtime.graph.meters = meters.clone();
                } else if !runtime.graph.meters.is_empty() {
                    runtime.graph.meters.clear();
                }
                return Ok(meters);
            }
        }

        let graph = self.read_runtime()?.graph.clone();
        let config = self.read_config()?.clone();
        let config = effective_config_with_runtime_auto_devices(&config, &graph);
        let config = config_with_unavailable_effects_bypassed(&config, &graph);
        let config = self.config_with_unhealthy_effects_bypassed(&config);
        let meters = self.refresh_meter_supervisor(&config, &graph, audio_graph_running, true)?;
        let mut runtime = self.write_runtime()?;
        if runtime.status.audio_graph_running {
            runtime.graph.meters = meters.clone();
        } else if !runtime.graph.meters.is_empty() {
            runtime.graph.meters.clear();
        }
        Ok(meters)
    }

    pub fn open_meter_stream(&self) -> Result<CoreMeterStream, EngineError> {
        let socket_path = self.paths.meter_stream_socket();
        let result = (|| -> Result<CoreMeterStream, EngineError> {
            let mut stream = UnixStream::connect(&socket_path).map_err(|error| {
                EngineError::Io(format!(
                    "failed to connect meter stream {}: {error}",
                    socket_path.display()
                ))
            })?;
            stream.set_read_timeout(Some(Duration::from_secs(1)))?;
            let header = wavelinux_dsp::read_meter_stream_header(&mut stream).map_err(|error| {
                EngineError::Io(format!("invalid meter stream handshake: {error}"))
            })?;
            stream.set_read_timeout(Some(Duration::from_millis(200)))?;
            let frame_bytes = Vec::with_capacity(header.frame_bytes());
            let mut client = CoreMeterStream {
                stream,
                header,
                frame_bytes,
                target_revision: None,
                targets: Vec::new(),
                last_sequence: 0,
            };
            self.refresh_meter_stream_targets(&mut client)?;
            Ok(client)
        })();

        match result {
            Ok(client) => {
                self.meter_transport.connected(client.header.slots.len());
                self.change_signal.notify_state();
                self.log_engine_event(
                    "meters.transport",
                    format!(
                        "connected protocol={} slots={} rate_hz={}",
                        wavelinux_dsp::METER_STREAM_PROTOCOL_VERSION,
                        client.header.slots.len(),
                        client.header.rate_hz
                    ),
                );
                Ok(client)
            }
            Err(error) => {
                self.meter_transport.disconnected(Some(error.to_string()));
                Err(error)
            }
        }
    }

    pub fn read_meter_stream(
        &self,
        client: &mut CoreMeterStream,
    ) -> Result<Vec<LevelMeter>, EngineError> {
        let audio_graph_running =
            self.read_runtime()?.status.audio_graph_running && !self.stop.load(Ordering::SeqCst);
        let revision = MeterTargetRevision::new(self.revisions(), audio_graph_running);
        if client.target_revision != Some(revision) {
            self.refresh_meter_stream_targets(client)?;
        }

        let frame = match wavelinux_dsp::read_meter_stream_frame(
            &mut client.stream,
            client.header.slots.len(),
            &mut client.frame_bytes,
        ) {
            Ok(frame) => frame,
            Err(error) => {
                let message = format!("meter stream read failed: {error}");
                self.meter_transport.disconnected(Some(message.clone()));
                self.change_signal.notify_state();
                return Err(EngineError::Io(message));
            }
        };
        if client.last_sequence != 0 && frame.sequence <= client.last_sequence {
            let message = format!(
                "meter stream sequence regressed from {} to {}",
                client.last_sequence, frame.sequence
            );
            self.meter_transport.disconnected(Some(message.clone()));
            self.change_signal.notify_state();
            return Err(EngineError::Io(message));
        }
        client.last_sequence = frame.sequence;
        self.meter_transport.frame_received(frame.sequence);

        Ok(client
            .targets
            .iter()
            .filter_map(|target| {
                let sample = frame.samples.get(target.slot_index)?;
                Some(LevelMeter {
                    node_id: target.node_id.clone(),
                    peak_left: meter_output_level(sample.peak_left, target.gain),
                    peak_right: meter_output_level(sample.peak_right, target.gain),
                })
            })
            .collect())
    }

    pub fn close_meter_stream(&self) {
        self.meter_transport.disconnected(None);
        self.change_signal.notify_state();
    }

    pub fn record_meter_fallback_poll(&self) {
        self.meter_transport.fallback_polled();
    }

    fn refresh_meter_stream_targets(
        &self,
        client: &mut CoreMeterStream,
    ) -> Result<(), EngineError> {
        let (graph, audio_graph_running) = {
            let runtime = self.read_runtime()?;
            (
                runtime.graph.clone(),
                runtime.status.audio_graph_running && !self.stop.load(Ordering::SeqCst),
            )
        };
        let revision = MeterTargetRevision::new(self.revisions(), audio_graph_running);
        if !audio_graph_running {
            client.targets.clear();
            client.target_revision = Some(revision);
            return Ok(());
        }
        let config = self.read_config()?.clone();
        let config = effective_config_with_runtime_auto_devices(&config, &graph);
        let config = config_with_unavailable_effects_bypassed(&config, &graph);
        let config = self.config_with_unhealthy_effects_bypassed(&config);
        let targets = meter_targets_for_config_with_devices(&config, &graph.inputs);
        let slot_indices = client
            .header
            .slots
            .iter()
            .enumerate()
            .map(|(index, slot)| (slot.id.as_str(), (index, slot.kind)))
            .collect::<BTreeMap<_, _>>();
        client.targets = targets
            .into_iter()
            .filter_map(|target| {
                let bus_channel_id = channel_id_from_bus_meter_id(&target.node_id);
                let slot_id = bus_channel_id.unwrap_or(&target.node_id);
                let (slot_index, kind) = *slot_indices.get(slot_id)?;
                let gain = match kind {
                    wavelinux_dsp::MeterStreamSlotKind::Channel => {
                        if target.muted {
                            0.0
                        } else {
                            target.gain
                        }
                    }
                    // The core mix meter already includes all bus and master gains.
                    wavelinux_dsp::MeterStreamSlotKind::Mix => 1.0,
                };
                Some(CoreMeterTarget {
                    node_id: target.node_id,
                    slot_index,
                    gain,
                })
            })
            .collect();
        client.target_revision = Some(revision);
        Ok(())
    }
}
