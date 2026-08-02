import { describe, expect, it } from "vitest";
import { demoState } from "../demo";
import { buildTestingHealthReport } from "./TestingHealthReport";

describe("buildTestingHealthReport", () => {
  it("includes native audio integrity and queue counters", () => {
    const state = structuredClone(demoState);
    state.engine.audio_core = [
      {
        channel_id: "hardware_in",
        online: true,
        sample_rate_hz: 48_000,
        target_latency_msec: 28,
        current_buffer_frames: 1_344,
        buffer_fill_msec: 28,
        captured_frames: 96_000,
        rendered_frames: 96_000,
        dropped_frames: 0,
        underrun_frames: 0,
        underrun_delta: 0,
        capture_callbacks: 100,
        worker_running: true,
        worker_blocks: 100,
        worker_queue_frames: 0,
        worker_queue_capacity_frames: 32768,
        worker_overrun_frames: 0,
        accelerator_provider: "cuda",
        accelerator_active_states: 1,
        accelerator_provider_pids: [4242],
        accelerator_provider_blocks: 90,
        accelerator_fallback_blocks: 2,
        accelerator_deadline_misses: 1,
        accelerator_invalid_results: 0,
        accelerator_stale_results: 1,
        accelerator_disabled_states: 0,
        accelerator_startup_failures: [],
        accelerator_last_failure: "provider missed the block deadline",
        last_process_micros: 90,
        max_process_micros: 220,
        chain_swaps: 3,
        non_finite_blocks: 2,
        non_finite_samples: 128,
        non_finite_effect_mask: 1,
        chain_recoveries: 1,
        chain_swap_replacements: 4,
        retired_chain_overflows: 0,
        submitted_generation: 8,
        acknowledged_generation: 7,
        submitted_route_generation: 5,
        applied_route_generation: 4,
        input_target_node_name: "alsa_input.usb_cm01",
        output_target_node_names: [],
        route_target_error: null,
        rate_correction: 1,
        error: null,
      },
    ];
    state.engine.meter_transport = {
      protocol_version: 1,
      connected: true,
      slot_count: 8,
      last_sequence: 900,
      frames_received: 899,
      connections: 1,
      disconnects: 0,
      fallback_polls: 0,
      errors: 0,
      last_error: null,
    };
    state.engine.pipewire_audio_health.profiler_available = true;
    state.engine.pipewire_audio_health.profiler_samples = 30;
    state.engine.pipewire_audio_health.direct_errors = 2;
    state.engine.pipewire_audio_health.owned_direct_errors = 1;
    state.engine.pipewire_registry = {
      available: true,
      connected: true,
      initialized: true,
      generation: 42,
      object_count: 96,
      node_count: 18,
      device_count: 3,
      port_count: 24,
      link_count: 16,
      metadata_count: 2,
      playback_stream_count: 1,
      capture_stream_count: 2,
      batches_received: 43,
      objects_changed: 112,
      direct_link_failures: 0,
      direct_node_errors: 0,
      reconnects: 1,
      last_event_unix: 1_700_000_000,
      last_error: null,
    };
    state.engine.accelerator_providers = [
      {
        provider: "cuda",
        protocol_version: 1,
        installed: true,
        valid: true,
        qualified: false,
        active: false,
        pack_version: "0.1.0",
        model_sha256: "abc",
        hardware_fingerprint: "host",
        tested_unix: 1_700_000_000,
        blocks: 5_000,
        numerical_max_abs_error: 0.000004,
        deadline_misses: 0,
        discontinuities: 0,
        added_latency_msec: 0,
        cpu_reduction_percent: 12,
        fallback_validated: false,
        live_workload_validated: false,
        detail: "provider failed qualification",
      },
    ];

    const report = buildTestingHealthReport({
      audioActionReport: null,
      diagnostics: [],
      elgatoDeviceError: null,
      elgatoDevices: [],
      graphReport: null,
      report: null,
      state,
      streamerDeviceError: null,
      streamerDevices: [],
      updateInfo: null,
    });

    expect(report).toContain("non_finite_samples=128");
    expect(report).toContain("effect_mask=0x1");
    expect(report).toContain("recoveries=1");
    expect(report).toContain("replacements=4");
    expect(report).toContain("retired_overflows=0");
    expect(report).toContain("accelerator=cuda accelerator_states=1 accelerator_pids=4242");
    expect(report).toContain("accelerator_blocks=90 accelerator_fallbacks=2");
    expect(report).toContain("accelerator_deadlines=1 accelerator_invalid=0 accelerator_stale=1");
    expect(report).toContain("submitted_generation=8 acknowledged_generation=7");
    expect(report).toContain("submitted_route_generation=5 applied_route_generation=4");
    expect(report).toContain("targets=alsa_input.usb_cm01");
    expect(report).toContain("Meter transport: protocol=1; connected=yes; slots=8");
    expect(report).toContain("profiler=active; profiler_samples=30; direct_errors=2; owned_direct_errors=1");
    expect(report).toContain("PipeWire registry: available=yes; connected=yes; initialized=yes; generation=42");
    expect(report).toContain("direct_link_failures=0; direct_node_errors=0; reconnects=1");
    expect(report).toContain("cuda=installed:yes,valid:yes,qualified:no,active:no");
    expect(report).toContain("cpu:12%");
  });
});
