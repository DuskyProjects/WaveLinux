//! Isolated accelerator-pack protocol for WaveLinux 6.
//!
//! The audio core and provider process exchange fixed-size RNNoise neural-stage
//! blocks through a private shared-memory file. Control and lifecycle messages
//! remain on an authenticated Unix socket. Provider availability alone never
//! enables acceleration; a machine-local qualification record is also required.

use memmap2::{MmapMut, MmapOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem::{align_of, size_of};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use thiserror::Error;

pub mod rnnoise;

pub const ACCELERATOR_PROTOCOL_VERSION: u16 = 1;
pub const SHARED_MEMORY_MAGIC: [u8; 8] = *b"WL6ACCEL";
pub const SHARED_QUEUE_CAPACITY: usize = 8;
pub const RNNOISE_FEATURE_COUNT: usize = 42;
pub const RNNOISE_GAIN_COUNT: usize = 22;
pub const RNNOISE_VAD_STATE_COUNT: usize = 24;
pub const RNNOISE_NOISE_STATE_COUNT: usize = 48;
pub const RNNOISE_DENOISE_STATE_COUNT: usize = 96;
pub const RNNOISE_STATE_COUNT: usize =
    RNNOISE_VAD_STATE_COUNT + RNNOISE_NOISE_STATE_COUNT + RNNOISE_DENOISE_STATE_COUNT;

const QUALIFICATION_SCHEMA_VERSION: u16 = 1;
const MAX_CONTROL_MESSAGE_BYTES: usize = 16 * 1024;
const PROVIDER_START_TIMEOUT: Duration = Duration::from_secs(4);
static PROVIDER_CLIENT_INSTANCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorProvider {
    Cuda,
    OpenVino,
    MiGraphX,
    Cpu,
}

impl AcceleratorProvider {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "cuda" | "nvidia" => Some(Self::Cuda),
            "openvino" | "intel" => Some(Self::OpenVino),
            "migraphx" | "amd" | "rocm" => Some(Self::MiGraphX),
            "cpu" | "portable_cpu" => Some(Self::Cpu),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cuda => "cuda",
            Self::OpenVino => "openvino",
            Self::MiGraphX => "migraphx",
            Self::Cpu => "cpu",
        }
    }

    pub const fn execution_provider_name(self) -> &'static str {
        match self {
            Self::Cuda => "CUDAExecutionProvider",
            Self::OpenVino => "OpenVINOExecutionProvider",
            Self::MiGraphX => "MIGraphXExecutionProvider",
            Self::Cpu => "CPUExecutionProvider",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderPackManifest {
    pub protocol_version: u16,
    pub pack_version: String,
    pub provider: AcceleratorProvider,
    pub executable: String,
    pub executable_sha256: String,
    pub model: String,
    pub model_sha256: String,
    pub golden_fixture: String,
    pub golden_fixture_sha256: String,
    pub onnx_runtime_library: Option<String>,
}

impl ProviderPackManifest {
    pub fn validate(&self) -> Result<(), AcceleratorError> {
        if self.protocol_version != ACCELERATOR_PROTOCOL_VERSION {
            return Err(AcceleratorError::ProtocolVersion {
                expected: ACCELERATOR_PROTOCOL_VERSION,
                actual: self.protocol_version,
            });
        }
        if self.pack_version.trim().is_empty()
            || self.executable.trim().is_empty()
            || self.model.trim().is_empty()
        {
            return Err(AcceleratorError::InvalidManifest(
                "pack version, executable, and model are required".into(),
            ));
        }
        validate_sha256(&self.executable_sha256)?;
        validate_sha256(&self.model_sha256)?;
        validate_sha256(&self.golden_fixture_sha256)?;
        Ok(())
    }

    pub fn resolve(&self, pack_dir: &Path) -> Result<ResolvedProviderPack, AcceleratorError> {
        self.validate()?;
        let executable = resolve_pack_member(pack_dir, &self.executable)?;
        let model = resolve_pack_member(pack_dir, &self.model)?;
        if !executable.is_file() {
            return Err(AcceleratorError::InvalidManifest(format!(
                "provider executable is missing: {}",
                executable.display()
            )));
        }
        validate_private_pack_member(&executable, true)?;
        let actual_executable_sha256 = sha256_file(&executable)?;
        if actual_executable_sha256 != self.executable_sha256.to_ascii_lowercase() {
            return Err(AcceleratorError::ExecutableHashMismatch {
                expected: self.executable_sha256.to_ascii_lowercase(),
                actual: actual_executable_sha256,
            });
        }
        if !model.is_file() {
            return Err(AcceleratorError::InvalidManifest(format!(
                "provider model is missing: {}",
                model.display()
            )));
        }
        validate_private_pack_member(&model, false)?;
        let actual_model_sha256 = sha256_file(&model)?;
        if actual_model_sha256 != self.model_sha256.to_ascii_lowercase() {
            return Err(AcceleratorError::ModelHashMismatch {
                expected: self.model_sha256.to_ascii_lowercase(),
                actual: actual_model_sha256,
            });
        }
        let golden_fixture = resolve_pack_member(pack_dir, &self.golden_fixture)?;
        if !golden_fixture.is_file() {
            return Err(AcceleratorError::InvalidManifest(format!(
                "provider golden fixture is missing: {}",
                golden_fixture.display()
            )));
        }
        validate_private_pack_member(&golden_fixture, false)?;
        let actual_fixture_sha256 = sha256_file(&golden_fixture)?;
        if actual_fixture_sha256 != self.golden_fixture_sha256.to_ascii_lowercase() {
            return Err(AcceleratorError::FixtureHashMismatch {
                expected: self.golden_fixture_sha256.to_ascii_lowercase(),
                actual: actual_fixture_sha256,
            });
        }
        let onnx_runtime_library = self
            .onnx_runtime_library
            .as_deref()
            .map(|path| resolve_pack_member(pack_dir, path))
            .transpose()?;
        Ok(ResolvedProviderPack {
            manifest: self.clone(),
            pack_dir: pack_dir.to_path_buf(),
            executable,
            model,
            golden_fixture,
            onnx_runtime_library,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProviderPack {
    pub manifest: ProviderPackManifest,
    pub pack_dir: PathBuf,
    pub executable: PathBuf,
    pub model: PathBuf,
    pub golden_fixture: PathBuf,
    pub onnx_runtime_library: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct QualifiedProviderPack {
    resolved: ResolvedProviderPack,
}

impl QualifiedProviderPack {
    pub fn resolved(&self) -> &ResolvedProviderPack {
        &self.resolved
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderPackProbe {
    pub provider: AcceleratorProvider,
    pub installed: bool,
    pub valid: bool,
    pub qualified: bool,
    pub pack_dir: Option<PathBuf>,
    pub pack_version: Option<String>,
    pub model_sha256: Option<String>,
    pub hardware_fingerprint: String,
    pub qualification: Option<QualificationRecord>,
    pub detail: String,
}

pub fn provider_data_root() -> PathBuf {
    std::env::var_os("WAVELINUX_PROVIDER_ROOT")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .map(|path| path.join("wavelinux6/providers"))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|path| path.join(".local/share/wavelinux6/providers"))
        })
        .unwrap_or_else(|| PathBuf::from(".local/share/wavelinux6/providers"))
}

pub fn hardware_fingerprint(provider: AcceleratorProvider) -> String {
    let mut components = vec![format!("provider={}", provider.as_str())];
    if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") {
        if let Some(model) = cpuinfo
            .lines()
            .find(|line| line.starts_with("model name") || line.starts_with("Hardware"))
        {
            components.push(model.trim().to_string());
        }
    }
    let expected_vendor = match provider {
        AcceleratorProvider::Cuda => Some("0x10de"),
        AcceleratorProvider::OpenVino => Some("0x8086"),
        AcceleratorProvider::MiGraphX => Some("0x1002"),
        AcceleratorProvider::Cpu => None,
    };
    if let Ok(devices) = fs::read_dir("/sys/bus/pci/devices") {
        let mut pci = devices
            .flatten()
            .filter_map(|entry| {
                let root = entry.path();
                let vendor = fs::read_to_string(root.join("vendor")).ok()?;
                if expected_vendor.is_some_and(|expected| vendor.trim() != expected) {
                    return None;
                }
                let device = fs::read_to_string(root.join("device")).ok()?;
                let subsystem_vendor =
                    fs::read_to_string(root.join("subsystem_vendor")).unwrap_or_default();
                let subsystem_device =
                    fs::read_to_string(root.join("subsystem_device")).unwrap_or_default();
                Some(format!(
                    "pci={}:{}:{}:{}",
                    vendor.trim(),
                    device.trim(),
                    subsystem_vendor.trim(),
                    subsystem_device.trim()
                ))
            })
            .collect::<Vec<_>>();
        pci.sort();
        components.extend(pci);
    }
    format!("{:x}", Sha256::digest(components.join("\n")))
}

pub fn probe_provider_pack(provider: AcceleratorProvider) -> ProviderPackProbe {
    probe_provider_pack_at(&provider_data_root(), provider)
}

pub fn load_qualified_provider_pack(
    provider: AcceleratorProvider,
) -> Result<QualifiedProviderPack, AcceleratorError> {
    load_qualified_provider_pack_at(&provider_data_root(), provider)
}

pub fn load_qualified_provider_pack_at(
    root: &Path,
    provider: AcceleratorProvider,
) -> Result<QualifiedProviderPack, AcceleratorError> {
    let probe = probe_provider_pack_at(root, provider);
    if !probe.qualified {
        return Err(AcceleratorError::ProviderNotQualified(probe.detail));
    }
    let pack_dir = root.join(provider.as_str());
    let manifest: ProviderPackManifest =
        serde_json::from_slice(&fs::read(pack_dir.join("manifest.json"))?)?;
    if manifest.provider != provider {
        return Err(AcceleratorError::InvalidManifest(
            "provider manifest kind does not match its directory".into(),
        ));
    }
    Ok(QualifiedProviderPack {
        resolved: manifest.resolve(&pack_dir)?,
    })
}

pub fn probe_provider_pack_at(root: &Path, provider: AcceleratorProvider) -> ProviderPackProbe {
    let fingerprint = hardware_fingerprint(provider);
    let pack_dir = root.join(provider.as_str());
    let manifest_path = pack_dir.join("manifest.json");
    if !manifest_path.is_file() {
        return ProviderPackProbe {
            provider,
            installed: false,
            valid: false,
            qualified: false,
            pack_dir: None,
            pack_version: None,
            model_sha256: None,
            hardware_fingerprint: fingerprint,
            qualification: None,
            detail: "provider pack is not installed".into(),
        };
    }
    if let Err(error) = validate_private_pack_member(&pack_dir, false)
        .and_then(|_| validate_private_pack_member(&manifest_path, false))
    {
        return ProviderPackProbe {
            provider,
            installed: true,
            valid: false,
            qualified: false,
            pack_dir: Some(pack_dir),
            pack_version: None,
            model_sha256: None,
            hardware_fingerprint: fingerprint,
            qualification: None,
            detail: error.to_string(),
        };
    }
    let manifest = match fs::read(&manifest_path)
        .map_err(AcceleratorError::from)
        .and_then(|payload| {
            serde_json::from_slice::<ProviderPackManifest>(&payload).map_err(AcceleratorError::from)
        }) {
        Ok(manifest) if manifest.provider == provider => manifest,
        Ok(manifest) => {
            return ProviderPackProbe {
                provider,
                installed: true,
                valid: false,
                qualified: false,
                pack_dir: Some(pack_dir),
                pack_version: Some(manifest.pack_version),
                model_sha256: Some(manifest.model_sha256),
                hardware_fingerprint: fingerprint,
                qualification: None,
                detail: "provider manifest kind does not match its directory".into(),
            };
        }
        Err(error) => {
            return ProviderPackProbe {
                provider,
                installed: true,
                valid: false,
                qualified: false,
                pack_dir: Some(pack_dir),
                pack_version: None,
                model_sha256: None,
                hardware_fingerprint: fingerprint,
                qualification: None,
                detail: error.to_string(),
            };
        }
    };
    if let Err(error) = manifest.resolve(&pack_dir) {
        return ProviderPackProbe {
            provider,
            installed: true,
            valid: false,
            qualified: false,
            pack_dir: Some(pack_dir),
            pack_version: Some(manifest.pack_version),
            model_sha256: Some(manifest.model_sha256),
            hardware_fingerprint: fingerprint,
            qualification: None,
            detail: error.to_string(),
        };
    }
    let qualification_path = pack_dir.join("qualification.json");
    let qualification = fs::read(&qualification_path)
        .ok()
        .and_then(|payload| serde_json::from_slice::<QualificationRecord>(&payload).ok());
    let qualified = qualification.as_ref().is_some_and(|record| {
        record.is_current_for(
            provider,
            &manifest.pack_version,
            &manifest.model_sha256,
            &fingerprint,
        )
    });
    let detail = if qualified {
        "provider pack passed machine-local workload qualification".into()
    } else if let Some(record) = qualification.as_ref() {
        format!("provider pack is not qualified: {}", record.reason)
    } else {
        "provider pack is valid but has no machine-local qualification".into()
    };
    ProviderPackProbe {
        provider,
        installed: true,
        valid: true,
        qualified,
        pack_dir: Some(pack_dir),
        pack_version: Some(manifest.pack_version),
        model_sha256: Some(manifest.model_sha256),
        hardware_fingerprint: fingerprint,
        qualification,
        detail,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostControlMessage {
    Hello {
        protocol_version: u16,
        nonce: String,
        provider: AcceleratorProvider,
        shared_memory_path: String,
        model_path: String,
        model_sha256: String,
    },
    Shutdown {
        protocol_version: u16,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderControlMessage {
    Hello {
        protocol_version: u16,
        nonce: String,
        provider: AcceleratorProvider,
        execution_provider: String,
        pid: u32,
        model_sha256: String,
    },
    Status {
        protocol_version: u16,
        processed_blocks: u64,
        missed_deadlines: u64,
        last_error: Option<String>,
    },
}

pub struct ProviderClient {
    child: Child,
    control: UnixStream,
    queue: SharedNeuralQueue,
    socket_path: PathBuf,
    shared_memory_path: PathBuf,
    next_sequence: u64,
    provider: AcceleratorProvider,
}

impl ProviderClient {
    pub fn spawn(
        pack: &ResolvedProviderPack,
        runtime_directory: &Path,
    ) -> Result<Self, AcceleratorError> {
        ensure_private_directory(runtime_directory)?;
        let nonce = launch_nonce(pack.manifest.provider);
        let instance = PROVIDER_CLIENT_INSTANCE.fetch_add(1, Ordering::Relaxed);
        let socket_path = runtime_directory.join(format!(
            "accelerator-{}-{}-{instance}.sock",
            pack.manifest.provider.as_str(),
            std::process::id()
        ));
        let shared_memory_path = runtime_directory.join(format!(
            "accelerator-{}-{}-{instance}.shm",
            pack.manifest.provider.as_str(),
            std::process::id()
        ));
        let _ = fs::remove_file(&socket_path);
        let _ = fs::remove_file(&shared_memory_path);
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let queue = SharedNeuralQueue::create(&shared_memory_path)?;

        let mut command = Command::new(&pack.executable);
        command
            .arg("--serve")
            .arg("--provider")
            .arg(pack.manifest.provider.as_str())
            .arg("--model")
            .arg(&pack.model)
            .arg("--socket")
            .arg(&socket_path)
            .arg("--shared-memory")
            .arg(&shared_memory_path)
            .arg("--nonce")
            .arg(&nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(runtime) = &pack.onnx_runtime_library {
            command.arg("--runtime").arg(runtime);
        }
        let mut child = command.spawn().map_err(|error| {
            AcceleratorError::ProviderLaunch(format!(
                "failed to start {}: {error}",
                pack.executable.display()
            ))
        })?;

        let handshake = (|| {
            let mut control = accept_provider(&listener, &mut child, PROVIDER_START_TIMEOUT)?;
            control.set_read_timeout(Some(PROVIDER_START_TIMEOUT))?;
            control.set_write_timeout(Some(PROVIDER_START_TIMEOUT))?;
            validate_peer(&control, child.id())?;
            write_bounded_json(
                &mut control,
                &HostControlMessage::Hello {
                    protocol_version: ACCELERATOR_PROTOCOL_VERSION,
                    nonce: nonce.clone(),
                    provider: pack.manifest.provider,
                    shared_memory_path: shared_memory_path.display().to_string(),
                    model_path: pack.model.display().to_string(),
                    model_sha256: pack.manifest.model_sha256.clone(),
                },
            )?;
            let response: ProviderControlMessage = read_bounded_json(&control)?;
            match response {
                ProviderControlMessage::Hello {
                    protocol_version,
                    nonce: provider_nonce,
                    provider,
                    execution_provider,
                    pid,
                    model_sha256,
                } if protocol_version == ACCELERATOR_PROTOCOL_VERSION
                    && provider_nonce == nonce
                    && provider == pack.manifest.provider
                    && execution_provider == provider.execution_provider_name()
                    && pid == child.id()
                    && model_sha256.eq_ignore_ascii_case(&pack.manifest.model_sha256) => {}
                other => {
                    return Err(AcceleratorError::ProviderHandshake(format!(
                        "unexpected provider hello: {other:?}"
                    )));
                }
            }
            control.set_read_timeout(None)?;
            control.set_write_timeout(None)?;
            Ok(control)
        })();
        let control = match handshake {
            Ok(control) => control,
            Err(error) => {
                terminate_child(&mut child);
                let _ = fs::remove_file(&socket_path);
                let _ = fs::remove_file(&shared_memory_path);
                return Err(error);
            }
        };
        Ok(Self {
            child,
            control,
            queue,
            socket_path,
            shared_memory_path,
            next_sequence: 1,
            provider: pack.manifest.provider,
        })
    }

    pub fn provider(&self) -> AcceleratorProvider {
        self.provider
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn submit(
        &mut self,
        features: [f32; RNNOISE_FEATURE_COUNT],
        state: [f32; RNNOISE_STATE_COUNT],
        deadline_monotonic_ns: u64,
    ) -> Result<u64, QueueFull> {
        let sequence = self.next_sequence;
        self.queue.submit(NeuralRequest {
            sequence,
            deadline_monotonic_ns,
            features,
            state,
        })?;
        self.next_sequence = self.next_sequence.wrapping_add(1).max(1);
        Ok(sequence)
    }

    pub fn poll(&mut self) -> Option<NeuralResponse> {
        self.queue.receive_response()
    }

    pub fn wait(&mut self, sequence: u64, timeout: Duration) -> Option<NeuralResponse> {
        wait_for_response(&mut self.queue, sequence, timeout)
    }

    pub fn is_running(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    pub fn shutdown(&mut self) {
        self.queue.request_shutdown();
        let _ = write_bounded_json(
            &mut self.control,
            &HostControlMessage::Shutdown {
                protocol_version: ACCELERATOR_PROTOCOL_VERSION,
            },
        );
        terminate_child(&mut self.child);
    }
}

impl Drop for ProviderClient {
    fn drop(&mut self) {
        self.shutdown();
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.shared_memory_path);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct NeuralRequest {
    pub sequence: u64,
    pub deadline_monotonic_ns: u64,
    pub features: [f32; RNNOISE_FEATURE_COUNT],
    pub state: [f32; RNNOISE_STATE_COUNT],
}

impl Default for NeuralRequest {
    fn default() -> Self {
        Self {
            sequence: 0,
            deadline_monotonic_ns: 0,
            features: [0.0; RNNOISE_FEATURE_COUNT],
            state: [0.0; RNNOISE_STATE_COUNT],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct NeuralResponse {
    pub sequence: u64,
    pub completed_monotonic_ns: u64,
    pub processing_ns: u64,
    pub deadline_missed: u32,
    pub reserved: u32,
    pub gains: [f32; RNNOISE_GAIN_COUNT],
    pub vad_probability: f32,
    pub state: [f32; RNNOISE_STATE_COUNT],
}

impl Default for NeuralResponse {
    fn default() -> Self {
        Self {
            sequence: 0,
            completed_monotonic_ns: 0,
            processing_ns: 0,
            deadline_missed: 0,
            reserved: 0,
            gains: [0.0; RNNOISE_GAIN_COUNT],
            vad_probability: 0.0,
            state: [0.0; RNNOISE_STATE_COUNT],
        }
    }
}

#[repr(C, align(64))]
struct SharedHeader {
    magic: [u8; 8],
    protocol_version: u32,
    capacity: u32,
    request_write: AtomicU64,
    request_read: AtomicU64,
    response_write: AtomicU64,
    response_read: AtomicU64,
    shutdown: AtomicU32,
    reserved: [u32; 5],
}

#[repr(C, align(64))]
#[derive(Clone, Copy, Default)]
struct RequestSlot {
    payload: NeuralRequest,
}

#[repr(C, align(64))]
#[derive(Clone, Copy, Default)]
struct ResponseSlot {
    payload: NeuralResponse,
}

const HEADER_OFFSET: usize = 0;
const REQUEST_OFFSET: usize = align_up(size_of::<SharedHeader>(), align_of::<RequestSlot>());
const RESPONSE_OFFSET: usize = align_up(
    REQUEST_OFFSET + SHARED_QUEUE_CAPACITY * size_of::<RequestSlot>(),
    align_of::<ResponseSlot>(),
);
pub const SHARED_MEMORY_SIZE: usize =
    RESPONSE_OFFSET + SHARED_QUEUE_CAPACITY * size_of::<ResponseSlot>();

const fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

pub struct SharedNeuralQueue {
    path: PathBuf,
    _file: File,
    map: MmapMut,
}

// MmapMut owns its mapping. Access is synchronized by the mapped atomics and
// the queue is strictly single-producer/single-consumer in each direction.
unsafe impl Send for SharedNeuralQueue {}

impl SharedNeuralQueue {
    pub fn create(path: &Path) -> Result<Self, AcceleratorError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.set_len(SHARED_MEMORY_SIZE as u64)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let mut queue = Self::map(path, file)?;
        queue.map.fill(0);
        let header = queue.header_mut();
        header.magic = SHARED_MEMORY_MAGIC;
        header.protocol_version = u32::from(ACCELERATOR_PROTOCOL_VERSION);
        header.capacity = SHARED_QUEUE_CAPACITY as u32;
        queue.map.flush()?;
        Ok(queue)
    }

    pub fn open(path: &Path) -> Result<Self, AcceleratorError> {
        let metadata = fs::metadata(path)?;
        if metadata.len() != SHARED_MEMORY_SIZE as u64 {
            return Err(AcceleratorError::InvalidSharedMemory(format!(
                "expected {SHARED_MEMORY_SIZE} bytes, found {}",
                metadata.len()
            )));
        }
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let queue = Self::map(path, file)?;
        queue.validate_header()?;
        Ok(queue)
    }

    fn map(path: &Path, file: File) -> Result<Self, AcceleratorError> {
        // SAFETY: the file is held for the lifetime of the mapping and its size
        // is fixed before mapping. Queue methods bounds-check every slot index.
        let map = unsafe { MmapOptions::new().len(SHARED_MEMORY_SIZE).map_mut(&file)? };
        Ok(Self {
            path: path.to_path_buf(),
            _file: file,
            map,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn submit(&mut self, request: NeuralRequest) -> Result<(), QueueFull> {
        let header = self.header();
        let write = header.request_write.load(Ordering::Relaxed);
        let read = header.request_read.load(Ordering::Acquire);
        if write.wrapping_sub(read) >= SHARED_QUEUE_CAPACITY as u64 {
            return Err(QueueFull);
        }
        let index = write as usize % SHARED_QUEUE_CAPACITY;
        self.request_slots_mut()[index].payload = request;
        self.header()
            .request_write
            .store(write.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    pub fn receive_request(&mut self) -> Option<NeuralRequest> {
        let header = self.header();
        let read = header.request_read.load(Ordering::Relaxed);
        let write = header.request_write.load(Ordering::Acquire);
        if read == write {
            return None;
        }
        let index = read as usize % SHARED_QUEUE_CAPACITY;
        let request = self.request_slots()[index].payload;
        self.header()
            .request_read
            .store(read.wrapping_add(1), Ordering::Release);
        Some(request)
    }

    pub fn publish(&mut self, response: NeuralResponse) -> Result<(), QueueFull> {
        let header = self.header();
        let write = header.response_write.load(Ordering::Relaxed);
        let read = header.response_read.load(Ordering::Acquire);
        if write.wrapping_sub(read) >= SHARED_QUEUE_CAPACITY as u64 {
            return Err(QueueFull);
        }
        let index = write as usize % SHARED_QUEUE_CAPACITY;
        self.response_slots_mut()[index].payload = response;
        self.header()
            .response_write
            .store(write.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    pub fn receive_response(&mut self) -> Option<NeuralResponse> {
        let header = self.header();
        let read = header.response_read.load(Ordering::Relaxed);
        let write = header.response_write.load(Ordering::Acquire);
        if read == write {
            return None;
        }
        let index = read as usize % SHARED_QUEUE_CAPACITY;
        let response = self.response_slots()[index].payload;
        self.header()
            .response_read
            .store(read.wrapping_add(1), Ordering::Release);
        Some(response)
    }

    pub fn request_shutdown(&self) {
        self.header().shutdown.store(1, Ordering::Release);
    }

    pub fn shutdown_requested(&self) -> bool {
        self.header().shutdown.load(Ordering::Acquire) != 0
    }

    pub fn reset(&mut self) {
        let header = self.header();
        header.request_write.store(0, Ordering::Release);
        header.request_read.store(0, Ordering::Release);
        header.response_write.store(0, Ordering::Release);
        header.response_read.store(0, Ordering::Release);
        header.shutdown.store(0, Ordering::Release);
    }

    fn validate_header(&self) -> Result<(), AcceleratorError> {
        let header = self.header();
        if header.magic != SHARED_MEMORY_MAGIC {
            return Err(AcceleratorError::InvalidSharedMemory(
                "invalid magic value".into(),
            ));
        }
        if header.protocol_version != u32::from(ACCELERATOR_PROTOCOL_VERSION) {
            return Err(AcceleratorError::ProtocolVersion {
                expected: ACCELERATOR_PROTOCOL_VERSION,
                actual: header.protocol_version as u16,
            });
        }
        if header.capacity != SHARED_QUEUE_CAPACITY as u32 {
            return Err(AcceleratorError::InvalidSharedMemory(format!(
                "unsupported queue capacity {}",
                header.capacity
            )));
        }
        Ok(())
    }

    fn header(&self) -> &SharedHeader {
        // SAFETY: mmap bases are page-aligned and HEADER_OFFSET is zero.
        unsafe { &*(self.map.as_ptr().add(HEADER_OFFSET).cast::<SharedHeader>()) }
    }

    fn header_mut(&mut self) -> &mut SharedHeader {
        // SAFETY: this mapping is exclusively borrowed and page-aligned.
        unsafe {
            &mut *(self
                .map
                .as_mut_ptr()
                .add(HEADER_OFFSET)
                .cast::<SharedHeader>())
        }
    }

    fn request_slots(&self) -> &[RequestSlot] {
        // SAFETY: offsets and lengths are compile-time checked by SHARED_MEMORY_SIZE.
        unsafe {
            std::slice::from_raw_parts(
                self.map.as_ptr().add(REQUEST_OFFSET).cast::<RequestSlot>(),
                SHARED_QUEUE_CAPACITY,
            )
        }
    }

    fn request_slots_mut(&mut self) -> &mut [RequestSlot] {
        // SAFETY: this mapping is exclusively borrowed and the range is in bounds.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.map
                    .as_mut_ptr()
                    .add(REQUEST_OFFSET)
                    .cast::<RequestSlot>(),
                SHARED_QUEUE_CAPACITY,
            )
        }
    }

    fn response_slots(&self) -> &[ResponseSlot] {
        // SAFETY: offsets and lengths are compile-time checked by SHARED_MEMORY_SIZE.
        unsafe {
            std::slice::from_raw_parts(
                self.map
                    .as_ptr()
                    .add(RESPONSE_OFFSET)
                    .cast::<ResponseSlot>(),
                SHARED_QUEUE_CAPACITY,
            )
        }
    }

    fn response_slots_mut(&mut self) -> &mut [ResponseSlot] {
        // SAFETY: this mapping is exclusively borrowed and the range is in bounds.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.map
                    .as_mut_ptr()
                    .add(RESPONSE_OFFSET)
                    .cast::<ResponseSlot>(),
                SHARED_QUEUE_CAPACITY,
            )
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QualificationRecord {
    pub schema_version: u16,
    pub protocol_version: u16,
    pub provider: AcceleratorProvider,
    pub pack_version: String,
    pub model_sha256: String,
    pub hardware_fingerprint: String,
    pub tested_unix: i64,
    pub blocks: u64,
    pub numerical_max_abs_error: f64,
    pub deadline_misses: u64,
    pub discontinuities: u64,
    pub added_latency_msec: f64,
    pub cpu_reduction_percent: f64,
    pub fallback_validated: bool,
    pub live_workload_validated: bool,
    pub qualified: bool,
    pub reason: String,
}

impl QualificationRecord {
    pub fn evaluate(mut self) -> Self {
        let numerical_ok = self.numerical_max_abs_error <= 1.0e-4;
        let continuity_ok = self.deadline_misses == 0 && self.discontinuities == 0;
        let latency_ok = self.added_latency_msec <= 0.0;
        let cpu_ok = self.cpu_reduction_percent >= 30.0;
        self.qualified = numerical_ok
            && continuity_ok
            && latency_ok
            && cpu_ok
            && self.fallback_validated
            && self.live_workload_validated;
        self.reason = if self.qualified {
            "provider passed numerical, continuity, latency, and CPU gates".into()
        } else {
            let mut failures = Vec::new();
            if !numerical_ok {
                failures.push("numerical equivalence");
            }
            if !continuity_ok {
                failures.push("deadline/continuity");
            }
            if !latency_ok {
                failures.push("latency regression");
            }
            if !cpu_ok {
                failures.push("30% CPU reduction");
            }
            if !self.fallback_validated {
                failures.push("block-boundary fallback");
            }
            if !self.live_workload_validated {
                failures.push("live audio workload");
            }
            format!("provider failed: {}", failures.join(", "))
        };
        self
    }

    pub fn is_current_for(
        &self,
        provider: AcceleratorProvider,
        pack_version: &str,
        model_sha256: &str,
        hardware_fingerprint: &str,
    ) -> bool {
        self.schema_version == QUALIFICATION_SCHEMA_VERSION
            && self.protocol_version == ACCELERATOR_PROTOCOL_VERSION
            && self.provider == provider
            && self.pack_version == pack_version
            && self.model_sha256.eq_ignore_ascii_case(model_sha256)
            && self.hardware_fingerprint == hardware_fingerprint
            && self.qualified
    }
}

pub fn blank_qualification(
    provider: AcceleratorProvider,
    pack_version: impl Into<String>,
    model_sha256: impl Into<String>,
    hardware_fingerprint: impl Into<String>,
) -> QualificationRecord {
    QualificationRecord {
        schema_version: QUALIFICATION_SCHEMA_VERSION,
        protocol_version: ACCELERATOR_PROTOCOL_VERSION,
        provider,
        pack_version: pack_version.into(),
        model_sha256: model_sha256.into(),
        hardware_fingerprint: hardware_fingerprint.into(),
        tested_unix: 0,
        blocks: 0,
        numerical_max_abs_error: f64::INFINITY,
        deadline_misses: 0,
        discontinuities: 0,
        added_latency_msec: f64::INFINITY,
        cpu_reduction_percent: 0.0,
        fallback_validated: false,
        live_workload_validated: false,
        qualified: false,
        reason: "provider has not been qualified".into(),
    }
}

pub fn monotonic_nanos() -> u64 {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: value points to initialized writable memory.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } != 0 {
        return 0;
    }
    (value.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(value.tv_nsec as u64)
}

pub fn wait_for_response(
    queue: &mut SharedNeuralQueue,
    sequence: u64,
    timeout: Duration,
) -> Option<NeuralResponse> {
    let deadline = monotonic_nanos().saturating_add(timeout.as_nanos() as u64);
    loop {
        if let Some(response) = queue.receive_response() {
            if response.sequence == sequence {
                return Some(response);
            }
        }
        if monotonic_nanos() >= deadline || queue.shutdown_requested() {
            return None;
        }
        std::thread::sleep(Duration::from_micros(50));
    }
}

pub fn sha256_file(path: &Path) -> Result<String, AcceleratorError> {
    let bytes = fs::read(path)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn write_bounded_json<T: Serialize>(
    stream: &mut UnixStream,
    message: &T,
) -> Result<(), AcceleratorError> {
    let payload = serde_json::to_vec(message)?;
    if payload.len() + 1 > MAX_CONTROL_MESSAGE_BYTES {
        return Err(AcceleratorError::ProviderHandshake(
            "control message exceeds 16 KiB protocol bound".into(),
        ));
    }
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

pub fn read_bounded_json<T: for<'de> Deserialize<'de>>(
    stream: &UnixStream,
) -> Result<T, AcceleratorError> {
    let mut line = Vec::new();
    BufReader::new(stream)
        .take((MAX_CONTROL_MESSAGE_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if line.is_empty() || line.len() > MAX_CONTROL_MESSAGE_BYTES || !line.ends_with(b"\n") {
        return Err(AcceleratorError::ProviderHandshake(
            "invalid or oversized control message".into(),
        ));
    }
    Ok(serde_json::from_slice(&line)?)
}

fn ensure_private_directory(path: &Path) -> Result<(), AcceleratorError> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn launch_nonce(provider: AcceleratorProvider) -> String {
    let entropy = format!(
        "{}:{}:{}:{}",
        provider.as_str(),
        std::process::id(),
        monotonic_nanos(),
        std::thread::current().name().unwrap_or("unnamed")
    );
    format!("{:x}", Sha256::digest(entropy))
}

fn accept_provider(
    listener: &UnixListener,
    child: &mut Child,
    timeout: Duration,
) -> Result<UnixStream, AcceleratorError> {
    let deadline = monotonic_nanos().saturating_add(timeout.as_nanos() as u64);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(status) = child.try_wait()? {
            return Err(AcceleratorError::ProviderLaunch(format!(
                "provider exited before handshake with status {status}"
            )));
        }
        if monotonic_nanos() >= deadline {
            return Err(AcceleratorError::ProviderLaunch(
                "provider did not connect before startup timeout".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn validate_peer(stream: &UnixStream, expected_pid: u32) -> Result<(), AcceleratorError> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: credentials and length are valid writable pointers, and stream
    // owns a live Unix-domain socket descriptor.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let current_uid = unsafe { libc::geteuid() };
    if credentials.uid != current_uid || credentials.pid != expected_pid as i32 {
        return Err(AcceleratorError::ProviderHandshake(format!(
            "provider peer mismatch: expected pid={expected_pid} uid={current_uid}, got pid={} uid={}",
            credentials.pid, credentials.uid
        )));
    }
    Ok(())
}

fn terminate_child(child: &mut Child) {
    for _ in 0..40 {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn validate_sha256(value: &str) -> Result<(), AcceleratorError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(AcceleratorError::InvalidManifest(
            "model_sha256 must contain 64 hexadecimal characters".into(),
        ))
    }
}

fn validate_private_pack_member(path: &Path, executable: bool) -> Result<(), AcceleratorError> {
    let metadata = fs::metadata(path)?;
    let mode = metadata.mode();
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(AcceleratorError::InsecurePack(format!(
            "{} is not owned by the current user",
            path.display()
        )));
    }
    if mode & 0o022 != 0 {
        return Err(AcceleratorError::InsecurePack(format!(
            "{} is group/world writable",
            path.display()
        )));
    }
    if executable && mode & 0o100 == 0 {
        return Err(AcceleratorError::InsecurePack(format!(
            "{} is not executable by its owner",
            path.display()
        )));
    }
    Ok(())
}

fn resolve_pack_member(pack_dir: &Path, member: &str) -> Result<PathBuf, AcceleratorError> {
    let member = Path::new(member);
    if member.is_absolute()
        || member
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(AcceleratorError::InvalidManifest(format!(
            "pack path must be relative and may not escape the pack: {}",
            member.display()
        )));
    }
    Ok(pack_dir.join(member))
}

#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
#[error("shared neural queue is full")]
pub struct QueueFull;

#[derive(Debug, Error)]
pub enum AcceleratorError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("accelerator protocol mismatch: expected {expected}, got {actual}")]
    ProtocolVersion { expected: u16, actual: u16 },
    #[error("invalid provider pack manifest: {0}")]
    InvalidManifest(String),
    #[error("invalid accelerator shared memory: {0}")]
    InvalidSharedMemory(String),
    #[error("provider model hash mismatch: expected {expected}, got {actual}")]
    ModelHashMismatch { expected: String, actual: String },
    #[error("provider golden fixture hash mismatch: expected {expected}, got {actual}")]
    FixtureHashMismatch { expected: String, actual: String },
    #[error("provider executable hash mismatch: expected {expected}, got {actual}")]
    ExecutableHashMismatch { expected: String, actual: String },
    #[error("insecure accelerator provider pack: {0}")]
    InsecurePack(String),
    #[error("accelerator provider launch failed: {0}")]
    ProviderLaunch(String),
    #[error("accelerator provider handshake failed: {0}")]
    ProviderHandshake(String),
    #[error("accelerator provider is not qualified: {0}")]
    ProviderNotQualified(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn write_test_pack(root: &Path, provider: AcceleratorProvider) -> ProviderPackManifest {
        let pack = root.join(provider.as_str());
        fs::create_dir_all(&pack).unwrap();
        fs::set_permissions(&pack, fs::Permissions::from_mode(0o700)).unwrap();
        let executable = pack.join("provider");
        let model = pack.join("model.onnx");
        let fixture = pack.join("golden.json");
        fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&model, b"test-model").unwrap();
        fs::write(&fixture, b"{}").unwrap();
        let manifest = ProviderPackManifest {
            protocol_version: ACCELERATOR_PROTOCOL_VERSION,
            pack_version: "test-1".into(),
            provider,
            executable: "provider".into(),
            executable_sha256: sha256_file(&executable).unwrap(),
            model: "model.onnx".into(),
            model_sha256: sha256_file(&model).unwrap(),
            golden_fixture: "golden.json".into(),
            golden_fixture_sha256: sha256_file(&fixture).unwrap(),
            onnx_runtime_library: None,
        };
        fs::write(
            pack.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        manifest
    }

    #[test]
    fn shared_queue_round_trips_fixed_blocks() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("provider.shm");
        let mut host = SharedNeuralQueue::create(&path).unwrap();
        let mut provider = SharedNeuralQueue::open(&path).unwrap();

        let mut request = NeuralRequest {
            sequence: 42,
            deadline_monotonic_ns: monotonic_nanos() + 1_000_000,
            ..NeuralRequest::default()
        };
        request.features[3] = 0.75;
        host.submit(request).unwrap();
        let received = provider.receive_request().unwrap();
        assert_eq!(received, request);

        let mut response = NeuralResponse {
            sequence: received.sequence,
            ..NeuralResponse::default()
        };
        response.gains[2] = 0.5;
        provider.publish(response).unwrap();
        assert_eq!(host.receive_response(), Some(response));
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn queue_reports_backpressure_instead_of_overwriting() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("provider.shm");
        let mut queue = SharedNeuralQueue::create(&path).unwrap();
        for sequence in 0..SHARED_QUEUE_CAPACITY as u64 {
            queue
                .submit(NeuralRequest {
                    sequence,
                    ..NeuralRequest::default()
                })
                .unwrap();
        }
        assert_eq!(queue.submit(NeuralRequest::default()), Err(QueueFull));
    }

    #[test]
    fn stale_qualification_never_enables_a_provider() {
        let mut record =
            blank_qualification(AcceleratorProvider::Cuda, "1.0.0", "a".repeat(64), "gpu-a");
        record.numerical_max_abs_error = 0.0;
        record.added_latency_msec = 0.0;
        record.cpu_reduction_percent = 31.0;
        record.fallback_validated = true;
        record.live_workload_validated = true;
        record = record.evaluate();
        assert!(record.qualified);
        assert!(record.is_current_for(
            AcceleratorProvider::Cuda,
            "1.0.0",
            &"a".repeat(64),
            "gpu-a"
        ));
        assert!(!record.is_current_for(
            AcceleratorProvider::Cuda,
            "1.0.1",
            &"a".repeat(64),
            "gpu-a"
        ));
    }

    #[test]
    fn qualification_enforces_every_release_gate() {
        let mut record =
            blank_qualification(AcceleratorProvider::OpenVino, "1", "b".repeat(64), "intel");
        record.numerical_max_abs_error = 0.0;
        record.added_latency_msec = 0.1;
        record.cpu_reduction_percent = 99.0;
        let evaluated = record.evaluate();
        assert!(!evaluated.qualified);
        assert!(evaluated.reason.contains("latency regression"));
    }

    #[test]
    fn manifests_cannot_escape_the_pack_directory() {
        let manifest = ProviderPackManifest {
            protocol_version: ACCELERATOR_PROTOCOL_VERSION,
            pack_version: "1".into(),
            provider: AcceleratorProvider::Cuda,
            executable: "../provider".into(),
            executable_sha256: "c".repeat(64),
            model: "model.onnx".into(),
            model_sha256: "a".repeat(64),
            golden_fixture: "golden.json".into(),
            golden_fixture_sha256: "d".repeat(64),
            onnx_runtime_library: None,
        };
        let error = manifest.resolve(Path::new("/tmp/pack")).unwrap_err();
        assert!(error.to_string().contains("may not escape"));
    }

    #[test]
    fn valid_pack_is_reported_but_not_enabled_without_qualification() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = write_test_pack(temp.path(), AcceleratorProvider::OpenVino);

        let probe = probe_provider_pack_at(temp.path(), AcceleratorProvider::OpenVino);

        assert!(probe.installed);
        assert!(probe.valid);
        assert!(!probe.qualified);
        assert_eq!(probe.pack_version.as_deref(), Some("test-1"));
        assert_eq!(
            probe.model_sha256.as_deref(),
            Some(manifest.model_sha256.as_str())
        );
        assert!(probe.qualification.is_none());
    }

    #[test]
    fn modified_provider_pack_fails_hash_validation() {
        let temp = tempfile::tempdir().unwrap();
        write_test_pack(temp.path(), AcceleratorProvider::Cuda);
        fs::write(temp.path().join("cuda/model.onnx"), b"tampered").unwrap();

        let probe = probe_provider_pack_at(temp.path(), AcceleratorProvider::Cuda);

        assert!(probe.installed);
        assert!(!probe.valid);
        assert!(!probe.qualified);
        assert!(probe.detail.contains("model hash mismatch"));
    }
}
