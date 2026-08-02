use super::*;

impl WaveLinuxEngine {
    pub fn repair_audio_graph(&self) -> Result<RepairReport, EngineError> {
        self.log_engine_event("repair.start", "requested audio graph repair");
        let report = {
            let _audio_commands = self.lock_audio_commands()?;
            self.repair_audio_graph_unlocked()?
        };
        let _ = self.refresh_runtime();
        if report.outputs.iter().all(|output| output.error.is_none()) {
            self.finalize_wavelinux5_migration();
        }
        Ok(report)
    }

    fn finalize_wavelinux5_migration(&self) {
        let marker = self.paths.wavelinux5_migration_marker();
        if !marker.is_file() {
            return;
        }
        let Some(config_dir) = wavelinux5_config_dir() else {
            return;
        };
        if config_dir == self.paths.config_dir {
            return;
        }
        match fs::remove_dir_all(&config_dir) {
            Ok(()) => {
                let _ = fs::remove_file(&marker);
                self.log_engine_event(
                    "config.migration",
                    format!(
                        "completed WaveLinux5 to WaveLinux6 migration and removed {}",
                        config_dir.display()
                    ),
                );
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let _ = fs::remove_file(&marker);
            }
            Err(err) => self.log_engine_event(
                "config.migration",
                format!(
                    "WaveLinux6 is active but old config cleanup failed for {}: {err}",
                    config_dir.display()
                ),
            ),
        }
    }

    pub(super) fn sync_active_mix_routes_unlocked(
        &self,
        config: &MixerConfig,
        view: IncrementalMixRouteView<'_>,
        active_app_channel_ids: &BTreeSet<String>,
        active_mix_ids: &BTreeSet<String>,
        route_health: &[RouteHealthIssue],
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let started = Instant::now();
        let mut seen = BTreeSet::new();
        let unhealthy_module_ids = route_health
            .iter()
            .filter(|issue| issue.reason != RouteHealthReason::LevelMismatch)
            .filter_map(|issue| issue.module_id.clone())
            .collect::<BTreeSet<_>>();
        let stale_modules = view
            .managed_modules
            .iter()
            .filter(|module| {
                managed_module_is_incremental_mix_route(module)
                    && (module_is_stale_for_active_routes(
                        module,
                        config,
                        active_app_channel_ids,
                        active_mix_ids,
                    ) || unhealthy_module_ids.contains(&module.module_id)
                        || module_dedupe_key_for_config(module, config)
                            .is_some_and(|key| !seen.insert(key)))
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut outputs = self
            .pw
            .execute_all(plan_unload_modules(&stale_modules))
            .into_iter()
            .map(command_execution)
            .collect::<Vec<_>>();
        let plan =
            plan_ensure_graph_for_active_routes(config, active_app_channel_ids, active_mix_ids);
        let mut skipped = Vec::new();
        let commands = plan
            .commands
            .into_iter()
            .filter(command_is_incremental_mix_route)
            .filter_map(|command| {
                if !route_command_endpoints_available(&command, view.graph) {
                    skipped.push(skipped_command_with_stderr(
                        command,
                        "live route endpoint is not visible yet; retrying on the next audio event",
                    ));
                    return None;
                }
                (!repair_command_is_satisfied(
                    &command,
                    view.graph,
                    view.source_outputs,
                    view.sink_inputs,
                    view.managed_modules,
                ))
                .then_some(command)
            })
            .collect::<Vec<_>>();
        let load_count = commands.len();
        outputs.extend(skipped);
        outputs.extend(
            self.pw
                .execute_all(commands)
                .into_iter()
                .map(command_execution),
        );
        if route_health
            .iter()
            .any(|issue| issue.reason == RouteHealthReason::LevelMismatch)
            || active_mix_routes_have_custom_levels(config, active_app_channel_ids, active_mix_ids)
        {
            outputs.extend(self.apply_managed_route_levels(config)?);
        }
        self.log_engine_event(
            "repair.active-routes",
            format!(
                "active_channels={} active_mixes={} loaded={} outputs={} failed={} elapsed_ms={}",
                active_app_channel_ids.len(),
                active_mix_ids.len(),
                load_count,
                outputs.len(),
                outputs
                    .iter()
                    .filter(|output| output.error.is_some())
                    .count(),
                started.elapsed().as_millis(),
            ),
        );
        Ok(outputs)
    }

    pub(super) fn repair_audio_graph_unlocked(&self) -> Result<RepairReport, EngineError> {
        self.repair_audio_graph_unlocked_from_snapshot(None)
    }

    pub(super) fn repair_audio_graph_unlocked_from_snapshot(
        &self,
        initial_state: Option<AudioStateSnapshot>,
    ) -> Result<RepairReport, EngineError> {
        let started = Instant::now();
        let mut phase_started = Instant::now();
        let mut repair_phases = Vec::new();
        let saved_config = self.read_config()?.clone();
        let reused_initial_snapshot = initial_state.is_some();
        let (mut pre_cleanup_state, mut repair_snapshot_timings) = match initial_state {
            Some(snapshot) => (snapshot, Vec::new()),
            None => self.audio_state_snapshot_for_config_timed(Some(&saved_config))?,
        };
        let mut bluetooth_cards = self
            .bluetooth_audio_cards_for_devices(
                pre_cleanup_state.bluetooth_cards.clone(),
                &pre_cleanup_state.graph.inputs,
                &pre_cleanup_state.graph.outputs,
            )
            .unwrap_or_default();
        let mut outputs = self.ensure_bluetooth_a2dp_profiles_for_cards(&bluetooth_cards, true)?;
        self.log_command_executions("repair.bluetooth", &outputs);
        if outputs
            .iter()
            .any(|output| !output.skipped && output.error.is_none())
        {
            thread::sleep(Duration::from_millis(250));
            let (next_state, timings) =
                self.audio_state_snapshot_for_config_timed(Some(&saved_config))?;
            pre_cleanup_state = next_state;
            repair_snapshot_timings.extend(timings);
            bluetooth_cards = self
                .bluetooth_audio_cards_for_devices(
                    pre_cleanup_state.bluetooth_cards.clone(),
                    &pre_cleanup_state.graph.inputs,
                    &pre_cleanup_state.graph.outputs,
                )
                .unwrap_or_default();
        }
        let default_source = pre_cleanup_state.default_source.clone();
        let default_sink = pre_cleanup_state.default_sink.clone();
        let config =
            self.config_with_unhealthy_effects_bypassed(&effective_config_with_profiled_devices(
                &saved_config,
                &pre_cleanup_state.graph.inputs,
                &pre_cleanup_state.graph.outputs,
                &bluetooth_cards,
                default_source.as_deref(),
                default_sink.as_deref(),
                pre_cleanup_state.active_playback_sink.as_deref(),
            ));
        record_refresh_phase(&mut repair_phases, &mut phase_started, "bluetooth");
        let active_app_channel_ids =
            active_app_channel_ids_for_graph(&config, &pre_cleanup_state.graph);
        let active_mix_ids = active_mix_ids_for_routes(
            &config,
            &pre_cleanup_state.graph,
            &pre_cleanup_state.routes.source_output_routes,
            &pre_cleanup_state.routes.sink_input_routes,
        );
        let monitor_preroute_outputs = self.preload_monitor_output_routes_for_config(
            &config,
            &active_mix_ids,
            &pre_cleanup_state,
        )?;
        let monitor_preroute_mutated =
            command_executions_may_have_mutated_graph(&monitor_preroute_outputs);
        let preserve_stale_monitor_routes = monitor_preroute_outputs.iter().any(|output| {
            output.error.is_some() || output.stderr.contains("preserving existing monitor route")
        });
        self.log_command_executions("repair.preroute", &monitor_preroute_outputs);
        outputs.extend(monitor_preroute_outputs);
        let cleanup_outputs = self.cleanup_stale_modules_for_config_from_snapshot(
            &config,
            &active_app_channel_ids,
            &active_mix_ids,
            preserve_stale_monitor_routes,
            &pre_cleanup_state.routes.managed_modules,
        )?;
        let cleanup_mutated = command_executions_may_have_mutated_graph(&cleanup_outputs);
        self.log_command_executions("repair.cleanup", &cleanup_outputs);
        outputs.extend(cleanup_outputs);
        self.rebuild_effect_chain_configs_from_config(&config, &graph_prefix())?;
        record_refresh_phase(&mut repair_phases, &mut phase_started, "prepare");

        let mut planned =
            plan_ensure_graph_for_active_routes(&config, &active_app_channel_ids, &active_mix_ids);
        let planned_count = planned.commands.len();
        let reused_planning_snapshot = !monitor_preroute_mutated && !cleanup_mutated;
        let planning_state = if reused_planning_snapshot {
            pre_cleanup_state.clone()
        } else {
            let (snapshot, timings) = self.audio_state_snapshot_for_config_timed(Some(&config))?;
            repair_snapshot_timings.extend(timings);
            snapshot
        };
        let existing_graph = planning_state.graph;
        let managed_modules = planning_state.routes.managed_modules;
        let source_outputs = planning_state.routes.source_output_routes;
        let sink_inputs = planning_state.routes.sink_input_routes;
        let active_effect_channel_ids = config
            .channels
            .iter()
            .filter(|channel| {
                channel_uses_persistent_audio_core(channel) || channel_has_active_effects(channel)
            })
            .map(|channel| channel.id.clone())
            .collect::<BTreeSet<_>>();
        planned.commands.retain(|command| {
            if command_routes_active_effect_channel(command, &active_effect_channel_ids) {
                return true;
            }
            !repair_command_is_satisfied(
                command,
                &existing_graph,
                &source_outputs,
                &sink_inputs,
                &managed_modules,
            )
        });

        let (graph_commands, mut route_commands) = split_repair_commands(&planned.commands);
        self.log_engine_event(
            "repair.plan",
            format!(
                "planned={} retained={} graph_commands={} route_commands={} managed_modules={} source_outputs={} sink_inputs={} inputs={} outputs={}",
                planned_count,
                planned.commands.len(),
                graph_commands.len(),
                route_commands.len(),
                managed_modules.len(),
                source_outputs.len(),
                sink_inputs.len(),
                existing_graph.inputs.len(),
                existing_graph.outputs.len(),
            ),
        );
        self.log_engine_event(
            "repair.snapshot",
            format!(
                "initial={} planning={} commands={}",
                if reused_initial_snapshot {
                    "reused"
                } else {
                    "captured"
                },
                if reused_planning_snapshot {
                    "reused"
                } else {
                    "captured"
                },
                if repair_snapshot_timings.is_empty() {
                    "none".into()
                } else {
                    format_snapshot_command_timings(&repair_snapshot_timings)
                },
            ),
        );
        record_refresh_phase(&mut repair_phases, &mut phase_started, "plan");
        outputs.extend(
            self.pw
                .execute_all(graph_commands)
                .into_iter()
                .map(command_execution),
        );
        record_refresh_phase(&mut repair_phases, &mut phase_started, "base_graph");

        outputs.extend(self.start_effect_chain_processes(&config)?);
        let mut route_config = config.clone();
        let active_effect_channels = config
            .channels
            .iter()
            .filter(|channel| {
                channel_uses_persistent_audio_core(channel) || channel_has_active_effects(channel)
            })
            .collect::<Vec<_>>();
        if !active_effect_channels.is_empty() {
            if graph_prefix() == "wavelinux6" {
                let _ = self.wait_for_persistent_core_nodes_ready_for_routing(
                    &active_effect_channels,
                    &config.mixes,
                );
            } else {
                for channel in &active_effect_channels {
                    let _ = self.wait_for_effect_nodes_ready_for_routing(channel);
                }
            }
            let post_effect_graph = RuntimeGraph {
                inputs: self.pw.list_inputs().unwrap_or_default(),
                outputs: self.pw.list_outputs().unwrap_or_default(),
                ..RuntimeGraph::default()
            };
            let mut missing_effect_channels = Vec::new();
            for channel in &mut route_config.channels {
                if !channel_uses_persistent_audio_core(channel)
                    && !channel_has_active_effects(channel)
                {
                    continue;
                }
                if effect_chain_endpoint_readiness_for_graph(&post_effect_graph, channel).ready() {
                    continue;
                }
                missing_effect_channels.push(channel.name.clone());
                for effect in &mut channel.effects {
                    effect.bypassed = true;
                }
            }
            if !missing_effect_channels.is_empty() {
                self.log_engine_event(
                    "repair.effects",
                    format!(
                        "missing FX sources for {}; routing affected channels from raw monitors",
                        missing_effect_channels.join(", ")
                    ),
                );
                let fallback_plan = plan_ensure_graph_for_active_routes(
                    &route_config,
                    &active_app_channel_ids,
                    &active_mix_ids,
                );
                let (_, fallback_route_commands) = split_repair_commands(&fallback_plan.commands);
                let managed_modules = self.pw.managed_modules().unwrap_or_default();
                let source_outputs = self.pw.source_output_routes().unwrap_or_default();
                let sink_inputs = self.pw.sink_input_routes().unwrap_or_default();
                route_commands = fallback_route_commands
                    .into_iter()
                    .filter(|command| {
                        !repair_command_is_satisfied(
                            command,
                            &post_effect_graph,
                            &source_outputs,
                            &sink_inputs,
                            &managed_modules,
                        )
                    })
                    .collect();
            }
            if graph_prefix() != "wavelinux6" {
                outputs.extend(self.cleanup_modules(|module| {
                    matches!(
                        module.role.as_deref(),
                        Some("channel_to_mix") | Some("channel_to_effect")
                    ) && module
                        .channel_id
                        .as_deref()
                        .is_some_and(|channel_id| active_effect_channel_ids.contains(channel_id))
                })?);
            }
        }
        record_refresh_phase(&mut repair_phases, &mut phase_started, "effects");

        outputs.extend(
            self.pw
                .execute_all(route_commands)
                .into_iter()
                .map(command_execution),
        );
        record_refresh_phase(&mut repair_phases, &mut phase_started, "routes");
        outputs.extend(self.apply_graph_levels(&route_config)?);
        record_refresh_phase(&mut repair_phases, &mut phase_started, "levels");
        let linked_effect_channel_ids = route_config
            .channels
            .iter()
            .filter(|channel| {
                channel_uses_persistent_audio_core(channel) || channel_has_active_effects(channel)
            })
            .map(|channel| channel.id.clone())
            .collect::<BTreeSet<_>>();
        let route_issues =
            self.wait_for_effect_routes_linked(&route_config, &linked_effect_channel_ids);
        if !route_issues.is_empty() {
            self.log_engine_event(
                "repair.effects",
                format!(
                    "FX loopbacks still unhealthy after repair: {}",
                    route_health_summary(&route_issues)
                ),
            );
        }
        record_refresh_phase(&mut repair_phases, &mut phase_started, "route_link");
        outputs.extend(self.apply_default_device_locks(&route_config)?);
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        outputs.extend(self.execute_capture_stream_moves_unlocked(&route_config, &source_outputs)?);
        record_refresh_phase(&mut repair_phases, &mut phase_started, "finalize");
        self.log_command_executions("repair.outputs", &outputs);
        self.log_engine_event(
            "repair.end",
            format!(
                "outputs={} failed={} skipped={} elapsed_ms={} phases={}",
                outputs.len(),
                outputs
                    .iter()
                    .filter(|output| output.error.is_some())
                    .count(),
                outputs.iter().filter(|output| output.skipped).count(),
                started.elapsed().as_millis(),
                repair_phases
                    .iter()
                    .map(|(phase, elapsed_ms)| format!("{phase}={elapsed_ms}ms"))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        );
        Ok(RepairReport {
            dry_run: self.options.dry_run,
            planned,
            outputs,
        })
    }

    pub(super) fn repair_auto_device_routes_unlocked(
        &self,
        initial_state: Option<AudioStateSnapshot>,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let started = Instant::now();
        let saved_config = self.read_config()?.clone();
        let reused_initial_snapshot = initial_state.is_some();
        let initial_state = match initial_state {
            Some(snapshot) => snapshot,
            None => {
                self.audio_state_snapshot_for_config_timed(Some(&saved_config))?
                    .0
            }
        };
        let bluetooth_cards = self
            .bluetooth_audio_cards_for_devices(
                initial_state.bluetooth_cards.clone(),
                &initial_state.graph.inputs,
                &initial_state.graph.outputs,
            )
            .unwrap_or_default();
        let default_source = initial_state.default_source.clone();
        let default_sink = initial_state.default_sink.clone();
        let config =
            self.config_with_unhealthy_effects_bypassed(&effective_config_with_profiled_devices(
                &saved_config,
                &initial_state.graph.inputs,
                &initial_state.graph.outputs,
                &bluetooth_cards,
                default_source.as_deref(),
                default_sink.as_deref(),
                initial_state.active_playback_sink.as_deref(),
            ));
        let mut persistent_core_outputs = Vec::new();
        if graph_prefix() == "wavelinux6" {
            self.rebuild_effect_chain_configs_from_config(&config, &graph_prefix())?;
            persistent_core_outputs.push(self.start_persistent_audio_core_process());
            if persistent_core_outputs
                .iter()
                .all(|output| output.error.is_none())
            {
                persistent_core_outputs.extend(self.sync_persistent_audio_core_targets()?);
            }
        }
        let active_app_channel_ids =
            active_app_channel_ids_for_graph(&config, &initial_state.graph);
        let active_mix_ids = active_mix_ids_for_routes(
            &config,
            &initial_state.graph,
            &initial_state.routes.source_output_routes,
            &initial_state.routes.sink_input_routes,
        );
        let monitor_preroute_outputs = self.preload_monitor_output_routes_for_config(
            &config,
            &active_mix_ids,
            &initial_state,
        )?;
        let monitor_preroute_mutated =
            command_executions_may_have_mutated_graph(&monitor_preroute_outputs);
        let preserve_stale_monitor_routes = monitor_preroute_outputs.iter().any(|output| {
            output.error.is_some() || output.stderr.contains("preserving existing monitor route")
        });
        let mut outputs = persistent_core_outputs;
        outputs.extend(monitor_preroute_outputs);
        let cleanup_outputs = self.cleanup_stale_auto_device_modules_for_config_from_snapshot(
            &config,
            &active_mix_ids,
            preserve_stale_monitor_routes,
            &initial_state.routes.managed_modules,
        );
        let cleanup_mutated = command_executions_may_have_mutated_graph(&cleanup_outputs);
        outputs.extend(cleanup_outputs);

        let mut planned =
            plan_ensure_graph_for_active_routes(&config, &active_app_channel_ids, &active_mix_ids);
        let planned_count = planned.commands.len();
        let reused_planning_snapshot = !monitor_preroute_mutated && !cleanup_mutated;
        let planning_state = if reused_planning_snapshot {
            initial_state
        } else {
            self.audio_state_snapshot_for_config_timed(Some(&config))?.0
        };
        let existing_graph = planning_state.graph;
        let managed_modules = planning_state.routes.managed_modules;
        let source_outputs = planning_state.routes.source_output_routes;
        let sink_inputs = planning_state.routes.sink_input_routes;
        planned.commands.retain(|command| {
            command_is_auto_device_route(command)
                && !repair_command_is_satisfied(
                    command,
                    &existing_graph,
                    &source_outputs,
                    &sink_inputs,
                    &managed_modules,
                )
        });
        self.log_engine_event(
            "repair.auto-device",
            format!(
                "planned={} retained={} managed_modules={} source_outputs={} sink_inputs={} inputs={} outputs={} initial_snapshot={} planning_snapshot={}",
                planned_count,
                planned.commands.len(),
                managed_modules.len(),
                source_outputs.len(),
                sink_inputs.len(),
                existing_graph.inputs.len(),
                existing_graph.outputs.len(),
                if reused_initial_snapshot { "reused" } else { "captured" },
                if reused_planning_snapshot { "reused" } else { "captured" },
            ),
        );
        outputs.extend(
            self.pw
                .execute_all(planned.commands)
                .into_iter()
                .map(command_execution),
        );
        outputs.extend(self.apply_default_device_locks(&config)?);
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        outputs.extend(self.execute_capture_stream_moves_unlocked(&config, &source_outputs)?);
        self.log_engine_event(
            "repair.auto-device",
            format!(
                "outputs={} failed={} skipped={} elapsed_ms={}",
                outputs.len(),
                outputs
                    .iter()
                    .filter(|output| output.error.is_some())
                    .count(),
                outputs.iter().filter(|output| output.skipped).count(),
                started.elapsed().as_millis(),
            ),
        );
        Ok(outputs)
    }

    pub(super) fn repair_bluetooth_monitor_routes_unlocked(
        &self,
        config: &MixerConfig,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let plan = plan_ensure_graph(config);
        let monitor_commands = plan
            .commands
            .into_iter()
            .filter(command_is_mix_monitor_route)
            .filter(command_targets_bluetooth_sink)
            .collect::<Vec<_>>();
        if monitor_commands.is_empty() {
            return Ok(Vec::new());
        }

        let desired_routes = monitor_commands
            .iter()
            .filter_map(|command| {
                let properties = command_arg_value(&command.args, "source_output_properties=")?;
                let mix_id = graph_property_value_from_arg(properties, "mix_id")?;
                let sink = command_arg_value(&command.args, "sink=")?;
                Some((mix_id.to_owned(), sink.to_owned()))
            })
            .collect::<Vec<_>>();

        let mut outputs = self.cleanup_modules(|module| {
            module.role.as_deref() == Some("mix_monitor")
                && desired_routes.iter().any(|(mix_id, sink)| {
                    module.mix_id.as_deref() == Some(mix_id.as_str())
                        && module
                            .sink_name
                            .as_deref()
                            .is_some_and(|actual| audio_endpoint_names_match(actual, sink))
                })
        })?;

        if !outputs.is_empty() {
            thread::sleep(CLEANUP_MODULE_SETTLE);
        }

        let mut graph = self
            .pw
            .snapshot_for_config_with_effect_availability(None, Vec::new());
        if monitor_commands
            .iter()
            .any(|command| !monitor_route_endpoints_available(command, &graph))
        {
            for _ in 0..6 {
                thread::sleep(Duration::from_millis(200));
                graph = self
                    .pw
                    .snapshot_for_config_with_effect_availability(None, Vec::new());
                if monitor_commands
                    .iter()
                    .all(|command| monitor_route_endpoints_available(command, &graph))
                {
                    break;
                }
            }
        }

        if monitor_commands
            .iter()
            .any(|command| monitor_route_endpoints_available(command, &graph))
        {
            self.log_engine_event(
                "hotplug.output",
                "Bluetooth monitor route reset; waiting for A2DP transport before reconnecting",
            );
            thread::sleep(BLUETOOTH_MONITOR_ROUTE_SETTLE);
            graph = self
                .pw
                .snapshot_for_config_with_effect_availability(None, Vec::new());
        }

        let commands = monitor_commands
            .into_iter()
            .filter_map(|command| {
                if monitor_route_endpoints_available(&command, &graph) {
                    Some(command)
                } else {
                    outputs.push(skipped_command_with_stderr(
                        command,
                        "Bluetooth monitor output is not visible; keeping route disconnected",
                    ));
                    None
                }
            })
            .collect::<Vec<_>>();
        outputs.extend(
            self.pw
                .execute_all(commands)
                .into_iter()
                .map(command_execution),
        );
        Ok(outputs)
    }
}
