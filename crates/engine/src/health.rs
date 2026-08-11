use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use time::OffsetDateTime;
use wavelinux_model::PipeWireAudioHealthStatus;

#[derive(Debug, Default)]
pub(crate) struct PipeWireAudioHealthTracker {
    monitor_available: AtomicBool,
    profiler_available: AtomicBool,
    profiler_samples: AtomicU64,
    profiler_warmup_frames: AtomicU64,
    direct_errors: AtomicU64,
    owned_direct_errors: AtomicU64,
    profiler_node_errors: Mutex<BTreeMap<u32, (String, u64)>>,
    warning_events: AtomicU64,
    out_of_buffers: AtomicU64,
    resyncs: AtomicU64,
    link_failures: AtomicU64,
    xruns: AtomicU64,
    owned_events: AtomicU64,
    last_event_unix: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CpuTimes {
    total: u64,
    idle: u64,
}

#[derive(Debug, Default)]
pub(crate) struct CpuPressureSampler {
    previous_cpu: Option<CpuTimes>,
    previous_scheduler_stall: Option<(u64, Instant)>,
}

impl CpuPressureSampler {
    pub(crate) fn sample(&mut self) -> Option<f32> {
        let now = Instant::now();
        let busy_pressure = fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|stat| parse_proc_stat_cpu(&stat))
            .and_then(|current| {
                self.previous_cpu
                    .replace(current)
                    .and_then(|previous| cpu_pressure_between(previous, current))
            });
        let scheduler_pressure = fs::read_to_string("/proc/pressure/cpu")
            .ok()
            .and_then(|pressure| parse_proc_pressure_total(&pressure))
            .and_then(|current| {
                self.previous_scheduler_stall
                    .replace((current, now))
                    .and_then(|(previous, sampled_at)| {
                        stall_pressure_between(previous, current, now.duration_since(sampled_at))
                    })
            });
        let load_pressure = fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|loadavg| {
                let cpu_count = std::thread::available_parallelism().ok()?.get();
                parse_proc_load_pressure(&loadavg, cpu_count)
            });

        [busy_pressure, scheduler_pressure, load_pressure]
            .into_iter()
            .flatten()
            .reduce(f32::max)
    }
}

impl PipeWireAudioHealthTracker {
    pub(crate) fn set_monitor_available(&self, available: bool) {
        self.monitor_available.store(available, Ordering::Release);
    }

    pub(crate) fn set_profiler_available(&self, available: bool) {
        self.profiler_available.store(available, Ordering::Release);
    }

    pub(crate) fn observe_profiler_line(&self, line: &str, owned_prefix: &str) -> bool {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.first() == Some(&"S") && fields.get(1) == Some(&"ID") {
            self.profiler_samples.fetch_add(1, Ordering::Relaxed);
            self.profiler_warmup_frames.fetch_add(1, Ordering::Release);
            return false;
        }
        let Some(node_id) = fields.get(1).and_then(|value| value.parse::<u32>().ok()) else {
            return false;
        };
        let Some(errors) = fields.get(8).and_then(|value| value.parse::<u64>().ok()) else {
            return false;
        };
        if !fields.get(9).is_some_and(|value| audio_format(value)) {
            return false;
        }
        let identity = fields
            .get(12..)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|value| *value != "+")
            .collect::<Vec<_>>()
            .join(" ");
        if identity.is_empty() {
            return false;
        }
        let delta = self
            .profiler_node_errors
            .lock()
            .ok()
            .and_then(|mut previous| {
                let old = previous.insert(node_id, (identity.clone(), errors))?;
                if old.0 != identity || errors < old.1 {
                    return Some(0);
                }
                Some(errors.saturating_sub(old.1))
            })
            .unwrap_or_default();
        // `pw-top` starts with a synthetic current-state frame whose counters
        // may be zero, followed by the first real profiler frame. Warm both
        // frames into the baseline so historical errors cannot look new.
        if self.profiler_warmup_frames.load(Ordering::Acquire) < 3 || delta == 0 {
            return false;
        }

        // An idle node can advance its PipeWire error counter while a device
        // profile is being activated or suspended. Keep that value as the
        // next running baseline, but do not report it as an audible failure.
        if fields.first() != Some(&"R") {
            return false;
        }

        let owned = !owned_prefix.is_empty()
            && identity
                .to_ascii_lowercase()
                .contains(&owned_prefix.to_ascii_lowercase());
        self.direct_errors.fetch_add(delta, Ordering::Relaxed);
        self.owned_direct_errors
            .fetch_add(if owned { delta } else { 0 }, Ordering::Relaxed);
        self.warning_events.fetch_add(delta, Ordering::Relaxed);
        self.xruns.fetch_add(delta, Ordering::Relaxed);
        self.owned_events
            .fetch_add(if owned { delta } else { 0 }, Ordering::Relaxed);
        self.mark_event();
        true
    }

    pub(crate) fn reset_profiler_baseline(&self) {
        if let Ok(mut previous) = self.profiler_node_errors.lock() {
            previous.clear();
        }
        self.profiler_warmup_frames.store(0, Ordering::Release);
    }

    pub(crate) fn observe_line(&self, line: &str, owned_prefix: &str) -> bool {
        let line = line.to_ascii_lowercase();
        let out_of_buffers = line.contains("out of buffers");
        let resync = line.contains("resync");
        let link_failure = line.contains("link failed") || line.contains("failed to activate");
        let xrun = line.contains("xrun") || line.contains("underrun") || line.contains("overrun");
        if !out_of_buffers && !resync && !link_failure && !xrun {
            return false;
        }

        self.warning_events.fetch_add(1, Ordering::Relaxed);
        self.out_of_buffers
            .fetch_add(u64::from(out_of_buffers), Ordering::Relaxed);
        self.resyncs.fetch_add(u64::from(resync), Ordering::Relaxed);
        self.link_failures
            .fetch_add(u64::from(link_failure), Ordering::Relaxed);
        self.xruns.fetch_add(u64::from(xrun), Ordering::Relaxed);
        if !owned_prefix.is_empty() && line.contains(&owned_prefix.to_ascii_lowercase()) {
            self.owned_events.fetch_add(1, Ordering::Relaxed);
        }
        self.mark_event();
        true
    }

    pub(crate) fn snapshot(&self) -> PipeWireAudioHealthStatus {
        let last_event_unix = self.last_event_unix.load(Ordering::Relaxed);
        PipeWireAudioHealthStatus {
            monitor_available: self.monitor_available.load(Ordering::Acquire),
            profiler_available: self.profiler_available.load(Ordering::Acquire),
            profiler_samples: self.profiler_samples.load(Ordering::Relaxed),
            direct_errors: self.direct_errors.load(Ordering::Relaxed),
            owned_direct_errors: self.owned_direct_errors.load(Ordering::Relaxed),
            warning_events: self.warning_events.load(Ordering::Relaxed),
            out_of_buffers: self.out_of_buffers.load(Ordering::Relaxed),
            resyncs: self.resyncs.load(Ordering::Relaxed),
            link_failures: self.link_failures.load(Ordering::Relaxed),
            xruns: self.xruns.load(Ordering::Relaxed),
            owned_events: self.owned_events.load(Ordering::Relaxed),
            last_event_unix: (last_event_unix > 0).then_some(last_event_unix as i64),
        }
    }

    fn mark_event(&self) {
        self.last_event_unix.store(
            OffsetDateTime::now_utc().unix_timestamp().max(0) as u64,
            Ordering::Relaxed,
        );
    }
}

fn audio_format(value: &str) -> bool {
    let value = value.to_ascii_uppercase();
    value.starts_with('F')
        || value.starts_with('S')
        || value.starts_with('U')
        || value.starts_with("ALAW")
        || value.starts_with("ULAW")
}

pub(crate) fn pipewire_health_deltas(
    previous: &PipeWireAudioHealthStatus,
    current: &PipeWireAudioHealthStatus,
) -> (u64, u64) {
    (
        current
            .warning_events
            .saturating_sub(previous.warning_events),
        current.owned_events.saturating_sub(previous.owned_events),
    )
}

pub(crate) fn parse_proc_stat_cpu(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().find(|line| line.starts_with("cpu "))?;
    let values = line
        .split_whitespace()
        .skip(1)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() < 4 {
        return None;
    }
    let total = values.iter().copied().fold(0_u64, u64::saturating_add);
    let idle = values[3].saturating_add(values.get(4).copied().unwrap_or(0));
    Some(CpuTimes { total, idle })
}

pub(crate) fn cpu_pressure_between(previous: CpuTimes, current: CpuTimes) -> Option<f32> {
    let total = current.total.saturating_sub(previous.total);
    if total == 0 {
        return None;
    }
    let idle = current.idle.saturating_sub(previous.idle).min(total);
    Some(((total - idle) as f64 / total as f64).clamp(0.0, 1.0) as f32)
}

pub(crate) fn parse_proc_pressure_total(pressure: &str) -> Option<u64> {
    pressure
        .lines()
        .find(|line| line.starts_with("some "))?
        .split_whitespace()
        .find_map(|field| field.strip_prefix("total="))?
        .parse()
        .ok()
}

pub(crate) fn stall_pressure_between(
    previous_total_micros: u64,
    current_total_micros: u64,
    elapsed: Duration,
) -> Option<f32> {
    let elapsed_micros = elapsed.as_micros();
    if elapsed_micros == 0 {
        return None;
    }
    let stalled_micros = current_total_micros.saturating_sub(previous_total_micros);
    Some((stalled_micros as f64 / elapsed_micros as f64).clamp(0.0, 1.0) as f32)
}

pub(crate) fn parse_proc_load_pressure(loadavg: &str, cpu_count: usize) -> Option<f32> {
    let one_minute_load = loadavg.split_whitespace().next()?.parse::<f32>().ok()?;
    Some((one_minute_load / cpu_count.max(1) as f32).clamp(0.0, 1.0))
}
