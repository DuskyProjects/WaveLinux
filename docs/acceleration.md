# DSP Acceleration Policy

WaveLinux 6 ships a native CPU audio path. RNNoise and the low-order effects run
inside `wavelinux6-audio-core`; they do not require CUDA, OpenVINO, ONNX Runtime,
or a distro LADSPA package.

## Current Status

The checked-in implementation includes provider protocol v1, fixed-capacity
shared-memory queues, an isolated ONNX Runtime process, CUDA/OpenVINO/MIGraphX
pack tooling, machine-local qualification records, live Health counters, and
an exact block-boundary CPU fallback. The RNNoise recurrent state is explicit
on both paths, so a late, invalid, stale, or failed provider result is discarded
without corrupting the committed CPU state.

Qualified provider inference runs only on the per-channel non-real-time DSP
worker. PipeWire callbacks never wait for IPC or inference. The installed
launcher uses `dsp_auto`: it selects a provider only when the pack is valid and
its qualification record matches this machine, model, and pack version. With
no qualified pack, the same setting selects native CPU without reporting a
failure. Detecting a host driver or runtime is diagnostic evidence only, never
permission to place a workload on it.

Developer overrides are:

```text
WAVELINUX_AUDIO_RUNTIME=pipewire_filter_chain|dsp_cpu|dsp_auto|dsp_accelerated
WAVELINUX_DSP_PROVIDER=auto|cuda|openvino|migraphx|cpu
WAVELINUX_DSP_FORCE_PROVIDER_FAIL=<provider list>
```

These variables do not download drivers or packages. The normal production
configuration uses the persistent CPU core.

## Provider Pack Contract

CUDA, OpenVINO, and AMD MIGraphX providers are separately versioned,
user-installed packs under `~/.local/share/wavelinux6/providers/<provider>`.
The main AppImage does not bundle them. A pack contains a private executable,
the generated RNNoise neural model, its golden fixture, and a manifest with
SHA-256 hashes. WaveLinux rejects packs or members owned by another user or
writable by a group or other users.

The provider runs in an isolated child process and authenticates protocol,
provider kind, model hash, shared-memory path, and a per-launch nonce before
receiving blocks. Requests and responses use a bounded shared-memory queue. A
provider crash, queue overflow, non-finite output, stale response, or missed
deadline selects the native CPU result from the last committed recurrent state;
it never removes public PipeWire nodes or rebuilds the graph. Three consecutive
provider failures disable that state for the current chain. A later effect-chain
swap may start a fresh qualified provider process while the old CPU-backed chain
continues through the normal 20 ms crossfade.

Each RNNoise state gets a distinct private socket, shared-memory queue, and
provider child. Mono microphone chains therefore use one provider state;
stereo chains use two. Health reports provider PIDs, active/disabled states,
provider and CPU-fallback blocks, deadline misses, invalid/stale results,
startup failures, and the latest fallback reason per channel.

A pack may be enabled automatically only after qualification on the exact
machine and workload proves:

- at least 30% active-core CPU reduction;
- no latency regression;
- no new underruns, dropped frames, or silent intervals;
- callback/process time within the current quantum budget;
- reliable fallback under forced provider termination.

Ordinary EQ, filters, dynamics, delay, and mixing remain CPU DSP. Driver
detection by itself is not qualification.

Build and install an unqualified development pack with:

```bash
bash scripts/build-accelerator-pack.sh cuda  # or openvino/migraphx
bash scripts/install-accelerator-pack.sh cuda
bash scripts/qualify-accelerator-pack.sh cuda
```

The qualifier verifies protocol, numerical equivalence, IPC, and isolated
runtime performance. It intentionally cannot self-approve the live-audio,
continuity, latency, and total-process CPU fields. Those gates must be supplied
by the stress/benchmark harness on the exact machine before `qualified` becomes
true. Replacing a pack removes its old qualification record.

## Security And Installation

Provider packs are optional user data and must not request root. Host GPU
drivers and matching ONNX Runtime libraries remain the user's or distro's
responsibility. Pack qualification is bound to the provider, binary/model
hashes, and a hardware fingerprint. The current user-only local pack workflow
does not define a remote distribution trust/signature policy; provider packs
must not be published as downloadable artifacts until that policy exists.

The committed model is reproducibly generated from the pinned `nnnoiseless`
weights. `scripts/test-all.sh` verifies the weights, model, and fixture hashes,
regenerates and byte-compares artifacts when Python ONNX tooling is available,
and runs the isolated provider termination/fallback test when a host ONNX
Runtime library is available.
