# WaveLinux5 Hardware Acceleration

WaveLinux5 is the 5.0.0 release line. It keeps a separate application identity
from pre-5 WaveLinux installs while the hardware-acceleration work continues to
be hardened behind runtime switches.

## Identity And Paths

WaveLinux5 uses its own application identity:

- Product name: `WaveLinux5`
- Main binary: `wavelinux5`
- App identifier: `io.github.duskyprojects.WaveLinux5`
- Desktop file: `~/.local/share/applications/wavelinux5.desktop`
- Launcher: `~/.local/bin/wavelinux5`
- DSP helper: `~/.local/bin/wavelinux5-dsp-helper`
- AppImage: `~/.local/share/wavelinux5/WaveLinux5_5.0.0_amd64.AppImage`
- Config: `~/.config/wavelinux5/config.json`
- Runtime data: `~/.local/share/wavelinux5`
- Optional ALSA aliases: marked `WaveLinux5 ALSA aliases` block in `~/.asoundrc`

The stable `wavelinux` launcher, stable AppImage, stable desktop entry, and
stable config remain separate.

## Namespace Rules

WaveLinux5 sets these runtime defaults before the engine starts:

```text
WAVELINUX_XDG_APP_NAME=WaveLinux5
WAVELINUX_GRAPH_PREFIX=wavelinux5
WAVELINUX_GRAPH_PROPERTY_PREFIX=wavelinux5
WAVELINUX_APP_DISPLAY_NAME=WaveLinux5
```

The model and PipeWire planner derive virtual nodes from that namespace:

- Mix sinks: `wavelinux5_mix_<mix-id>`
- Mix monitor sources: `wavelinux5_mix_<mix-id>_source`
- Channel sinks: `wavelinux5_channel_<channel-id>`
- Effect-chain nodes: `wavelinux5_fx_<channel-id>_*`
- PipeWire ownership properties: `wavelinux5.managed`,
  `wavelinux5.role`, `wavelinux5.channel_id`, and related route metadata.

The local installer refreshes ALSA aliases for legacy applications that do not
enumerate PipeWire/PulseAudio sources directly. Those aliases use the same
namespace, for example `wavelinux5_mic`, `wavelinux5_mix_stream`,
`wavelinux5_mix_monitor`, and `wavelinux5_channel_hardware_in`. The
`wavelinux5_mic` alias targets the processed effect output named
`wavelinux5-mic`.

Stable WaveLinux still uses `wavelinux_*` node names and `wavelinux.*`
properties. Cleanup, stale-helper matching, graph parsing, and route planning
must use the active namespace only.

## AppImage Setup Behavior

WaveLinux5 AppImages bundle supported LADSPA plugins under
`usr/wavelinux-runtime/lib/ladspa` and prepend that directory to `LADSPA_PATH`
before dependency checks, effect availability probes, or helper launches run.
If bundled RNNoise or SWH dynamics are present, the installer does not ask the
host package manager to reinstall them.

When runtime packages are missing, WaveLinux5 chooses the privilege helper based
on launch context:

- Terminal install: try `sudo`, then fall back to `pkexec`.
- GUI/no-tty install: try `pkexec`, then fall back to `sudo`.

If neither helper is available or both fail, setup returns an error with the
manual package command. It does not report success after a skipped privileged
install.

On normal startup, WaveLinux5 probes `pactl info` before opening the UI. If the
host audio server is not reachable, it tries to start the user PipeWire stack
with `systemctl --user` and then with direct `pipewire`, `pipewire-pulse`, and
`wireplumber` daemon fallback. If `pactl` still cannot connect, WaveLinux5 exits
with a setup error because virtual sinks cannot be built.

## First Launch Config Import

If `~/.config/wavelinux5/config.json` does not exist, WaveLinux5 attempts to
read stable config from `~/.config/wavelinux/config.json`. The imported config
is rewritten into the `wavelinux5_*` namespace before it is saved.

Only desired mixer state is persisted in `MixerConfig`; live PipeWire route ids
are not saved there, so there are no transient route ids to clear during import.

## Runtime Modes

The experimental DSP runtime is controlled with:

```text
WAVELINUX_AUDIO_RUNTIME=pipewire_filter_chain|dsp_cpu|dsp_auto|dsp_accelerated
WAVELINUX_DSP_PROVIDER=auto|cuda|openvino|cpu
```

Modes:

- `pipewire_filter_chain`: existing PipeWire filter-chain path. This remains
  the rollback path.
- `dsp_cpu`: force the WaveLinux5 DSP helper's native PipeWire streams and CPU
  processing when every active effect in the channel is supported by the helper.
  Unsupported native effects, such as RNNoise, fall back to the filter-chain
  bridge for that channel.
- `dsp_auto`: probe CUDA, OpenVINO, portable CPU, then pure CPU fallback.
- `dsp_accelerated`: prefer CUDA/OpenVINO and report a fallback if neither
  provider is available.

Current release status: `dsp_cpu` is the native-helper path for high-pass, EQ,
compressor, gate, and limiter. It creates the same logical FX input/output
nodes as the filter-chain path, but audio is processed inside
`wavelinux5-dsp-helper`. RNNoise remains on the PipeWire LADSPA filter-chain
path. `dsp_auto` and `dsp_accelerated` still launch the helper-supervised
filter-chain rollback while accelerated providers are benchmarked and hardened.

Provider order is CUDA/NVIDIA, OpenVINO/Intel, portable CPU acceleration, pure
CPU fallback. Host GPU and ML runtimes are optional and are not bundled in the
AppImage.

## Noise Suppression FX

WaveLinux5 exposes one realtime microphone cleanup effect:

- `rnnoise`: the low-latency RNNoise LADSPA plugin. It is the primary
  user-selectable noise suppression effect in WaveLinux5.

The tested RNNoise Broadcast profile is the default and first preset:

- `vad_threshold`: `50`
- `hold_ms`: `200`
- `lead_in_ms`: `0`

DeepFilterNet3 was removed from the WaveLinux5 release line after live testing
showed it was not reliable enough across hardware and room-noise conditions.
The app no longer advertises, installs, stages, probes, or renders the
DeepFilterNet LADSPA plugin. Existing configs that still contain a
`deepfilternet` effect id are migrated to RNNoise Broadcast during model
normalization so the config remains loadable.

If the effect-chain log reports realtime underruns, WaveLinux5 keeps reporting
the underrun in Health diagnostics. Heavy-effect quarantine remains generic for
future effects that explicitly opt into it, but RNNoise is not treated as a
heavy fallback effect.

## DSP Helper

`crates/dsp` builds `wavelinux5-dsp-helper`. The helper exposes:

```sh
wavelinux5-dsp-helper --probe
wavelinux5-dsp-helper --run-native --config ~/.local/share/wavelinux5/effects/wavelinux5-chain-hardware_in.json
wavelinux5-dsp-helper --run-filter-chain --channel-id hardware_in --config ~/.local/share/wavelinux5/effects/wavelinux5-chain-hardware_in.conf
wavelinux5-dsp-helper --bench-fixture --frames 240000 --sample-rate 48000
```

The engine writes two files per active FX channel:

- `*.conf`: PipeWire filter-chain rollback config.
- `*.json`: native helper config with channel id, WaveLinux5 node names,
  WaveLinux5 ownership property prefix, sample rate, latency target, and effect
  chain parameters.

The helper includes provider probing, the filter-chain bridge, native PipeWire
stream endpoints, and CPU DSP nodes for high-pass filtering, 3-band EQ,
compressor, gate, and limiter. CUDA and OpenVINO are provider probes in this
experimental implementation; they are selected only when the host runtime is
discoverable, otherwise the helper reports CPU fallback.

Native helper logs include `native_start`, capture/playback stream state,
`native_stats` counters for captured/rendered/dropped/underrun frames,
per-buffer processing time, and `native_stop`. Those logs are written to the
same channel log path as the filter-chain bridge.

Health diagnostics include the requested runtime, effective runtime, requested
provider, selected provider, acceleration flag, provider probe failures, runtime
fallback reason, and fallback count when WaveLinux5 or a DSP override is active.

## Build And Install

Build and install the side-by-side release line from the checkout:

```sh
bash scripts/build-local.sh
bash scripts/install-local.sh
```

The installer stops only WaveLinux5-owned processes:

- `wavelinux5`
- `WaveLinux5_*_amd64.AppImage`
- `wavelinux5-dsp-helper`
- WaveLinux5-owned fallback `pipewire -c .../wavelinux5/.../wavelinux5-chain-*`
  helpers

After those processes are stopped, install and uninstall also unload only
Pulse/PipeWire modules whose module metadata contains the WaveLinux5 namespace
(`wavelinux5` or `WaveLinux5`). This clears modules left behind by a forced
kill or crashed build without touching pre-5 WaveLinux modules.

It must not kill stable `wavelinux`, stable `WaveLinux_*` AppImages, or stable
effect helpers.

## Benchmark Gate

Run:

```sh
bash scripts/bench-audio-runtime.sh
```

The script writes JSONL reports under `target/bench` for:

- `pipewire_filter_chain`
- `dsp_cpu`
- `dsp_auto`

Keep the accelerated path experimental until `dsp_auto` shows at least 30% lower
helper CPU than the current filter-chain fallback on the hardware input chain,
with no latency regression and no new PipeWire underruns or errors.

For live underrun checks during a benchmark:

```sh
journalctl --user --since "5 minutes ago" | grep -Ei "pipewire|underrun|xrun|error"
```
