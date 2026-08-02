use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use pipewire as pw;
use pw::proxy::{Listener, ProxyT};
use pw::spa;
use serde_json::{Map, Value as JsonValue};
use wavelinux_pw::{PipeWireRegistryCache, RegistryBatch};

pub(super) struct NativeRegistryHooks {
    pub cache: PipeWireRegistryCache,
    pub stopping: Arc<dyn Fn() -> bool + Send + Sync>,
    pub on_batch: Arc<dyn Fn(RegistryBatch) + Send + Sync>,
}

struct BoundProxy {
    _proxy: Box<dyn ProxyT>,
    _listener: Box<dyn Listener>,
}

#[derive(Debug, Clone)]
struct MetadataEntry {
    subject: u32,
    key: String,
    type_name: Option<String>,
    value: JsonValue,
}

struct NativeRegistryState {
    cache: PipeWireRegistryCache,
    on_batch: Arc<dyn Fn(RegistryBatch) + Send + Sync>,
    bootstrap: BTreeMap<u32, JsonValue>,
    params: BTreeMap<(u32, String), BTreeMap<u32, JsonValue>>,
    metadata: BTreeMap<u32, BTreeMap<(u32, String), MetadataEntry>>,
    initialized: bool,
}

impl NativeRegistryState {
    fn new(hooks: &NativeRegistryHooks) -> Self {
        Self {
            cache: hooks.cache.clone(),
            on_batch: Arc::clone(&hooks.on_batch),
            bootstrap: BTreeMap::new(),
            params: BTreeMap::new(),
            metadata: BTreeMap::new(),
            initialized: false,
        }
    }

    fn update(&mut self, update: JsonValue) {
        let Some(id) = update
            .get("id")
            .and_then(JsonValue::as_u64)
            .and_then(|id| u32::try_from(id).ok())
        else {
            return;
        };
        if !self.initialized {
            let object = self
                .bootstrap
                .entry(id)
                .or_insert_with(|| serde_json::json!({}));
            merge_json(object, update);
            return;
        }
        self.dispatch(vec![update]);
    }

    fn remove(&mut self, id: u32) {
        self.params.retain(|(object_id, _), _| *object_id != id);
        self.metadata.remove(&id);
        if !self.initialized {
            self.bootstrap.remove(&id);
            return;
        }
        self.dispatch(vec![serde_json::json!({"id": id, "info": null})]);
    }

    fn update_param(
        &mut self,
        object_id: u32,
        param_type: spa::param::ParamType,
        index: u32,
        param: Option<&spa::pod::Pod>,
    ) {
        let Some(name) = registry_param_name(param_type) else {
            return;
        };
        let key = (object_id, name.to_string());
        let values = self.params.entry(key).or_default();
        match param.and_then(decode_param_pod) {
            Some(value) => {
                values.insert(index, value);
            }
            None if index == 0 => values.clear(),
            None => {
                values.remove(&index);
            }
        }
        let values = values.values().cloned().collect::<Vec<_>>();
        self.update(serde_json::json!({
            "id": object_id,
            "info": {"params": {name: values}}
        }));
    }

    fn update_metadata(
        &mut self,
        object_id: u32,
        subject: u32,
        key: Option<&str>,
        type_name: Option<&str>,
        value: Option<&str>,
    ) {
        let entries = self.metadata.entry(object_id).or_default();
        match (key, value) {
            (Some(key), Some(value)) => {
                entries.insert(
                    (subject, key.to_string()),
                    MetadataEntry {
                        subject,
                        key: key.to_string(),
                        type_name: type_name.map(ToOwned::to_owned),
                        value: serde_json::from_str(value)
                            .unwrap_or_else(|_| JsonValue::String(value.to_string())),
                    },
                );
            }
            (Some(key), None) => {
                entries.remove(&(subject, key.to_string()));
            }
            (None, _) => entries.retain(|(entry_subject, _), _| *entry_subject != subject),
        }
        let metadata = entries
            .values()
            .map(|entry| {
                serde_json::json!({
                    "subject": entry.subject,
                    "key": entry.key,
                    "type": entry.type_name,
                    "value": entry.value,
                })
            })
            .collect::<Vec<_>>();
        self.update(serde_json::json!({"id": object_id, "metadata": metadata}));
    }

    fn finish_bootstrap(&mut self) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        let objects = self.bootstrap.values().cloned().collect::<Vec<_>>();
        self.dispatch(objects);
    }

    fn dispatch(&self, objects: Vec<JsonValue>) {
        let batch = self.cache.apply_batch(objects);
        (self.on_batch)(batch);
    }
}

/// Run one native PipeWire registry connection until shutdown or a core error.
///
/// All proxy callbacks stay on this thread's PipeWire main loop. The actor
/// publishes an atomic bootstrap only after two core sync barriers, so the
/// reconciler never observes a half-populated graph.
pub(super) fn run_native_registry_connection(hooks: NativeRegistryHooks) -> Result<(), String> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|error| format!("PipeWire registry mainloop creation failed: {error}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|error| format!("PipeWire registry context creation failed: {error}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|error| format!("PipeWire registry connection failed: {error}"))?;
    let registry = core
        .get_registry_rc()
        .map_err(|error| format!("PipeWire registry acquisition failed: {error}"))?;

    let state = Rc::new(RefCell::new(NativeRegistryState::new(&hooks)));
    let proxies = Rc::new(RefCell::new(BTreeMap::<u32, BoundProxy>::new()));
    let registry_weak = registry.downgrade();
    let state_for_global = Rc::clone(&state);
    let proxies_for_global = Rc::clone(&proxies);
    let state_for_remove = Rc::clone(&state);
    let proxies_for_remove = Rc::clone(&proxies);
    let _registry_listener = registry
        .add_listener_local()
        .global(move |object| {
            state_for_global
                .borrow_mut()
                .update(global_object_json(object));
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            let id = object.id;
            let bound = match object.type_ {
                pw::types::ObjectType::Node => {
                    bind_node(&registry, object, Rc::clone(&state_for_global))
                }
                pw::types::ObjectType::Device => {
                    bind_device(&registry, object, Rc::clone(&state_for_global))
                }
                pw::types::ObjectType::Port => {
                    bind_port(&registry, object, Rc::clone(&state_for_global))
                }
                pw::types::ObjectType::Link => {
                    bind_link(&registry, object, Rc::clone(&state_for_global))
                }
                pw::types::ObjectType::Metadata => {
                    bind_metadata(&registry, object, Rc::clone(&state_for_global))
                }
                pw::types::ObjectType::Client => {
                    bind_client(&registry, object, Rc::clone(&state_for_global))
                }
                _ => None,
            };
            if let Some(bound) = bound {
                proxies_for_global.borrow_mut().insert(id, bound);
            }
        })
        .global_remove(move |id| {
            state_for_remove.borrow_mut().remove(id);
            proxies_for_remove.borrow_mut().remove(&id);
        })
        .register();

    let pending = Rc::new(Cell::new(Some((
        core.sync(0)
            .map_err(|error| format!("PipeWire registry bootstrap sync failed: {error}"))?,
        0_u8,
    ))));
    let state_for_sync = Rc::clone(&state);
    let pending_for_sync = Rc::clone(&pending);
    let core_for_sync = core.clone();
    let mainloop_for_error = mainloop.clone();
    let core_error = Rc::new(RefCell::new(None::<String>));
    let core_error_for_listener = Rc::clone(&core_error);
    let _core_listener = core
        .add_listener_local()
        .done(move |id, sequence| {
            let Some((expected, phase)) = pending_for_sync.get() else {
                return;
            };
            if id != pw::core::PW_ID_CORE || sequence != expected {
                return;
            }
            if phase == 0 {
                match core_for_sync.sync(sequence.seq()) {
                    Ok(next) => pending_for_sync.set(Some((next, 1))),
                    Err(error) => {
                        pending_for_sync.set(None);
                        *core_error_for_listener.borrow_mut() =
                            Some(format!("PipeWire registry final sync failed: {error}"));
                        mainloop_for_error.quit();
                    }
                }
            } else {
                pending_for_sync.set(None);
                state_for_sync.borrow_mut().finish_bootstrap();
            }
        })
        .error({
            let mainloop = mainloop.clone();
            let core_error = Rc::clone(&core_error);
            move |id, sequence, result, message| {
                if transient_registry_object_error(result, message) {
                    return;
                }
                if id == pw::core::PW_ID_CORE {
                    *core_error.borrow_mut() = Some(format!(
                        "PipeWire registry core error id={id} seq={sequence} result={result}: {message}"
                    ));
                    mainloop.quit();
                }
            }
        })
        .register();

    let mainloop_for_stop = mainloop.clone();
    let stopping = Arc::clone(&hooks.stopping);
    let stop_timer = mainloop.loop_().add_timer(move |_| {
        if stopping() {
            mainloop_for_stop.quit();
        }
    });
    stop_timer
        .update_timer(
            Some(Duration::from_millis(100)),
            Some(Duration::from_millis(100)),
        )
        .into_result()
        .map_err(|error| format!("PipeWire registry stop timer failed: {error}"))?;

    mainloop.run();
    if let Some(error) = core_error.borrow_mut().take() {
        return Err(error);
    }
    Ok(())
}

fn bind_node<P>(
    registry: &pw::registry::Registry,
    object: &pw::registry::GlobalObject<P>,
    state: Rc<RefCell<NativeRegistryState>>,
) -> Option<BoundProxy>
where
    P: AsRef<spa::utils::dict::DictRef>,
{
    let node: pw::node::Node = registry.bind(object).ok()?;
    let id = object.id;
    let state_for_info = Rc::clone(&state);
    let listener = node
        .add_listener_local()
        .info(move |info| {
            let (state_name, error) = node_state_json(info.state());
            state_for_info.borrow_mut().update(serde_json::json!({
                "id": id,
                "info": {
                    "state": state_name,
                    "error": error,
                    "props": dict_json(info.props()),
                }
            }));
        })
        .param(move |_sequence, param_type, index, _next, param| {
            state
                .borrow_mut()
                .update_param(id, param_type, index, param);
        })
        .register();
    node.subscribe_params(&[spa::param::ParamType::Props]);
    node.enum_params(0, Some(spa::param::ParamType::Props), 0, u32::MAX);
    Some(BoundProxy {
        _proxy: Box::new(node),
        _listener: Box::new(listener),
    })
}

fn bind_device<P>(
    registry: &pw::registry::Registry,
    object: &pw::registry::GlobalObject<P>,
    state: Rc<RefCell<NativeRegistryState>>,
) -> Option<BoundProxy>
where
    P: AsRef<spa::utils::dict::DictRef>,
{
    let device: pw::device::Device = registry.bind(object).ok()?;
    let id = object.id;
    let state_for_info = Rc::clone(&state);
    let listener = device
        .add_listener_local()
        .info(move |info| {
            state_for_info.borrow_mut().update(serde_json::json!({
                "id": id,
                "info": {"props": dict_json(info.props())}
            }));
        })
        .param(move |_sequence, param_type, index, _next, param| {
            state
                .borrow_mut()
                .update_param(id, param_type, index, param);
        })
        .register();
    let params = [
        spa::param::ParamType::EnumProfile,
        spa::param::ParamType::Profile,
        spa::param::ParamType::EnumRoute,
    ];
    device.subscribe_params(&params);
    for param in params {
        device.enum_params(0, Some(param), 0, u32::MAX);
    }
    Some(BoundProxy {
        _proxy: Box::new(device),
        _listener: Box::new(listener),
    })
}

fn bind_port<P>(
    registry: &pw::registry::Registry,
    object: &pw::registry::GlobalObject<P>,
    state: Rc<RefCell<NativeRegistryState>>,
) -> Option<BoundProxy>
where
    P: AsRef<spa::utils::dict::DictRef>,
{
    let port: pw::port::Port = registry.bind(object).ok()?;
    let id = object.id;
    let listener = port
        .add_listener_local()
        .info(move |info| {
            let direction = if info.direction() == spa::utils::Direction::Input {
                "input"
            } else if info.direction() == spa::utils::Direction::Output {
                "output"
            } else {
                "unknown"
            };
            state.borrow_mut().update(serde_json::json!({
                "id": id,
                "info": {
                    "direction": direction,
                    "props": dict_json(info.props()),
                }
            }));
        })
        .register();
    Some(BoundProxy {
        _proxy: Box::new(port),
        _listener: Box::new(listener),
    })
}

fn bind_link<P>(
    registry: &pw::registry::Registry,
    object: &pw::registry::GlobalObject<P>,
    state: Rc<RefCell<NativeRegistryState>>,
) -> Option<BoundProxy>
where
    P: AsRef<spa::utils::dict::DictRef>,
{
    let link: pw::link::Link = registry.bind(object).ok()?;
    let id = object.id;
    let listener = link
        .add_listener_local()
        .info(move |info| {
            let (state_name, error) = link_state_json(info.state());
            state.borrow_mut().update(serde_json::json!({
                "id": id,
                "info": {
                    "output-node-id": info.output_node_id(),
                    "output-port-id": info.output_port_id(),
                    "input-node-id": info.input_node_id(),
                    "input-port-id": info.input_port_id(),
                    "state": state_name,
                    "error": error,
                    "props": dict_json(info.props()),
                }
            }));
        })
        .register();
    Some(BoundProxy {
        _proxy: Box::new(link),
        _listener: Box::new(listener),
    })
}

fn bind_metadata<P>(
    registry: &pw::registry::Registry,
    object: &pw::registry::GlobalObject<P>,
    state: Rc<RefCell<NativeRegistryState>>,
) -> Option<BoundProxy>
where
    P: AsRef<spa::utils::dict::DictRef>,
{
    let metadata: pw::metadata::Metadata = registry.bind(object).ok()?;
    let id = object.id;
    let listener = metadata
        .add_listener_local()
        .property(move |subject, key, type_name, value| {
            state
                .borrow_mut()
                .update_metadata(id, subject, key, type_name, value);
            0
        })
        .register();
    Some(BoundProxy {
        _proxy: Box::new(metadata),
        _listener: Box::new(listener),
    })
}

fn bind_client<P>(
    registry: &pw::registry::Registry,
    object: &pw::registry::GlobalObject<P>,
    state: Rc<RefCell<NativeRegistryState>>,
) -> Option<BoundProxy>
where
    P: AsRef<spa::utils::dict::DictRef>,
{
    let client: pw::client::Client = registry.bind(object).ok()?;
    let id = object.id;
    let listener = client
        .add_listener_local()
        .info(move |info| {
            state.borrow_mut().update(serde_json::json!({
                "id": id,
                "info": {"props": dict_json(info.props())}
            }));
        })
        .register();
    Some(BoundProxy {
        _proxy: Box::new(client),
        _listener: Box::new(listener),
    })
}

fn global_object_json<P>(object: &pw::registry::GlobalObject<P>) -> JsonValue
where
    P: AsRef<spa::utils::dict::DictRef>,
{
    serde_json::json!({
        "id": object.id,
        "type": object.type_.to_string(),
        "version": object.version,
        "props": dict_json(object.props.as_ref().map(AsRef::as_ref)),
    })
}

fn dict_json(dict: Option<&spa::utils::dict::DictRef>) -> JsonValue {
    JsonValue::Object(
        dict.into_iter()
            .flat_map(spa::utils::dict::DictRef::iter)
            .map(|(key, value)| (key.to_string(), JsonValue::String(value.to_string())))
            .collect(),
    )
}

fn node_state_json(state: pw::node::NodeState<'_>) -> (&'static str, Option<String>) {
    match state {
        pw::node::NodeState::Error(error) => ("error", Some(error.to_string())),
        pw::node::NodeState::Creating => ("creating", None),
        pw::node::NodeState::Suspended => ("suspended", None),
        pw::node::NodeState::Idle => ("idle", None),
        pw::node::NodeState::Running => ("running", None),
    }
}

fn link_state_json(state: pw::link::LinkState<'_>) -> (&'static str, Option<String>) {
    match state {
        pw::link::LinkState::Error(error) => ("error", Some(error.to_string())),
        pw::link::LinkState::Unlinked => ("unlinked", None),
        pw::link::LinkState::Init => ("init", None),
        pw::link::LinkState::Negotiating => ("negotiating", None),
        pw::link::LinkState::Allocating => ("allocating", None),
        pw::link::LinkState::Paused => ("paused", None),
        pw::link::LinkState::Active => ("active", None),
    }
}

fn registry_param_name(param_type: spa::param::ParamType) -> Option<&'static str> {
    if param_type == spa::param::ParamType::Props {
        Some("Props")
    } else if param_type == spa::param::ParamType::EnumRoute {
        Some("EnumRoute")
    } else if param_type == spa::param::ParamType::EnumProfile {
        Some("EnumProfile")
    } else if param_type == spa::param::ParamType::Profile {
        Some("Profile")
    } else {
        None
    }
}

fn decode_param_pod(pod: &spa::pod::Pod) -> Option<JsonValue> {
    let (_, value) =
        spa::pod::deserialize::PodDeserializer::deserialize_from::<spa::pod::Value>(pod.as_bytes())
            .ok()?;
    Some(spa_value_json(&value))
}

fn spa_value_json(value: &spa::pod::Value) -> JsonValue {
    use spa::pod::{ChoiceValue, Value, ValueArray};
    match value {
        Value::None => JsonValue::Null,
        Value::Bool(value) => JsonValue::Bool(*value),
        Value::Id(value) => JsonValue::from(value.0),
        Value::Int(value) => JsonValue::from(*value),
        Value::Long(value) => JsonValue::from(*value),
        Value::Float(value) => JsonValue::from(*value as f64),
        Value::Double(value) => JsonValue::from(*value),
        Value::String(value) => JsonValue::String(value.clone()),
        Value::Bytes(values) => {
            JsonValue::Array(values.iter().copied().map(JsonValue::from).collect())
        }
        Value::Rectangle(value) => {
            serde_json::json!({"width": value.width, "height": value.height})
        }
        Value::Fraction(value) => serde_json::json!({"num": value.num, "denom": value.denom}),
        Value::Fd(value) => JsonValue::from(value.0),
        Value::ValueArray(values) => match values {
            ValueArray::None(values) => vec![JsonValue::Null; values.len()],
            ValueArray::Bool(values) => values.iter().copied().map(JsonValue::from).collect(),
            ValueArray::Id(values) => values
                .iter()
                .map(|value| JsonValue::from(value.0))
                .collect(),
            ValueArray::Int(values) => values.iter().copied().map(JsonValue::from).collect(),
            ValueArray::Long(values) => values.iter().copied().map(JsonValue::from).collect(),
            ValueArray::Float(values) => values
                .iter()
                .map(|value| JsonValue::from(*value as f64))
                .collect(),
            ValueArray::Double(values) => values.iter().copied().map(JsonValue::from).collect(),
            ValueArray::Rectangle(values) => values
                .iter()
                .map(|value| serde_json::json!({"width": value.width, "height": value.height}))
                .collect(),
            ValueArray::Fraction(values) => values
                .iter()
                .map(|value| serde_json::json!({"num": value.num, "denom": value.denom}))
                .collect(),
            ValueArray::Fd(values) => values
                .iter()
                .map(|value| JsonValue::from(value.0))
                .collect(),
        }
        .into(),
        Value::Struct(values) => JsonValue::Array(values.iter().map(spa_value_json).collect()),
        Value::Object(object) => spa_object_json(object),
        Value::Choice(choice) => match choice {
            ChoiceValue::Bool(choice) => choice_default(&choice.1).map(JsonValue::from),
            ChoiceValue::Int(choice) => choice_default(&choice.1).map(JsonValue::from),
            ChoiceValue::Long(choice) => choice_default(&choice.1).map(JsonValue::from),
            ChoiceValue::Float(choice) => {
                choice_default(&choice.1).map(|value| JsonValue::from(value as f64))
            }
            ChoiceValue::Double(choice) => choice_default(&choice.1).map(JsonValue::from),
            ChoiceValue::Id(choice) => {
                choice_default(&choice.1).map(|value| JsonValue::from(value.0))
            }
            ChoiceValue::Rectangle(choice) => choice_default(&choice.1)
                .map(|value| serde_json::json!({"width": value.width, "height": value.height})),
            ChoiceValue::Fraction(choice) => choice_default(&choice.1)
                .map(|value| serde_json::json!({"num": value.num, "denom": value.denom})),
            ChoiceValue::Fd(choice) => {
                choice_default(&choice.1).map(|value| JsonValue::from(value.0))
            }
        }
        .unwrap_or(JsonValue::Null),
        Value::Pointer(_, _) => JsonValue::Null,
    }
}

fn choice_default<T: spa::pod::CanonicalFixedSizedPod + Clone>(
    choice: &spa::utils::ChoiceEnum<T>,
) -> Option<T> {
    use spa::utils::ChoiceEnum;
    Some(match choice {
        ChoiceEnum::None(value) => value.clone(),
        ChoiceEnum::Range { default, .. }
        | ChoiceEnum::Step { default, .. }
        | ChoiceEnum::Enum { default, .. }
        | ChoiceEnum::Flags { default, .. } => default.clone(),
    })
}

fn spa_object_json(object: &spa::pod::Object) -> JsonValue {
    let mut result = Map::new();
    for property in &object.properties {
        let key = spa_property_name(object.type_, property.key)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| property.key.to_string());
        let value = if object.type_ == spa::sys::SPA_TYPE_OBJECT_ParamRoute
            && property.key == spa::sys::SPA_PARAM_ROUTE_direction
        {
            direction_value_json(&property.value)
        } else if matches!(
            (object.type_, property.key),
            (
                spa::sys::SPA_TYPE_OBJECT_ParamRoute,
                spa::sys::SPA_PARAM_ROUTE_available
            ) | (
                spa::sys::SPA_TYPE_OBJECT_ParamProfile,
                spa::sys::SPA_PARAM_PROFILE_available
            )
        ) {
            availability_value_json(&property.value)
        } else {
            spa_value_json(&property.value)
        };
        result.insert(key, value);
    }
    JsonValue::Object(result)
}

fn spa_property_name(object_type: u32, key: u32) -> Option<&'static str> {
    if object_type == spa::sys::SPA_TYPE_OBJECT_ParamRoute {
        return match key {
            spa::sys::SPA_PARAM_ROUTE_index => Some("index"),
            spa::sys::SPA_PARAM_ROUTE_direction => Some("direction"),
            spa::sys::SPA_PARAM_ROUTE_device => Some("device"),
            spa::sys::SPA_PARAM_ROUTE_name => Some("name"),
            spa::sys::SPA_PARAM_ROUTE_description => Some("description"),
            spa::sys::SPA_PARAM_ROUTE_priority => Some("priority"),
            spa::sys::SPA_PARAM_ROUTE_available => Some("available"),
            spa::sys::SPA_PARAM_ROUTE_info => Some("info"),
            spa::sys::SPA_PARAM_ROUTE_profiles => Some("profiles"),
            spa::sys::SPA_PARAM_ROUTE_props => Some("props"),
            spa::sys::SPA_PARAM_ROUTE_devices => Some("devices"),
            spa::sys::SPA_PARAM_ROUTE_profile => Some("profile"),
            spa::sys::SPA_PARAM_ROUTE_save => Some("save"),
            _ => None,
        };
    }
    if object_type == spa::sys::SPA_TYPE_OBJECT_ParamProfile {
        return match key {
            spa::sys::SPA_PARAM_PROFILE_index => Some("index"),
            spa::sys::SPA_PARAM_PROFILE_name => Some("name"),
            spa::sys::SPA_PARAM_PROFILE_description => Some("description"),
            spa::sys::SPA_PARAM_PROFILE_priority => Some("priority"),
            spa::sys::SPA_PARAM_PROFILE_available => Some("available"),
            spa::sys::SPA_PARAM_PROFILE_info => Some("info"),
            spa::sys::SPA_PARAM_PROFILE_classes => Some("classes"),
            spa::sys::SPA_PARAM_PROFILE_save => Some("save"),
            _ => None,
        };
    }
    if object_type == spa::sys::SPA_TYPE_OBJECT_Props {
        return match key {
            spa::sys::SPA_PROP_volume => Some("volume"),
            spa::sys::SPA_PROP_mute => Some("mute"),
            spa::sys::SPA_PROP_channelVolumes => Some("channelVolumes"),
            _ => None,
        };
    }
    None
}

fn direction_value_json(value: &spa::pod::Value) -> JsonValue {
    match value {
        spa::pod::Value::Id(value) if value.0 == spa::sys::SPA_DIRECTION_INPUT => {
            JsonValue::String("Input".into())
        }
        spa::pod::Value::Id(value) if value.0 == spa::sys::SPA_DIRECTION_OUTPUT => {
            JsonValue::String("Output".into())
        }
        _ => spa_value_json(value),
    }
}

fn availability_value_json(value: &spa::pod::Value) -> JsonValue {
    match value {
        spa::pod::Value::Id(value) if value.0 == spa::sys::SPA_PARAM_AVAILABILITY_no => {
            JsonValue::String("no".into())
        }
        spa::pod::Value::Id(value) if value.0 == spa::sys::SPA_PARAM_AVAILABILITY_yes => {
            JsonValue::String("yes".into())
        }
        spa::pod::Value::Id(value) if value.0 == spa::sys::SPA_PARAM_AVAILABILITY_unknown => {
            JsonValue::String("unknown".into())
        }
        _ => spa_value_json(value),
    }
}

fn transient_registry_object_error(result: i32, message: &str) -> bool {
    result == -libc::ENOENT && message.trim_start().starts_with("unknown resource")
}

fn merge_json(target: &mut JsonValue, update: JsonValue) {
    match (target, update) {
        (JsonValue::Object(target), JsonValue::Object(update)) => {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_pods_use_pw_dump_compatible_names() {
        let object = spa::pod::Object {
            type_: spa::sys::SPA_TYPE_OBJECT_ParamRoute,
            id: spa::param::ParamType::EnumRoute.as_raw(),
            properties: vec![
                spa::pod::Property::new(
                    spa::sys::SPA_PARAM_ROUTE_direction,
                    spa::pod::Value::Id(spa::utils::Id(spa::sys::SPA_DIRECTION_INPUT)),
                ),
                spa::pod::Property::new(
                    spa::sys::SPA_PARAM_ROUTE_available,
                    spa::pod::Value::Id(spa::utils::Id(spa::sys::SPA_PARAM_AVAILABILITY_no)),
                ),
                spa::pod::Property::new(
                    spa::sys::SPA_PARAM_ROUTE_name,
                    spa::pod::Value::String("[In] Headset".into()),
                ),
            ],
        };
        assert_eq!(
            spa_object_json(&object),
            serde_json::json!({
                "direction": "Input",
                "available": "no",
                "name": "[In] Headset",
            })
        );
    }

    #[test]
    fn bootstrap_is_published_as_one_initialized_batch() {
        let cache = PipeWireRegistryCache::default();
        cache.mark_connected(false);
        let batches = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&batches);
        let hooks = NativeRegistryHooks {
            cache: cache.clone(),
            stopping: Arc::new(|| false),
            on_batch: Arc::new(move |batch| recorded.lock().unwrap().push(batch)),
        };
        let mut state = NativeRegistryState::new(&hooks);
        state.update(serde_json::json!({
            "id": 10,
            "type": "PipeWire:Interface:Node",
            "props": {"media.class": "Audio/Sink", "node.name": "alsa_output.test"}
        }));
        assert!(!cache.status().initialized);
        state.finish_bootstrap();
        assert!(cache.status().initialized);
        assert_eq!(batches.lock().unwrap().len(), 1);
        assert!(batches.lock().unwrap()[0].initial);
    }

    #[test]
    fn vanished_registry_objects_are_not_fatal_core_errors() {
        assert!(transient_registry_object_error(
            -libc::ENOENT,
            "unknown resource 78 op:7"
        ));
        assert!(!transient_registry_object_error(
            -libc::EPIPE,
            "connection closed"
        ));
    }
}
