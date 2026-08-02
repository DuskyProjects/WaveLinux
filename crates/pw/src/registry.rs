use super::*;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use wavelinux_model::PipeWireRegistryStatus;

const NODE_INTERFACE: &str = "PipeWire:Interface:Node";
const DEVICE_INTERFACE: &str = "PipeWire:Interface:Device";
const PORT_INTERFACE: &str = "PipeWire:Interface:Port";
const LINK_INTERFACE: &str = "PipeWire:Interface:Link";
const METADATA_INTERFACE: &str = "PipeWire:Interface:Metadata";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RegistryEventKind {
    PlaybackStream,
    CaptureStream,
    Device,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryBatch {
    pub generation: u64,
    pub initial: bool,
    pub changed_objects: usize,
    pub events: BTreeSet<RegistryEventKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeStreamRoute {
    pub stream_node_id: u32,
    pub target_object_serial: String,
    pub target_node_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamRouteBackend {
    PulseCompatibility,
    Native(NativeStreamRoute),
    Unavailable(String),
}

#[derive(Debug, Default)]
struct RegistryState {
    objects: BTreeMap<u32, serde_json::Value>,
    status: PipeWireRegistryStatus,
}

#[derive(Debug, Clone, Default)]
pub struct PipeWireRegistryCache {
    shared: Arc<(Mutex<RegistryState>, Condvar)>,
}

impl PipeWireRegistryCache {
    pub fn mark_connected(&self, reconnect: bool) {
        let (state, ready) = &*self.shared;
        if let Ok(mut state) = state.lock() {
            state.status.available = true;
            state.status.connected = true;
            state.status.last_error = None;
            if reconnect && state.status.initialized {
                state.status.reconnects = state.status.reconnects.saturating_add(1);
                state.status.initialized = false;
                state.objects.clear();
            }
            ready.notify_all();
        }
    }

    pub fn mark_disconnected(&self, error: impl Into<String>) {
        let (state, ready) = &*self.shared;
        if let Ok(mut state) = state.lock() {
            state.status.connected = false;
            state.status.last_error = Some(error.into());
            ready.notify_all();
        }
    }

    pub fn mark_unavailable(&self, error: impl Into<String>) {
        let (state, ready) = &*self.shared;
        if let Ok(mut state) = state.lock() {
            state.status.available = false;
            state.status.connected = false;
            state.status.last_error = Some(error.into());
            ready.notify_all();
        }
    }

    pub fn wait_initialized(&self, timeout: Duration) -> bool {
        let (state, ready) = &*self.shared;
        let Ok(state) = state.lock() else {
            return false;
        };
        if state.status.initialized {
            return true;
        }
        ready
            .wait_timeout_while(state, timeout, |state| {
                !state.status.initialized
                    && (state.status.connected || state.status.last_error.is_none())
            })
            .map(|(state, _)| state.status.initialized)
            .unwrap_or(false)
    }

    pub fn status(&self) -> PipeWireRegistryStatus {
        self.shared
            .0
            .lock()
            .map(|state| state.status.clone())
            .unwrap_or_default()
    }

    pub fn apply_batch(&self, objects: Vec<serde_json::Value>) -> RegistryBatch {
        let (state, ready) = &*self.shared;
        let Ok(mut state) = state.lock() else {
            return RegistryBatch {
                generation: 0,
                initial: false,
                changed_objects: 0,
                events: BTreeSet::new(),
            };
        };
        let was_initialized = state.status.initialized;
        let mut changed_objects = 0_usize;
        let mut events = BTreeSet::new();
        for update in objects {
            let Some(id) = object_id(&update) else {
                continue;
            };
            let old = state.objects.get(&id).cloned();
            if object_removed(&update) {
                if let Some(old) = state.objects.remove(&id) {
                    changed_objects = changed_objects.saturating_add(1);
                    classify_registry_object(&old, true, &mut events);
                }
                continue;
            }

            let mut merged = old.clone().unwrap_or_else(|| serde_json::json!({}));
            merge_json(&mut merged, update);
            if old.as_ref() == Some(&merged) {
                continue;
            }
            changed_objects = changed_objects.saturating_add(1);
            classify_registry_object(&merged, old.is_none(), &mut events);
            observe_direct_error(&old, &merged, &mut state.status);
            state.objects.insert(id, merged);
        }

        state.status.connected = true;
        state.status.available = true;
        state.status.batches_received = state.status.batches_received.saturating_add(1);
        state.status.objects_changed = state
            .status
            .objects_changed
            .saturating_add(changed_objects as u64);
        if changed_objects > 0 {
            state.status.generation = state.status.generation.saturating_add(1);
            state.status.last_event_unix = unix_now();
        }
        refresh_counts(&mut state);
        // `pw-dump --monitor` can emit an empty bootstrap batch before its
        // initial registry snapshot. Do not expose that transient empty graph
        // to reconciliation; at least one node is always present in a usable
        // PipeWire registry.
        if !state.status.initialized && state.status.node_count > 0 {
            state.status.initialized = true;
        }
        let initial = !was_initialized && state.status.initialized;
        let result = RegistryBatch {
            generation: state.status.generation,
            initial,
            changed_objects,
            events: if !was_initialized {
                BTreeSet::new()
            } else {
                events
            },
        };
        ready.notify_all();
        result
    }

    pub fn audio_state_snapshot(
        &self,
        config: Option<&MixerConfig>,
        effect_availability: Vec<EffectAvailability>,
    ) -> Option<(AudioStateSnapshot, u64)> {
        let objects = {
            let state = self.shared.0.lock().ok()?;
            if !state.status.connected || !state.status.initialized {
                return None;
            }
            (state.objects.clone(), state.status.generation)
        };
        Some((
            audio_state_from_registry(&objects.0, config, effect_availability),
            objects.1,
        ))
    }

    pub fn playback_route_backend(
        &self,
        stream_id: &str,
        target_node_name: &str,
    ) -> Option<StreamRouteBackend> {
        self.stream_route_backend(stream_id, "Stream/Output/Audio", target_node_name)
    }

    pub fn capture_route_backend(
        &self,
        stream_id: &str,
        target_node_name: &str,
    ) -> Option<StreamRouteBackend> {
        self.stream_route_backend(stream_id, "Stream/Input/Audio", target_node_name)
    }

    fn stream_route_backend(
        &self,
        stream_id: &str,
        media_class: &str,
        target_node_name: &str,
    ) -> Option<StreamRouteBackend> {
        let state = self.shared.0.lock().ok()?;
        if !state.status.connected || !state.status.initialized {
            return None;
        }

        let clients = registry_client_properties(&state.objects);
        let stream = state.objects.values().find(|object| {
            if object_type(object) != Some(NODE_INTERFACE) {
                return false;
            }
            let props = object_properties(object);
            registry_property_string(&props, "media.class").as_deref() == Some(media_class)
                && registry_stream_id(object, &props) == stream_id
        })?;
        let stream_props = object_properties(stream);
        let client_props =
            registry_property_string(&stream_props, "client.id").and_then(|id| clients.get(&id));
        let client_api = registry_property_string(&stream_props, "client.api").or_else(|| {
            client_props.and_then(|props| registry_property_string(props, "client.api"))
        });
        if client_api.as_deref() == Some("pipewire-pulse") {
            return Some(StreamRouteBackend::PulseCompatibility);
        }
        if registry_property_string(&stream_props, "node.dont-move")
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        {
            return Some(StreamRouteBackend::Unavailable(format!(
                "native stream {stream_id} does not permit target changes"
            )));
        }

        let Some(target) = registry_node_by_name(&state.objects, target_node_name) else {
            return Some(StreamRouteBackend::Unavailable(format!(
                "target node {target_node_name} is not present in registry generation {}",
                state.status.generation
            )));
        };
        let target_props = object_properties(target);
        let Some(target_object_serial) = registry_property_string(&target_props, "object.serial")
        else {
            return Some(StreamRouteBackend::Unavailable(format!(
                "target node {target_node_name} has no object.serial"
            )));
        };
        let Some(stream_node_id) = object_id(stream) else {
            return Some(StreamRouteBackend::Unavailable(format!(
                "native stream {stream_id} has no node id"
            )));
        };
        Some(StreamRouteBackend::Native(NativeStreamRoute {
            stream_node_id,
            target_object_serial,
            target_node_name: target_node_name.to_owned(),
        }))
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

fn object_id(value: &serde_json::Value) -> Option<u32> {
    value
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn object_removed(value: &serde_json::Value) -> bool {
    value.get("info").is_some_and(serde_json::Value::is_null) && value.get("type").is_none()
}

fn merge_json(target: &mut serde_json::Value, update: serde_json::Value) {
    match (target, update) {
        (serde_json::Value::Object(target), serde_json::Value::Object(update)) => {
            for (key, value) in update {
                match target.get_mut(&key) {
                    Some(existing) => merge_json(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, update) => *target = update,
    }
}

fn object_type(value: &serde_json::Value) -> Option<&str> {
    value.get("type").and_then(serde_json::Value::as_str)
}

fn object_properties(value: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
    value
        .pointer("/info/props")
        .or_else(|| value.get("props"))
        .and_then(serde_json::Value::as_object)
        .map(|props| {
            props
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn classify_registry_object(
    value: &serde_json::Value,
    added_or_removed: bool,
    events: &mut BTreeSet<RegistryEventKind>,
) {
    match object_type(value) {
        Some(NODE_INTERFACE) => {
            let props = object_properties(value);
            match registry_property_string(&props, "media.class").as_deref() {
                Some("Stream/Output/Audio") if !registry_node_is_owned(&props) => {
                    events.insert(RegistryEventKind::PlaybackStream);
                }
                Some("Stream/Input/Audio") if !registry_node_is_owned(&props) => {
                    events.insert(RegistryEventKind::CaptureStream);
                }
                Some("Audio/Source") | Some("Audio/Sink")
                    if added_or_removed && !registry_node_is_owned(&props) =>
                {
                    events.insert(RegistryEventKind::Device);
                }
                _ => {}
            }
        }
        Some(DEVICE_INTERFACE) => {
            events.insert(RegistryEventKind::Device);
        }
        Some(PORT_INTERFACE) if added_or_removed => {
            let props = object_properties(value);
            if registry_property_string(&props, "port.physical")
                .is_some_and(|value| value == "true")
            {
                events.insert(RegistryEventKind::Device);
            }
        }
        Some(METADATA_INTERFACE) => {
            let props = object_properties(value);
            if matches!(
                registry_property_string(&props, "metadata.name").as_deref(),
                Some("default") | Some("default-profile")
            ) {
                events.insert(RegistryEventKind::Device);
            }
        }
        _ => {}
    }
}

fn observe_direct_error(
    old: &Option<serde_json::Value>,
    value: &serde_json::Value,
    status: &mut PipeWireRegistryStatus,
) {
    let error = value
        .pointer("/info/error")
        .and_then(serde_json::Value::as_str)
        .filter(|error| !error.trim().is_empty());
    let state_error = value
        .pointer("/info/state")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|state| state.eq_ignore_ascii_case("error"));
    if error.is_none() && !state_error {
        return;
    }
    let already_failed = old.as_ref().is_some_and(|old| {
        old.pointer("/info/error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|error| !error.trim().is_empty())
            || old
                .pointer("/info/state")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|state| state.eq_ignore_ascii_case("error"))
    });
    if already_failed {
        return;
    }
    match object_type(value) {
        Some(LINK_INTERFACE) => {
            status.direct_link_failures = status.direct_link_failures.saturating_add(1)
        }
        Some(NODE_INTERFACE) => {
            status.direct_node_errors = status.direct_node_errors.saturating_add(1)
        }
        _ => {}
    }
}

fn refresh_counts(state: &mut RegistryState) {
    let mut nodes = 0;
    let mut devices = 0;
    let mut ports = 0;
    let mut links = 0;
    let mut metadata = 0;
    let mut playback = 0;
    let mut capture = 0;
    for object in state.objects.values() {
        match object_type(object) {
            Some(NODE_INTERFACE) => {
                nodes += 1;
                let props = object_properties(object);
                match registry_property_string(&props, "media.class").as_deref() {
                    Some("Stream/Output/Audio") if !registry_node_is_owned(&props) => playback += 1,
                    Some("Stream/Input/Audio") if !registry_node_is_owned(&props) => capture += 1,
                    _ => {}
                }
            }
            Some(DEVICE_INTERFACE) => devices += 1,
            Some(PORT_INTERFACE) => ports += 1,
            Some(LINK_INTERFACE) => links += 1,
            Some(METADATA_INTERFACE) => metadata += 1,
            _ => {}
        }
    }
    state.status.object_count = state.objects.len();
    state.status.node_count = nodes;
    state.status.device_count = devices;
    state.status.port_count = ports;
    state.status.link_count = links;
    state.status.metadata_count = metadata;
    state.status.playback_stream_count = playback;
    state.status.capture_stream_count = capture;
}

fn registry_node_is_owned(properties: &BTreeMap<String, serde_json::Value>) -> bool {
    registry_graph_property_string(properties, "managed").as_deref() == Some("1")
        || ["node.name", "media.name", "application.name"]
            .into_iter()
            .filter_map(|key| registry_property_string(properties, key))
            .any(|value| looks_like_wavelinux_family_node(&value))
}

fn audio_state_from_registry(
    objects: &BTreeMap<u32, serde_json::Value>,
    config: Option<&MixerConfig>,
    effect_availability: Vec<EffectAvailability>,
) -> AudioStateSnapshot {
    let (default_source, default_sink) = registry_defaults(objects);
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for object in objects
        .values()
        .filter(|object| object_type(object) == Some(NODE_INTERFACE))
    {
        let props = object_properties(object);
        let media_class = registry_property_string(&props, "media.class");
        if !matches!(
            media_class.as_deref(),
            Some("Audio/Source") | Some("Audio/Sink")
        ) {
            continue;
        }
        let device = registry_device_info(
            object,
            &props,
            objects,
            if media_class.as_deref() == Some("Audio/Source") {
                default_source.as_deref()
            } else {
                default_sink.as_deref()
            },
        );
        if media_class.as_deref() == Some("Audio/Source") {
            inputs.push(device);
        } else {
            outputs.push(device);
        }
    }
    inputs.sort_by(|left, right| left.id.cmp(&right.id));
    outputs.sort_by(|left, right| left.id.cmp(&right.id));

    let node_names = registry_node_names(objects);
    let clients = registry_client_properties(objects);
    let sink_input_routes = registry_sink_input_routes(objects, &node_names);
    let source_output_routes = registry_source_output_routes(objects, &node_names);
    let mut app_streams =
        registry_app_streams(objects, config, &clients, &sink_input_routes, &outputs);
    app_streams.sort_by(|left, right| left.id.cmp(&right.id));
    let active_playback_sink = app_streams.iter().find_map(|stream| {
        if stream.muted {
            return None;
        }
        sink_input_routes
            .iter()
            .find(|route| route.id == stream.id)
            .and_then(|route| route.sink_name.clone())
            .filter(|name| !looks_like_wavelinux_family_node(name))
    });
    let sink_levels = outputs
        .iter()
        .filter_map(|sink| {
            let object = registry_node_by_name(objects, &sink.name)?;
            let (volume, muted) = registry_node_level(object);
            Some((
                sink.name.clone(),
                SinkLevelState {
                    volume_percent: Some((volume.clamp(0.0, 1.5) * 100.0).round() as u8),
                    muted,
                },
            ))
        })
        .collect();

    AudioStateSnapshot {
        graph: RuntimeGraph {
            inputs,
            outputs,
            app_streams,
            meters: Vec::new(),
            auto_devices: Vec::new(),
            effect_availability,
        },
        routes: RouteSnapshot {
            managed_modules: Vec::new(),
            sink_input_routes,
            source_output_routes,
        },
        sink_levels,
        active_playback_sink,
        bluetooth_cards: registry_bluetooth_cards(objects),
        default_source,
        default_sink,
    }
}

fn registry_device_info(
    object: &serde_json::Value,
    props: &BTreeMap<String, serde_json::Value>,
    objects: &BTreeMap<u32, serde_json::Value>,
    default_name: Option<&str>,
) -> DeviceInfo {
    let object_id = object_id(object).unwrap_or_default();
    let name =
        registry_property_string(props, "node.name").unwrap_or_else(|| object_id.to_string());
    let description = registry_property_string(props, "node.description")
        .or_else(|| registry_property_string(props, "device.description"))
        .or_else(|| registry_property_string(props, "node.nick"))
        .unwrap_or_else(|| name.clone());
    let is_virtual = looks_like_wavelinux_family_node(&name)
        || looks_like_wavelinux_family_node(&description)
        || registry_graph_property_string(props, "managed").as_deref() == Some("1");
    let route = registry_active_route(props, objects);
    let ports = route
        .as_ref()
        .map(|route| {
            vec![DevicePortInfo {
                name: route_string(route, "name").unwrap_or_default(),
                description: route_string(route, "description").unwrap_or_default(),
                availability: route_string(route, "available")
                    .map(|value| normalized_availability(&value))
                    .unwrap_or_else(|| "availability unknown".into()),
                direction: route_string(route, "direction"),
                port_type: route
                    .get("info")
                    .and_then(|info| spa_info_value(info, "port.type")),
            }]
        })
        .unwrap_or_default();
    let is_available = ports
        .iter()
        .all(|port| !availability_is_unavailable(&port.availability));
    DeviceInfo {
        id: name.clone(),
        index: Some(object_id.to_string()),
        name: name.clone(),
        description,
        is_available,
        active_port: ports.first().map(|port| port.name.clone()),
        ports,
        is_default: default_name.is_some_and(|default| audio_names_match(default, &name)),
        is_virtual,
        bus: detect_device_bus(&name, props, is_virtual),
        vendor_id: registry_property_string(props, "device.vendor.id")
            .or_else(|| registry_property_string(props, "api.usb.vendor.id"))
            .map(|value| normalize_hex_id(&value)),
        product_id: registry_property_string(props, "device.product.id")
            .or_else(|| registry_property_string(props, "api.usb.product.id"))
            .map(|value| normalize_hex_id(&value)),
        alsa_card: registry_property_string(props, "alsa.card")
            .or_else(|| registry_property_string(props, "api.alsa.card")),
        alsa_device: registry_property_string(props, "alsa.device")
            .or_else(|| registry_property_string(props, "api.alsa.pcm.device")),
        driver: registry_property_string(props, "alsa.driver_name")
            .or_else(|| registry_property_string(props, "device.driver")),
        bluetooth_modalias: registry_property_string(props, "api.bluez5.modalias")
            .or_else(|| registry_property_string(props, "bluez5.modalias")),
        active_profile: registry_property_string(props, "device.profile.name")
            .or_else(|| registry_property_string(props, "api.bluez5.profile")),
        active_codec: registry_property_string(props, "api.bluez5.codec")
            .or_else(|| registry_property_string(props, "bluez5.codec")),
        pipewire_properties: props
            .iter()
            .filter_map(|(key, value)| json_scalar_string(value).map(|value| (key.clone(), value)))
            .collect(),
        matched_profile_id: None,
        matched_profile_source: None,
        profile_confidence: None,
        active_latency_policy: None,
        active_routing_policy: None,
        active_bluetooth_mic_policy: None,
    }
}

fn registry_active_route(
    node_props: &BTreeMap<String, serde_json::Value>,
    objects: &BTreeMap<u32, serde_json::Value>,
) -> Option<serde_json::Value> {
    let device_id = registry_property_string(node_props, "device.id")?
        .parse::<u32>()
        .ok()?;
    let profile_device = registry_property_string(node_props, "card.profile.device")?;
    let device = objects.get(&device_id)?;
    device
        .pointer("/info/params/EnumRoute")?
        .as_array()?
        .iter()
        .find(|route| route_contains_device(route, &profile_device))
        .cloned()
}

fn route_contains_device(route: &serde_json::Value, device: &str) -> bool {
    route
        .get("devices")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|devices| {
            devices
                .iter()
                .any(|value| json_scalar_string(value).as_deref() == Some(device))
        })
        || route.get("device").and_then(json_scalar_string).as_deref() == Some(device)
}

fn route_string(route: &serde_json::Value, key: &str) -> Option<String> {
    route.get(key).and_then(json_scalar_string)
}

fn normalized_availability(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "no" | "unavailable" | "not available" => "not available".into(),
        "yes" | "available" => "available".into(),
        _ => "availability unknown".into(),
    }
}

fn spa_info_value(info: &serde_json::Value, key: &str) -> Option<String> {
    let values = info.as_array()?;
    let start = usize::from(values.first().is_some_and(serde_json::Value::is_number));
    values[start..]
        .chunks(2)
        .find(|pair| pair.first().and_then(serde_json::Value::as_str) == Some(key))
        .and_then(|pair| pair.get(1))
        .and_then(json_scalar_string)
}

fn registry_defaults(
    objects: &BTreeMap<u32, serde_json::Value>,
) -> (Option<String>, Option<String>) {
    let metadata = objects.values().find(|object| {
        object_type(object) == Some(METADATA_INTERFACE)
            && registry_property_string(&object_properties(object), "metadata.name").as_deref()
                == Some("default")
    });
    let value_for = |keys: &[&str]| {
        let entries = metadata?.get("metadata")?.as_array()?;
        keys.iter().find_map(|key| {
            let value = entries
                .iter()
                .find(|entry| entry.get("key").and_then(serde_json::Value::as_str) == Some(key))?
                .get("value")?;
            value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| value.as_str().map(ToOwned::to_owned))
        })
    };
    (
        value_for(&["default.configured.audio.source", "default.audio.source"]),
        value_for(&["default.configured.audio.sink", "default.audio.sink"]),
    )
}

fn registry_node_names(objects: &BTreeMap<u32, serde_json::Value>) -> BTreeMap<u32, String> {
    objects
        .iter()
        .filter(|(_, object)| object_type(object) == Some(NODE_INTERFACE))
        .filter_map(|(id, object)| {
            registry_property_string(&object_properties(object), "node.name")
                .map(|name| (*id, name))
        })
        .collect()
}

fn registry_client_properties(
    objects: &BTreeMap<u32, serde_json::Value>,
) -> BTreeMap<String, BTreeMap<String, serde_json::Value>> {
    objects
        .iter()
        .filter(|(_, object)| object_type(object) == Some("PipeWire:Interface:Client"))
        .map(|(id, object)| (id.to_string(), object_properties(object)))
        .collect()
}

fn registry_links(objects: &BTreeMap<u32, serde_json::Value>) -> Vec<(u32, u32)> {
    objects
        .values()
        .filter(|object| object_type(object) == Some(LINK_INTERFACE))
        .filter_map(|object| {
            let props = object_properties(object);
            Some((
                registry_property_string(&props, "link.output.node")?
                    .parse()
                    .ok()?,
                registry_property_string(&props, "link.input.node")?
                    .parse()
                    .ok()?,
            ))
        })
        .collect()
}

fn registry_sink_input_routes(
    objects: &BTreeMap<u32, serde_json::Value>,
    node_names: &BTreeMap<u32, String>,
) -> Vec<SinkInputRoute> {
    let links = registry_links(objects);
    objects
        .values()
        .filter(|object| object_type(object) == Some(NODE_INTERFACE))
        .filter_map(|object| {
            let props = object_properties(object);
            (registry_property_string(&props, "media.class").as_deref()
                == Some("Stream/Output/Audio"))
            .then_some(())?;
            let id = object_id(object)?;
            let sink_id = links
                .iter()
                .find_map(|(output, input)| (*output == id).then_some(*input));
            let (volume, muted) = registry_node_level(object);
            Some(SinkInputRoute {
                id: registry_stream_id(object, &props),
                module_id: None,
                role: registry_graph_property_string(&props, "role"),
                channel_id: registry_graph_property_string(&props, "channel_id"),
                mix_id: registry_graph_property_string(&props, "mix_id"),
                muted: Some(muted),
                volume_percent: Some((volume.clamp(0.0, 1.5) * 100.0).round() as u8),
                sink: sink_id.map(|id| id.to_string()),
                sink_name: sink_id.and_then(|id| node_names.get(&id).cloned()),
                target_object: registry_property_string(&props, "target.object")
                    .or_else(|| registry_graph_property_string(&props, "target_node")),
            })
        })
        .collect()
}

fn registry_source_output_routes(
    objects: &BTreeMap<u32, serde_json::Value>,
    node_names: &BTreeMap<u32, String>,
) -> Vec<SourceOutputRoute> {
    let links = registry_links(objects);
    objects
        .values()
        .filter(|object| object_type(object) == Some(NODE_INTERFACE))
        .filter_map(|object| {
            let props = object_properties(object);
            (registry_property_string(&props, "media.class").as_deref()
                == Some("Stream/Input/Audio"))
            .then_some(())?;
            let id = object_id(object)?;
            let source_id = links
                .iter()
                .find_map(|(output, input)| (*input == id).then_some(*output));
            let (volume, muted) = registry_node_level(object);
            Some(SourceOutputRoute {
                id: registry_stream_id(object, &props),
                module_id: None,
                role: registry_graph_property_string(&props, "role"),
                channel_id: registry_graph_property_string(&props, "channel_id"),
                mix_id: registry_graph_property_string(&props, "mix_id"),
                muted: Some(muted),
                volume_percent: Some((volume.clamp(0.0, 1.5) * 100.0).round() as u8),
                source_id: source_id.map(|id| id.to_string()),
                source_name: source_id.and_then(|id| node_names.get(&id).cloned()),
                target_object: registry_property_string(&props, "target.object")
                    .or_else(|| registry_graph_property_string(&props, "target_node")),
                application_name: registry_property_string(&props, "application.name"),
                node_name: registry_property_string(&props, "node.name"),
                media_name: registry_property_string(&props, "media.name"),
                managed: registry_graph_property_string(&props, "managed"),
                dont_move: registry_property_string(&props, "node.dont-move")
                    .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on")),
            })
        })
        .collect()
}

fn registry_app_streams(
    objects: &BTreeMap<u32, serde_json::Value>,
    config: Option<&MixerConfig>,
    clients: &BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    routes: &[SinkInputRoute],
    outputs: &[DeviceInfo],
) -> Vec<AppStream> {
    objects
        .values()
        .filter(|object| object_type(object) == Some(NODE_INTERFACE))
        .filter_map(|object| {
            let node_props = object_properties(object);
            if registry_property_string(&node_props, "media.class").as_deref()
                != Some("Stream/Output/Audio")
                || registry_node_is_owned(&node_props)
            {
                return None;
            }
            let mut props = registry_property_string(&node_props, "client.id")
                .and_then(|id| clients.get(&id).cloned())
                .unwrap_or_default();
            props.extend(node_props.clone());
            let id = registry_stream_id(object, &props);
            let app_id = registry_property_string(&props, "application.id")
                .or_else(|| registry_property_string(&props, "application.process.binary"))
                .or_else(|| registry_property_string(&props, "module-stream-restore.id"));
            let binary = registry_property_string(&props, "application.process.binary");
            let window_class = registry_property_string(&props, "window.x11.class")
                .or_else(|| registry_property_string(&props, "window.class"))
                .or_else(|| registry_property_string(&props, "application.window.class"));
            let process_name = registry_property_string(&props, "application.process.name")
                .or_else(|| binary.clone())
                .or_else(|| registry_property_string(&props, "application.name"))
                .or_else(|| registry_property_string(&props, "node.name"))
                .or_else(|| registry_property_string(&props, "media.name"));
            let display_name = registry_property_string(&props, "application.name")
                .or_else(|| app_id.clone())
                .unwrap_or_else(|| format!("Stream {id}"));
            let route = routes.iter().find(|route| route.id == id);
            let routed_channel_id =
                registry_graph_property_string(&props, "channel_id").or_else(|| {
                    let sink_name = route?.sink_name.as_deref()?;
                    config?
                        .channels
                        .iter()
                        .find(|channel| audio_names_match(&channel.virtual_sink_name, sink_name))
                        .map(|channel| channel.id.clone())
                });
            let (volume, muted) = registry_node_level(object);
            let mut stream = AppStream {
                id,
                app_id,
                binary,
                process_name,
                window_class,
                display_name,
                media_name: registry_property_string(&props, "media.name"),
                routed_channel_id,
                volume,
                muted,
            };
            if let Some(config) = config {
                apply_configured_app_label(config, &mut stream);
            }
            let target_exists = route
                .and_then(|route| route.sink_name.as_deref())
                .is_none_or(|target| {
                    outputs
                        .iter()
                        .any(|output| audio_names_match(&output.name, target))
                });
            target_exists.then_some(stream)
        })
        .collect()
}

fn registry_stream_id(
    object: &serde_json::Value,
    props: &BTreeMap<String, serde_json::Value>,
) -> String {
    registry_property_string(props, "object.serial")
        .or_else(|| object_id(object).map(|id| id.to_string()))
        .unwrap_or_default()
}

fn registry_node_level(object: &serde_json::Value) -> (f32, bool) {
    let props = object
        .pointer("/info/params/Props")
        .and_then(serde_json::Value::as_array)
        .and_then(|values| values.first())
        .and_then(serde_json::Value::as_object);
    let muted = props
        .and_then(|props| props.get("mute"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let volume = props
        .and_then(|props| props.get("channelVolumes"))
        .and_then(serde_json::Value::as_array)
        .and_then(|values| values.first())
        .and_then(serde_json::Value::as_f64)
        .map(|value| value.cbrt() as f32)
        .unwrap_or(1.0);
    (volume, muted)
}

fn registry_node_by_name<'a>(
    objects: &'a BTreeMap<u32, serde_json::Value>,
    name: &str,
) -> Option<&'a serde_json::Value> {
    objects.values().find(|object| {
        registry_property_string(&object_properties(object), "node.name")
            .is_some_and(|candidate| audio_names_match(&candidate, name))
    })
}

fn registry_bluetooth_cards(objects: &BTreeMap<u32, serde_json::Value>) -> Vec<BluetoothAudioCard> {
    objects
        .values()
        .filter(|object| object_type(object) == Some(DEVICE_INTERFACE))
        .filter_map(|object| {
            let props = object_properties(object);
            let name = registry_property_string(&props, "device.name")?;
            let is_bluetooth = name.starts_with("bluez_card.")
                || registry_property_string(&props, "device.bus").as_deref() == Some("bluetooth")
                || registry_property_string(&props, "device.api")
                    .is_some_and(|api| api.starts_with("bluez"));
            if !is_bluetooth {
                return None;
            }
            let profiles = object
                .pointer("/info/params/EnumProfile")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|profile| {
                    let name = route_string(profile, "name")?;
                    let description = route_string(profile, "description").unwrap_or_default();
                    Some(BluetoothCardProfile {
                        sinks: profile_class_count(profile.get("classes"), "Audio/Sink"),
                        priority: profile
                            .get("priority")
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or_default() as i32,
                        available: route_string(profile, "available")
                            .is_none_or(|value| !availability_is_unavailable(&value)),
                        name,
                        description,
                    })
                })
                .collect::<Vec<_>>();
            let preferred_a2dp_profile = profiles
                .iter()
                .filter(|profile| {
                    profile.available && profile.sinks > 0 && is_a2dp_profile_name(&profile.name)
                })
                .max_by_key(|profile| {
                    (
                        a2dp_codec_rank(&profile.name, &profile.description),
                        profile.priority,
                    )
                })
                .map(|profile| profile.name.clone());
            let active_profile = object
                .pointer("/info/params/Profile/0/name")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| registry_property_string(&props, "device.profile.name"));
            let device_key = registry_property_string(&props, "api.bluez5.address")
                .or_else(|| registry_property_string(&props, "device.string"))
                .or_else(|| name.strip_prefix("bluez_card.").map(ToOwned::to_owned))
                .map(|value| normalize_bluetooth_device_key(&value))?;
            Some(BluetoothAudioCard {
                name,
                device_key,
                active_profile,
                preferred_a2dp_profile,
                profiles,
            })
        })
        .collect()
}

fn profile_class_count(classes: Option<&serde_json::Value>, media_class: &str) -> u32 {
    let Some(classes) = classes.and_then(serde_json::Value::as_array) else {
        return 0;
    };
    classes
        .iter()
        .filter_map(serde_json::Value::as_array)
        .find(|class| class.first().and_then(serde_json::Value::as_str) == Some(media_class))
        .and_then(|class| class.get(1))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default()
        .min(u32::MAX as u64) as u32
}

fn json_scalar_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn registry_property_string(
    properties: &BTreeMap<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    properties
        .get(key)
        .and_then(json_scalar_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn registry_graph_property_string(
    properties: &BTreeMap<String, serde_json::Value>,
    name: &str,
) -> Option<String> {
    registry_property_string(properties, &graph_prop(name))
}

fn audio_names_match(left: &str, right: &str) -> bool {
    left == right || left.trim_end_matches(".monitor") == right.trim_end_matches(".monitor")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_initialization_wait_survives_connection_thread_startup_race() {
        let cache = PipeWireRegistryCache::default();
        let publisher = cache.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            publisher.mark_connected(false);
            std::thread::sleep(Duration::from_millis(20));
            publisher.apply_batch(vec![serde_json::json!({
                "id": 10,
                "type": NODE_INTERFACE,
                "info": {"props": {"media.class":"Audio/Sink","node.name":"system-output"}}
            })]);
        });

        assert!(cache.wait_initialized(Duration::from_millis(250)));
        worker.join().expect("registry publisher");
    }

    #[test]
    fn registry_initialization_wait_ends_on_explicit_connection_failure() {
        let cache = PipeWireRegistryCache::default();
        let publisher = cache.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            publisher.mark_unavailable("PipeWire unavailable");
        });

        let started = std::time::Instant::now();
        assert!(!cache.wait_initialized(Duration::from_secs(1)));
        assert!(started.elapsed() < Duration::from_millis(500));
        worker.join().expect("registry publisher");
    }

    #[test]
    fn registry_snapshot_preserves_explicit_unavailable_jack() {
        let cache = PipeWireRegistryCache::default();
        cache.mark_connected(false);
        let batch = cache.apply_batch(vec![
            serde_json::json!({
                "id": 10,
                "type": DEVICE_INTERFACE,
                "info": {"props": {"device.name": "alsa_card.pci-test", "device.bus": "pci"}, "params": {
                    "EnumRoute": [{"direction":"Input","name":"[In] Headset","description":"Headset Mic","available":"no","devices":[4],"info":[2,"port.type","headset","card.profile.port","4"]}]
                }}
            }),
            serde_json::json!({
                "id": 20,
                "type": NODE_INTERFACE,
                "info": {"props": {
                    "media.class":"Audio/Source","node.name":"alsa_input.pci-test.headset","node.description":"Headset Mic","device.id":10,"card.profile.device":4,"device.bus":"pci","object.serial":120
                }}
            }),
            serde_json::json!({
                "id": 30,
                "type": METADATA_INTERFACE,
                "props": {"metadata.name":"default"},
                "metadata": [{"subject":0,"key":"default.audio.source","value":{"name":"alsa_input.pci-test.headset"}}]
            }),
        ]);
        assert!(batch.initial);
        let (snapshot, generation) = cache
            .audio_state_snapshot(None, Vec::new())
            .expect("registry snapshot");
        assert_eq!(generation, 1);
        assert_eq!(
            snapshot.default_source.as_deref(),
            Some("alsa_input.pci-test.headset")
        );
        assert_eq!(snapshot.graph.inputs.len(), 1);
        assert!(!snapshot.graph.inputs[0].is_available);
        assert_eq!(
            snapshot.graph.inputs[0].active_port.as_deref(),
            Some("[In] Headset")
        );
        assert_eq!(
            snapshot.graph.inputs[0].ports[0].availability,
            "not available"
        );
    }

    #[test]
    fn registry_generations_classify_stream_add_and_remove() {
        let cache = PipeWireRegistryCache::default();
        cache.mark_connected(false);
        let empty = cache.apply_batch(Vec::new());
        assert!(!empty.initial);
        assert!(!cache.status().initialized);
        let initial = cache.apply_batch(vec![serde_json::json!({
            "id": 10,
            "type": NODE_INTERFACE,
            "info": {"props": {"media.class":"Audio/Sink","node.name":"system-output","object.serial":10}}
        })]);
        assert!(initial.initial);
        assert!(cache.status().initialized);
        let added = cache.apply_batch(vec![serde_json::json!({
            "id": 42,
            "type": NODE_INTERFACE,
            "info": {"props": {"media.class":"Stream/Output/Audio","node.name":"browser","object.serial":99}}
        })]);
        assert_eq!(
            added.events,
            BTreeSet::from([RegistryEventKind::PlaybackStream])
        );
        let removed = cache.apply_batch(vec![serde_json::json!({"id":42,"info":null})]);
        assert_eq!(
            removed.events,
            BTreeSet::from([RegistryEventKind::PlaybackStream])
        );
        assert!(removed.generation > added.generation);
    }

    #[test]
    fn owned_node_additions_do_not_trigger_device_reconciliation() {
        let mut events = BTreeSet::new();
        classify_registry_object(
            &serde_json::json!({
                "id": 71,
                "type": NODE_INTERFACE,
                "info": {"props": {
                    "media.class":"Audio/Sink",
                    "node.name":format!("{}_channel_music", graph_prefix())
                }}
            }),
            true,
            &mut events,
        );
        assert!(events.is_empty());
    }

    #[test]
    fn registry_selects_metadata_moves_only_for_native_streams() {
        let cache = PipeWireRegistryCache::default();
        cache.mark_connected(false);
        cache.apply_batch(vec![
            serde_json::json!({
                "id": 7,
                "type": "PipeWire:Interface:Client",
                "info": {"props": {"application.name":"Native player"}}
            }),
            serde_json::json!({
                "id": 8,
                "type": "PipeWire:Interface:Client",
                "info": {"props": {"application.name":"Pulse player","client.api":"pipewire-pulse"}}
            }),
            serde_json::json!({
                "id": 20,
                "type": NODE_INTERFACE,
                "info": {"props": {"media.class":"Audio/Sink","node.name":"wavelinux_channel_music","object.serial":500}}
            }),
            serde_json::json!({
                "id": 21,
                "type": NODE_INTERFACE,
                "info": {"props": {"media.class":"Audio/Source","node.name":"wavelinux6-mic","object.serial":501}}
            }),
            serde_json::json!({
                "id": 30,
                "type": NODE_INTERFACE,
                "info": {"props": {"media.class":"Stream/Output/Audio","node.name":"native-player","client.id":7,"object.serial":600}}
            }),
            serde_json::json!({
                "id": 31,
                "type": NODE_INTERFACE,
                "info": {"props": {"media.class":"Stream/Output/Audio","node.name":"pulse-player","client.id":8,"object.serial":601}}
            }),
            serde_json::json!({
                "id": 32,
                "type": NODE_INTERFACE,
                "info": {"props": {"media.class":"Stream/Input/Audio","node.name":"native-recorder","client.id":7,"object.serial":602}}
            }),
        ]);

        assert_eq!(
            cache.playback_route_backend("600", "wavelinux_channel_music"),
            Some(StreamRouteBackend::Native(NativeStreamRoute {
                stream_node_id: 30,
                target_object_serial: "500".into(),
                target_node_name: "wavelinux_channel_music".into(),
            }))
        );
        assert_eq!(
            cache.playback_route_backend("601", "wavelinux_channel_music"),
            Some(StreamRouteBackend::PulseCompatibility)
        );
        assert_eq!(
            cache.capture_route_backend("602", "wavelinux6-mic"),
            Some(StreamRouteBackend::Native(NativeStreamRoute {
                stream_node_id: 32,
                target_object_serial: "501".into(),
                target_node_name: "wavelinux6-mic".into(),
            }))
        );
    }
}
