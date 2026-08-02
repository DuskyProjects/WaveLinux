use std::process::Command;

use wavelinux_model::MixerConfig;
use wavelinux_pw::PipeWireRegistryCache;

#[test]
#[ignore = "reads the live user PipeWire registry"]
fn live_pipewire_dump_builds_a_complete_audio_snapshot() {
    // This isolated integration-test binary does not inherit the application
    // launcher's WaveLinux 6 namespace defaults.
    std::env::set_var("WAVELINUX_GRAPH_PREFIX", "wavelinux6");
    std::env::set_var("WAVELINUX_GRAPH_PROPERTY_PREFIX", "wavelinux6");

    let output = Command::new("pw-dump")
        .arg("--no-colors")
        .output()
        .expect("run pw-dump");
    assert!(output.status.success());

    let batches = serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<Vec<serde_json::Value>>()
        .collect::<Result<Vec<_>, _>>()
        .expect("parse pw-dump output");
    let cache = PipeWireRegistryCache::default();
    cache.mark_connected(false);
    for batch in batches {
        cache.apply_batch(batch);
    }

    let config = MixerConfig::default();
    let (snapshot, generation) = cache
        .audio_state_snapshot(Some(&config), Vec::new())
        .expect("native registry snapshot");
    assert!(generation > 0);
    assert!(!snapshot.graph.inputs.is_empty());
    assert!(!snapshot.graph.outputs.is_empty());
    assert!(snapshot.default_source.is_some());
    assert!(snapshot.default_sink.is_some());
    assert!(snapshot
        .graph
        .inputs
        .iter()
        .any(|input| input.name == "wavelinux6-mic"));
    assert!(snapshot
        .graph
        .outputs
        .iter()
        .any(|output| output.name.starts_with("wavelinux6_channel_")));
    assert!(snapshot.routes.source_output_routes.iter().any(|route| {
        route.role.as_deref() == Some("input_target")
            && route.channel_id.as_deref() == Some("hardware_in")
    }));
    assert!(snapshot.routes.sink_input_routes.iter().any(|route| {
        route.role.as_deref() == Some("mix_output_target")
            && route.mix_id.as_deref() == Some("monitor")
    }));
}
