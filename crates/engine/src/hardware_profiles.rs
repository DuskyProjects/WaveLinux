use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
#[cfg(test)]
use include_dir::{include_dir, Dir};
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;
use wavelinux_model::{
    BluetoothMicPolicy, DeviceBus, DeviceInfo, DevicePolicy, Diagnostic, DiagnosticSeverity,
    FallbackHardwareProfile, HardwareProfile, HardwareProfileMatch, HardwareProfileSummary,
    HardwareProfileUiState, ProfileConfidence, RuntimeGraph,
};

use crate::EnginePaths;

#[cfg(test)]
static TEST_PROFILE_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../profiles/v1/devices");
const PROFILE_PUBLIC_KEY_BASE64: &str = "RWRj/xx3s45rB1rCnnFCqvj7OuTsjpHDBPc7G/aSTn8pQSVnWZVPyPjk";
const PROFILE_RELEASE_BASE_URL: &str =
    "https://github.com/DuskyProjects/WaveLinux/releases/latest/download";
const PROFILE_INDEX_ASSET: &str = "hardware-profiles-v1-index.json";
const PROFILE_INDEX_SIG_ASSET: &str = "hardware-profiles-v1-index.json.sig";
const PROFILE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5);
const PROFILE_INDEX_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const PROFILE_INDEX_FAILURE_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_REMOTE_PROFILE_BYTES: usize = 256 * 1024;
const MAX_REMOTE_INDEX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct HardwareProfileCatalog {
    pub profiles: Vec<ProfileEntry>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct ProfileEntry {
    pub profile: HardwareProfile,
    pub source: ProfileSource,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteProfileSyncReport {
    pub fetched: usize,
    pub matched: usize,
    pub diagnostics: Vec<Diagnostic>,
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProfileSource {
    #[cfg(test)]
    Shipped,
    Remote,
    Local,
}

impl ProfileSource {
    pub fn as_str(self) -> &'static str {
        match self {
            #[cfg(test)]
            Self::Shipped => "shipped",
            Self::Remote => "remote",
            Self::Local => "local",
        }
    }

    fn priority(self) -> u8 {
        match self {
            #[cfg(test)]
            Self::Shipped => 10,
            Self::Remote => 20,
            Self::Local => 30,
        }
    }
}

pub fn load_hardware_profile_catalog(paths: &EnginePaths) -> HardwareProfileCatalog {
    let mut catalog = HardwareProfileCatalog::default();
    #[cfg(test)]
    load_test_profiles(&mut catalog);
    load_profile_dir(
        &paths
            .config_dir
            .join("hardware-profiles")
            .join("v1")
            .join("remote"),
        ProfileSource::Remote,
        &mut catalog,
    );
    load_profile_dir(
        &paths.local_hardware_profiles_dir(),
        ProfileSource::Local,
        &mut catalog,
    );
    dedupe_profile_ids(&mut catalog);
    catalog
}

pub fn sync_remote_profiles_for_devices(
    paths: &EnginePaths,
    devices: &[DeviceInfo],
    policy: &DevicePolicy,
    catalog: &HardwareProfileCatalog,
) -> RemoteProfileSyncReport {
    let mut report = RemoteProfileSyncReport::default();
    if !remote_profile_downloads_enabled() {
        return report;
    }
    let missing_assigned_profile_ids = missing_assigned_profile_ids(policy, catalog);
    let missing_devices = devices
        .iter()
        .filter(|device| should_lookup_remote_profile(device, catalog))
        .collect::<Vec<_>>();
    let needs_profile_assets =
        !missing_devices.is_empty() || !missing_assigned_profile_ids.is_empty();

    let index = match load_or_download_remote_index(paths) {
        Ok(index) => index,
        Err(reason) => {
            if needs_profile_assets {
                report.diagnostics.push(remote_profile_diagnostic(
                    DiagnosticSeverity::Warning,
                    reason,
                ));
            }
            return report;
        }
    };
    if !needs_profile_assets {
        return report;
    }

    let mut wanted_assets = BTreeSet::new();
    for profile_id in &missing_assigned_profile_ids {
        if let Some(entry) = index.profiles.iter().find(|entry| &entry.id == profile_id) {
            wanted_assets.insert(entry.asset.clone());
            report.matched += 1;
        }
    }
    for device in missing_devices {
        for entry in &index.profiles {
            if remote_index_entry_score(device, entry).is_some() {
                wanted_assets.insert(entry.asset.clone());
                report.matched += 1;
            }
        }
    }

    for asset in wanted_assets {
        match cache_remote_profile_asset(paths, &asset) {
            Ok(true) => {
                report.fetched += 1;
                report.changed = true;
            }
            Ok(false) => {}
            Err(reason) => report.diagnostics.push(remote_profile_diagnostic(
                DiagnosticSeverity::Warning,
                reason,
            )),
        }
    }
    report
}

pub fn remote_profile_sync_needed(
    devices: &[DeviceInfo],
    policy: &DevicePolicy,
    catalog: &HardwareProfileCatalog,
) -> bool {
    remote_profile_downloads_enabled()
        && (!missing_assigned_profile_ids(policy, catalog).is_empty()
            || devices
                .iter()
                .any(|device| should_lookup_remote_profile(device, catalog)))
}

pub fn apply_profile_policy_to_graph(
    graph: &mut RuntimeGraph,
    catalog: &HardwareProfileCatalog,
    policy: &DevicePolicy,
) {
    apply_profile_policy_to_devices(&mut graph.inputs, catalog, policy);
    apply_profile_policy_to_devices(&mut graph.outputs, catalog, policy);
}

#[cfg(test)]
pub fn apply_profiles_to_devices(devices: &mut [DeviceInfo], catalog: &HardwareProfileCatalog) {
    for device in devices {
        if let Some((entry, _score)) = best_profile_for_device(device, catalog) {
            apply_profile_to_device(device, &entry.profile, entry.source.as_str().into());
        }
    }
}

pub fn apply_profile_policy_to_devices(
    devices: &mut [DeviceInfo],
    catalog: &HardwareProfileCatalog,
    policy: &DevicePolicy,
) {
    for device in devices {
        if !device_is_hardware_profile_candidate(device) {
            clear_profile_policy(device);
            continue;
        }
        if let Some(profile_id) = policy.hardware_profile_assignments.get(&device.id) {
            if profile_id == &policy.fallback_hardware_profile.id {
                apply_fallback_profile_to_device(device, &policy.fallback_hardware_profile);
                continue;
            }
            if let Some(entry) = profile_entry_by_id(catalog, profile_id) {
                apply_profile_to_device(
                    device,
                    &entry.profile,
                    format!("assigned:{}", entry.source.as_str()),
                );
                continue;
            }
        }

        if let Some((entry, _score)) = best_profile_for_device(device, catalog) {
            apply_profile_to_device(device, &entry.profile, entry.source.as_str().into());
        } else {
            apply_fallback_profile_to_device(device, &policy.fallback_hardware_profile);
        }
    }
}

pub fn hardware_profile_ui_state(
    catalog: &HardwareProfileCatalog,
    policy: &DevicePolicy,
) -> HardwareProfileUiState {
    let mut profiles_by_id = BTreeMap::new();
    for entry in &catalog.profiles {
        profiles_by_id.insert(
            entry.profile.id.clone(),
            hardware_profile_summary(&entry.profile, entry.source.as_str()),
        );
    }
    profiles_by_id.insert(
        policy.fallback_hardware_profile.id.clone(),
        fallback_hardware_profile_summary(&policy.fallback_hardware_profile),
    );
    let mut profiles = profiles_by_id.into_values().collect::<Vec<_>>();
    profiles.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.id.cmp(&right.id))
    });
    HardwareProfileUiState {
        profiles,
        assignments: policy.hardware_profile_assignments.clone(),
        fallback_profile: policy.fallback_hardware_profile.clone(),
    }
}

pub fn hardware_profile_by_id<'a>(
    catalog: &'a HardwareProfileCatalog,
    profile_id: &str,
) -> Option<&'a HardwareProfile> {
    profile_entry_by_id(catalog, profile_id).map(|entry| &entry.profile)
}

pub fn hardware_profile_diagnostics(graph: &RuntimeGraph) -> Vec<Diagnostic> {
    graph
        .inputs
        .iter()
        .chain(graph.outputs.iter())
        .filter(|device| device_is_hardware_profile_candidate(device))
        .filter_map(|device| {
            let profile_id = device.matched_profile_id.as_deref()?;
            Some(Diagnostic {
                code: format!("hardware.profile.{profile_id}"),
                severity: DiagnosticSeverity::Info,
                message: format!(
                    "{} matched hardware profile {profile_id}",
                    device.description
                ),
                action: None,
            })
        })
        .collect()
}

fn apply_profile_to_device(device: &mut DeviceInfo, profile: &HardwareProfile, source: String) {
    device.matched_profile_id = Some(profile.id.clone());
    device.matched_profile_source = Some(source);
    device.profile_confidence = Some(profile.confidence);
    device.active_latency_policy = Some(active_latency_policy_for_profile(device, profile));
    device.active_routing_policy = Some(profile.routing_policy.clone());
    device.active_bluetooth_mic_policy = Some(effective_bluetooth_mic_policy(device, profile));
}

fn apply_fallback_profile_to_device(device: &mut DeviceInfo, profile: &FallbackHardwareProfile) {
    device.matched_profile_id = Some(profile.id.clone());
    device.matched_profile_source = Some("default".into());
    device.profile_confidence = Some(profile.confidence);
    device.active_latency_policy = Some(profile.latency_policy.clone());
    device.active_routing_policy = Some(profile.routing_policy.clone());
    device.active_bluetooth_mic_policy = Some(BluetoothMicPolicy::NeverIfHfp);
}

fn hardware_profile_summary(profile: &HardwareProfile, source: &str) -> HardwareProfileSummary {
    HardwareProfileSummary {
        id: profile.id.clone(),
        name: profile.name.clone(),
        source: source.into(),
        confidence: profile.confidence,
        latency_policy: profile.latency_policy.clone(),
        routing_policy: profile.routing_policy.clone(),
        bluetooth_mic_policy: profile.bluetooth_mic_policy,
    }
}

fn active_latency_policy_for_profile(
    device: &DeviceInfo,
    profile: &HardwareProfile,
) -> wavelinux_model::LatencyPolicy {
    let mut latency_policy = profile.latency_policy.clone();
    let Some(codec_floor) = codec_latency_floor_for_device(device, profile) else {
        return latency_policy;
    };
    latency_policy.bluetooth_floor_msec = Some(
        latency_policy
            .bluetooth_floor_msec
            .unwrap_or(0)
            .max(codec_floor.clamp(50, 500)),
    );
    latency_policy
}

fn codec_latency_floor_for_device(device: &DeviceInfo, profile: &HardwareProfile) -> Option<u16> {
    if profile.codec_policy.latency_floor_msec.is_empty() {
        return None;
    }
    [
        device.active_codec.as_deref(),
        device.active_profile.as_deref(),
        Some(device.name.as_str()),
        Some(device.description.as_str()),
    ]
    .into_iter()
    .flatten()
    .filter_map(codec_key_from_text)
    .find_map(|codec| {
        profile
            .codec_policy
            .latency_floor_msec
            .iter()
            .find(|(key, _)| codec_key_from_text(key).as_deref() == Some(codec.as_str()))
            .map(|(_, floor)| *floor)
    })
}

fn codec_key_from_text(text: &str) -> Option<String> {
    let normalized = text.trim().replace(['-', ' '], "_").to_ascii_lowercase();
    if normalized.contains("sbc_xq") {
        return Some("sbc_xq".into());
    }
    if normalized.contains("ldac") {
        return Some("ldac".into());
    }
    if normalized.contains("aac") {
        return Some("aac".into());
    }
    if normalized.contains("sbc") {
        return Some("sbc".into());
    }
    None
}

fn fallback_hardware_profile_summary(profile: &FallbackHardwareProfile) -> HardwareProfileSummary {
    HardwareProfileSummary {
        id: profile.id.clone(),
        name: profile.name.clone(),
        source: "default".into(),
        confidence: profile.confidence,
        latency_policy: profile.latency_policy.clone(),
        routing_policy: profile.routing_policy.clone(),
        bluetooth_mic_policy: profile.bluetooth_mic_policy,
    }
}

fn clear_profile_policy(device: &mut DeviceInfo) {
    device.matched_profile_id = None;
    device.matched_profile_source = None;
    device.profile_confidence = None;
    device.active_latency_policy = None;
    device.active_routing_policy = None;
    device.active_bluetooth_mic_policy = None;
}

fn device_is_hardware_profile_candidate(device: &DeviceInfo) -> bool {
    if device.is_virtual || device.bus == Some(DeviceBus::Virtual) {
        return false;
    }
    let text = format!("{} {} {}", device.id, device.name, device.description).to_ascii_lowercase();
    !text.contains("wavelinux") && !text.contains(".monitor") && !text.contains("monitor of")
}

fn effective_bluetooth_mic_policy(
    device: &DeviceInfo,
    profile: &HardwareProfile,
) -> BluetoothMicPolicy {
    if device.bus == Some(DeviceBus::Bluetooth)
        && profile.capabilities.bluetooth_hfp
        && !profile.capabilities.duplex_a2dp
    {
        BluetoothMicPolicy::NeverIfHfp
    } else {
        profile.bluetooth_mic_policy
    }
}

fn load_profile_dir(dir: &Path, source: ProfileSource, catalog: &mut HardwareProfileCatalog) {
    let mut paths = Vec::new();
    collect_profile_paths(dir, &mut paths);
    paths.sort();
    for path in paths {
        match fs::read_to_string(&path) {
            Ok(data) => {
                if source == ProfileSource::Remote {
                    match verify_remote_profile_signature(&path, data.as_bytes()) {
                        Ok(()) => {}
                        Err(reason) => {
                            catalog.diagnostics.push(profile_diagnostic(
                                &path,
                                DiagnosticSeverity::Warning,
                                reason,
                            ));
                            continue;
                        }
                    }
                }
                load_bundle(&data, source, &path, catalog);
            }
            Err(err) => catalog.diagnostics.push(profile_diagnostic(
                &path,
                DiagnosticSeverity::Warning,
                format!("Could not read hardware profile: {err}"),
            )),
        }
    }
}

#[cfg(test)]
fn load_test_profiles(catalog: &mut HardwareProfileCatalog) {
    load_test_profile_dir(&TEST_PROFILE_DIR, catalog);
}

#[cfg(test)]
fn load_test_profile_dir(dir: &Dir<'_>, catalog: &mut HardwareProfileCatalog) {
    for file in dir.files() {
        if file.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let path = Path::new("profiles/v1/devices").join(file.path());
        match file.contents_utf8() {
            Some(data) => load_bundle(data, ProfileSource::Shipped, &path, catalog),
            None => catalog.diagnostics.push(profile_diagnostic(
                &path,
                DiagnosticSeverity::Warning,
                "Ignored hardware profile because it was not valid UTF-8",
            )),
        }
    }
    for child in dir.dirs() {
        load_test_profile_dir(child, catalog);
    }
}

fn collect_profile_paths(dir: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_profile_paths(&path, paths);
        } else if path.extension().and_then(|value| value.to_str()) == Some("json") {
            paths.push(path);
        }
    }
}

fn load_bundle(
    data: &str,
    source: ProfileSource,
    path: &Path,
    catalog: &mut HardwareProfileCatalog,
) {
    let value = match serde_json::from_str::<serde_json::Value>(data) {
        Ok(value) => value,
        Err(err) => {
            catalog.diagnostics.push(profile_diagnostic(
                path,
                DiagnosticSeverity::Warning,
                format!("Ignored invalid hardware profile JSON: {err}"),
            ));
            return;
        }
    };
    if forbidden_profile_keys(&value) {
        catalog.diagnostics.push(profile_diagnostic(
            path,
            DiagnosticSeverity::Warning,
            "Ignored hardware profile because it contained executable or host-write fields",
        ));
        return;
    }
    let profiles = match profiles_from_value(value) {
        Ok(profiles) => profiles,
        Err(err) => {
            catalog.diagnostics.push(profile_diagnostic(
                path,
                DiagnosticSeverity::Warning,
                format!("Ignored hardware profile bundle: {err}"),
            ));
            return;
        }
    };
    for profile in profiles {
        match sanitize_profile(profile, source) {
            Ok(profile) => catalog.profiles.push(ProfileEntry { profile, source }),
            Err(reason) => catalog.diagnostics.push(profile_diagnostic(
                path,
                DiagnosticSeverity::Warning,
                reason,
            )),
        }
    }
}

fn dedupe_profile_ids(catalog: &mut HardwareProfileCatalog) {
    let mut profiles_by_id: BTreeMap<String, ProfileEntry> = BTreeMap::new();
    for entry in std::mem::take(&mut catalog.profiles) {
        let key = profile_entry_dedupe_key(&entry);
        match profiles_by_id.get(&entry.profile.id) {
            Some(existing) if profile_entry_dedupe_key(existing) >= key => {}
            _ => {
                profiles_by_id.insert(entry.profile.id.clone(), entry);
            }
        }
    }
    catalog.profiles = profiles_by_id.into_values().collect();
}

fn profile_entry_dedupe_key(entry: &ProfileEntry) -> (u32, u8, u8) {
    (
        entry.profile.revision,
        entry.source.priority(),
        confidence_priority(entry.profile.confidence),
    )
}

fn verify_remote_profile_signature(path: &Path, data: &[u8]) -> Result<(), String> {
    let Some(signature_path) = remote_signature_path(path) else {
        return Err("Ignored unsigned remote hardware profile cache".into());
    };
    let signature_text = fs::read_to_string(&signature_path).map_err(|err| {
        format!("Ignored remote hardware profile with unreadable signature: {err}")
    })?;
    verify_profile_signature(data, &signature_text)
}

fn verify_profile_signature(data: &[u8], signature_text: &str) -> Result<(), String> {
    let public_key = PublicKey::from_base64(PROFILE_PUBLIC_KEY_BASE64).map_err(|err| {
        format!("Ignored remote hardware profile because the public key was invalid: {err}")
    })?;
    let signature = decode_profile_signature(signature_text)?;
    public_key.verify(data, &signature, false).map_err(|err| {
        format!("Ignored remote hardware profile because signature verification failed: {err}")
    })
}

fn decode_profile_signature(signature_text: &str) -> Result<Signature, String> {
    match Signature::decode(signature_text) {
        Ok(signature) => Ok(signature),
        Err(raw_err) => {
            let decoded = BASE64_STANDARD.decode(signature_text.trim()).map_err(|_| {
                format!("Ignored remote hardware profile with invalid signature data: {raw_err}")
            })?;
            let decoded_text = std::str::from_utf8(&decoded).map_err(|err| {
                format!(
                    "Ignored remote hardware profile with non-UTF-8 wrapped signature data: {err}"
                )
            })?;
            Signature::decode(decoded_text).map_err(|err| {
                format!("Ignored remote hardware profile with invalid signature data: {err}")
            })
        }
    }
}

fn remote_signature_path(path: &Path) -> Option<PathBuf> {
    [path.with_extension("json.sig"), path.with_extension("sig")]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct RemoteProfileIndex {
    version: u32,
    profiles: Vec<RemoteProfileIndexEntry>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct RemoteProfileIndexEntry {
    id: String,
    name: String,
    revision: u32,
    asset: String,
    matches: Vec<HardwareProfileMatch>,
}

fn load_or_download_remote_index(paths: &EnginePaths) -> Result<RemoteProfileIndex, String> {
    let index_path = remote_index_path(paths);
    let signature_path = index_path.with_extension("json.sig");
    if cache_file_is_fresh(&index_path, PROFILE_INDEX_CACHE_TTL) {
        if let Ok(index) = read_verified_remote_index(&index_path, &signature_path) {
            return Ok(index);
        }
    }
    let failure_path = remote_index_failure_path(paths);
    if cache_file_is_fresh(&failure_path, PROFILE_INDEX_FAILURE_TTL) {
        if let Ok(index) = read_verified_remote_index(&index_path, &signature_path) {
            return Ok(index);
        }
        let recent_failure = fs::read_to_string(&failure_path).unwrap_or_default();
        let cache_missing = !index_path.is_file() || !signature_path.is_file();
        let retry_stale_failure_without_cache = cache_missing
            && (recent_failure.contains("404")
                || recent_failure.contains("invalid signature data")
                || recent_failure.contains("signature verification failed"));
        if retry_stale_failure_without_cache {
            let _ = fs::remove_file(&failure_path);
        } else {
            return read_verified_remote_index(&index_path, &signature_path).map_err(|cache_err| {
                format!(
                    "Hardware profile index download is in backoff after a recent failure; cached index unavailable: {cache_err}"
                )
            });
        }
    }

    let download_result = download_remote_index(paths);
    match download_result {
        Ok(index) => {
            let _ = fs::remove_file(failure_path);
            Ok(index)
        }
        Err(download_err) => {
            let _ = fs::create_dir_all(failure_path.parent().unwrap_or_else(|| Path::new(".")));
            let _ = fs::write(&failure_path, download_err.as_bytes());
            match read_verified_remote_index(&index_path, &signature_path) {
                Ok(index) => Ok(index),
                Err(cache_err) => Err(format!(
                    "Could not update hardware profile index from GitHub: {download_err}; cached index unavailable: {cache_err}"
                )),
            }
        }
    }
}

fn download_remote_index(paths: &EnginePaths) -> Result<RemoteProfileIndex, String> {
    let base_url = remote_profile_base_url();
    let data = download_remote_asset(&base_url, PROFILE_INDEX_ASSET, MAX_REMOTE_INDEX_BYTES)?;
    let signature = download_remote_asset(&base_url, PROFILE_INDEX_SIG_ASSET, 32 * 1024)?;
    verify_profile_signature(data.as_bytes(), &signature)?;
    let index = parse_remote_index(&data)?;

    let index_path = remote_index_path(paths);
    let signature_path = index_path.with_extension("json.sig");
    fs::create_dir_all(
        index_path
            .parent()
            .ok_or_else(|| "hardware profile index cache path was invalid".to_string())?,
    )
    .map_err(|err| format!("Could not create hardware profile index cache: {err}"))?;
    fs::write(&index_path, data)
        .map_err(|err| format!("Could not write hardware profile index cache: {err}"))?;
    fs::write(&signature_path, signature)
        .map_err(|err| format!("Could not write hardware profile index signature cache: {err}"))?;
    Ok(index)
}

fn read_verified_remote_index(
    index_path: &Path,
    signature_path: &Path,
) -> Result<RemoteProfileIndex, String> {
    let data = fs::read_to_string(index_path)
        .map_err(|err| format!("Could not read cached hardware profile index: {err}"))?;
    let signature = fs::read_to_string(signature_path)
        .map_err(|err| format!("Could not read cached hardware profile index signature: {err}"))?;
    verify_profile_signature(data.as_bytes(), &signature)?;
    parse_remote_index(&data)
}

fn parse_remote_index(data: &str) -> Result<RemoteProfileIndex, String> {
    let mut index: RemoteProfileIndex = serde_json::from_str(data)
        .map_err(|err| format!("Ignored invalid hardware profile index JSON: {err}"))?;
    if index.version == 0 {
        index.version = 1;
    }
    index.profiles = index
        .profiles
        .into_iter()
        .filter_map(sanitize_remote_index_entry)
        .collect();
    Ok(index)
}

fn sanitize_remote_index_entry(
    mut entry: RemoteProfileIndexEntry,
) -> Option<RemoteProfileIndexEntry> {
    entry.id = entry.id.trim().to_string();
    entry.name = entry.name.trim().to_string();
    entry.asset = entry.asset.trim().to_string();
    if entry.id.is_empty()
        || entry.name.is_empty()
        || entry.matches.is_empty()
        || remote_asset_file_name(&entry.asset).is_none()
    {
        return None;
    }
    if entry.revision == 0 {
        entry.revision = 1;
    }
    Some(entry)
}

fn cache_remote_profile_asset(paths: &EnginePaths, asset: &str) -> Result<bool, String> {
    let asset_file_name = remote_asset_file_name(asset).ok_or_else(|| {
        format!("Ignored remote hardware profile with invalid asset name: {asset}")
    })?;
    let profile_path = remote_profile_dir(paths).join(asset_file_name);
    if profile_path.is_file()
        && fs::read(&profile_path)
            .ok()
            .and_then(|data| verify_remote_profile_signature(&profile_path, &data).ok())
            .is_some()
    {
        return Ok(false);
    }

    let base_url = remote_profile_base_url();
    let data = download_remote_asset(&base_url, asset_file_name, MAX_REMOTE_PROFILE_BYTES)?;
    let signature_asset = format!("{asset_file_name}.sig");
    let signature = download_remote_asset(&base_url, &signature_asset, 32 * 1024)?;
    verify_profile_signature(data.as_bytes(), &signature)?;

    fs::create_dir_all(remote_profile_dir(paths))
        .map_err(|err| format!("Could not create remote hardware profile cache: {err}"))?;
    fs::write(&profile_path, data)
        .map_err(|err| format!("Could not write remote hardware profile cache: {err}"))?;
    fs::write(profile_path.with_extension("json.sig"), signature)
        .map_err(|err| format!("Could not write remote hardware profile signature cache: {err}"))?;
    Ok(true)
}

fn download_remote_asset(base_url: &str, asset: &str, max_bytes: usize) -> Result<String, String> {
    let url = format!("{}/{}", base_url.trim_end_matches('/'), asset);
    let response = reqwest::blocking::Client::builder()
        .timeout(PROFILE_DOWNLOAD_TIMEOUT)
        .user_agent("WaveLinux hardware profile fetcher")
        .build()
        .map_err(|err| format!("Could not create hardware profile downloader: {err}"))?
        .get(&url)
        .send()
        .map_err(|err| format!("Could not download {asset} from GitHub: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GitHub returned {status} for {asset}"));
    }
    let bytes = response
        .bytes()
        .map_err(|err| format!("Could not read {asset} from GitHub: {err}"))?;
    if bytes.len() > max_bytes {
        return Err(format!("Ignored oversized hardware profile asset: {asset}"));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|err| format!("Ignored non-UTF-8 hardware profile asset {asset}: {err}"))
}

fn remote_asset_file_name(asset: &str) -> Option<&str> {
    let path = Path::new(asset);
    let file_name = path.file_name()?.to_str()?;
    (file_name == asset && file_name.ends_with(".json")).then_some(file_name)
}

fn remote_profile_base_url() -> String {
    std::env::var("WAVELINUX_PROFILE_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| PROFILE_RELEASE_BASE_URL.into())
}

#[cfg(not(test))]
fn remote_profile_downloads_enabled() -> bool {
    std::env::var("WAVELINUX_DISABLE_PROFILE_DOWNLOADS").is_err()
}

#[cfg(test)]
fn remote_profile_downloads_enabled() -> bool {
    std::env::var("WAVELINUX_TEST_PROFILE_DOWNLOADS").is_ok()
}

fn remote_index_path(paths: &EnginePaths) -> PathBuf {
    paths
        .config_dir
        .join("hardware-profiles")
        .join("v1")
        .join("index")
        .join(PROFILE_INDEX_ASSET)
}

fn remote_index_failure_path(paths: &EnginePaths) -> PathBuf {
    paths
        .config_dir
        .join("hardware-profiles")
        .join("v1")
        .join("index")
        .join("last-download-error")
}

fn remote_profile_dir(paths: &EnginePaths) -> PathBuf {
    paths
        .config_dir
        .join("hardware-profiles")
        .join("v1")
        .join("remote")
}

fn cache_file_is_fresh(path: &Path, ttl: Duration) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    modified
        .elapsed()
        .map(|elapsed| elapsed <= ttl)
        .unwrap_or(false)
}

fn missing_assigned_profile_ids(
    policy: &DevicePolicy,
    catalog: &HardwareProfileCatalog,
) -> BTreeSet<String> {
    policy
        .hardware_profile_assignments
        .values()
        .filter(|profile_id| *profile_id != &policy.fallback_hardware_profile.id)
        .filter(|profile_id| profile_entry_by_id(catalog, profile_id).is_none())
        .cloned()
        .collect()
}

fn should_lookup_remote_profile(device: &DeviceInfo, catalog: &HardwareProfileCatalog) -> bool {
    device.is_available
        && device_is_hardware_profile_candidate(device)
        && best_profile_for_device(device, catalog).is_none()
}

fn remote_index_entry_score(device: &DeviceInfo, entry: &RemoteProfileIndexEntry) -> Option<u16> {
    entry
        .matches
        .iter()
        .filter_map(|rule| rule_score(device, rule))
        .max()
}

fn profiles_from_value(value: serde_json::Value) -> Result<Vec<HardwareProfile>, String> {
    if value.is_array() {
        return serde_json::from_value(value).map_err(|err| err.to_string());
    }
    if let Some(profiles) = value.get("profiles") {
        return serde_json::from_value(profiles.clone()).map_err(|err| err.to_string());
    }
    serde_json::from_value(value)
        .map(|profile| vec![profile])
        .map_err(|err| err.to_string())
}

fn sanitize_profile(
    mut profile: HardwareProfile,
    source: ProfileSource,
) -> Result<HardwareProfile, String> {
    profile.id = profile.id.trim().to_string();
    profile.name = profile.name.trim().to_string();
    if profile.id.is_empty() || profile.name.is_empty() {
        return Err("Ignored hardware profile with a missing id or name".into());
    }
    if profile.matches.is_empty() {
        return Err(format!(
            "Ignored hardware profile {} because it has no match rules",
            profile.id
        ));
    }
    if !profile.capabilities.input && !profile.capabilities.output {
        return Err(format!(
            "Ignored hardware profile {} because it is not an audio input or output endpoint",
            profile.id
        ));
    }
    if profile.capabilities.bluetooth_hfp
        && !profile.capabilities.duplex_a2dp
        && profile.bluetooth_mic_policy != BluetoothMicPolicy::NeverIfHfp
    {
        profile.bluetooth_mic_policy = BluetoothMicPolicy::NeverIfHfp;
    }
    if source == ProfileSource::Local {
        for rule in &profile.matches {
            if rule_is_too_broad(rule) {
                return Err(format!(
                    "Ignored local hardware profile {} because a match rule was too broad",
                    profile.id
                ));
            }
        }
    }
    Ok(profile)
}

fn rule_is_too_broad(rule: &HardwareProfileMatch) -> bool {
    rule.vendor_id.is_none()
        && rule.product_id.is_none()
        && rule.node_name_contains.is_empty()
        && rule.description_contains.is_empty()
        && rule.property_contains.is_empty()
        && rule.driver_contains.is_empty()
        && rule.bluetooth_modalias_contains.is_empty()
}

fn best_profile_for_device<'a>(
    device: &DeviceInfo,
    catalog: &'a HardwareProfileCatalog,
) -> Option<(&'a ProfileEntry, u16)> {
    catalog
        .profiles
        .iter()
        .filter_map(|entry| {
            let score = profile_score(device, &entry.profile)?;
            Some((entry, score))
        })
        .max_by_key(|(entry, score)| {
            (
                entry.source.priority(),
                *score,
                confidence_priority(entry.profile.confidence),
                entry.profile.revision,
            )
        })
}

fn profile_entry_by_id<'a>(
    catalog: &'a HardwareProfileCatalog,
    profile_id: &str,
) -> Option<&'a ProfileEntry> {
    catalog
        .profiles
        .iter()
        .filter(|entry| entry.profile.id == profile_id)
        .max_by_key(|entry| {
            (
                entry.source.priority(),
                confidence_priority(entry.profile.confidence),
                entry.profile.revision,
            )
        })
}

fn profile_score(device: &DeviceInfo, profile: &HardwareProfile) -> Option<u16> {
    profile
        .matches
        .iter()
        .filter_map(|rule| rule_score(device, rule))
        .max()
}

fn rule_score(device: &DeviceInfo, rule: &HardwareProfileMatch) -> Option<u16> {
    if rule_is_too_broad(rule) {
        return None;
    }
    let mut score = 0_u16;
    if let Some(bus) = rule.bus {
        if device.bus != Some(bus) {
            return None;
        }
        score += 10;
    }
    if let Some(vendor_id) = rule.vendor_id.as_deref() {
        if device.vendor_id.as_deref() != Some(normalize_id(vendor_id).as_str()) {
            return None;
        }
        score += 30;
    }
    if let Some(product_id) = rule.product_id.as_deref() {
        if device.product_id.as_deref() != Some(normalize_id(product_id).as_str()) {
            return None;
        }
        score += 30;
    }
    score += contains_score(&rule.node_name_contains, &device.id)?;
    score += contains_score(
        &rule.description_contains,
        &format!("{} {}", device.name, device.description),
    )?;
    score += contains_score(
        &rule.driver_contains,
        device.driver.as_deref().unwrap_or(""),
    )?;
    score += contains_score(
        &rule.bluetooth_modalias_contains,
        device.bluetooth_modalias.as_deref().unwrap_or(""),
    )?;
    if !rule.property_contains.is_empty() {
        let properties = device
            .pipewire_properties
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ");
        score += contains_score(&rule.property_contains, &properties)?;
    }
    Some(score.max(1))
}

fn contains_score(needles: &[String], haystack: &str) -> Option<u16> {
    if needles.is_empty() {
        return Some(0);
    }
    let haystack = haystack.to_ascii_lowercase();
    let mut seen = BTreeSet::new();
    for needle in needles {
        let needle = needle.trim().to_ascii_lowercase();
        if needle.is_empty() {
            continue;
        }
        if !haystack.contains(&needle) {
            return None;
        }
        seen.insert(needle);
    }
    Some((seen.len() as u16) * 5)
}

fn confidence_priority(confidence: ProfileConfidence) -> u8 {
    match confidence {
        ProfileConfidence::Low => 1,
        ProfileConfidence::Medium => 2,
        ProfileConfidence::High => 3,
    }
}

fn normalize_id(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("usb:")
        .trim_start_matches("pci:")
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn forbidden_profile_keys(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "command"
                    | "commands"
                    | "exec"
                    | "executable"
                    | "shell"
                    | "script"
                    | "hook"
                    | "hooks"
                    | "write_host_config"
            ) || forbidden_profile_keys(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(forbidden_profile_keys),
        _ => false,
    }
}

fn profile_diagnostic(
    path: &Path,
    severity: DiagnosticSeverity,
    message: impl Into<String>,
) -> Diagnostic {
    Diagnostic {
        code: format!("hardware.profile.{}", file_label(path)),
        severity,
        message: message.into(),
        action: Some(path.display().to_string()),
    }
}

fn remote_profile_diagnostic(
    severity: DiagnosticSeverity,
    message: impl Into<String>,
) -> Diagnostic {
    Diagnostic {
        code: "hardware.profile.remote".into(),
        severity,
        message: message.into(),
        action: Some(PROFILE_RELEASE_BASE_URL.into()),
    }
}

fn file_label(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("profile")
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '.'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use tempfile::tempdir;
    use wavelinux_model::{BluetoothMicPolicy, DeviceBus, DeviceInfo, DevicePolicy};

    use super::*;

    fn device(id: &str, description: &str, bus: DeviceBus) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            index: None,
            name: id.into(),
            description: description.into(),
            is_available: true,
            is_default: false,
            is_virtual: false,
            bus: Some(bus),
            vendor_id: None,
            product_id: None,
            alsa_card: None,
            alsa_device: None,
            driver: None,
            bluetooth_modalias: None,
            active_profile: None,
            active_codec: None,
            pipewire_properties: BTreeMap::new(),
            matched_profile_id: None,
            matched_profile_source: None,
            profile_confidence: None,
            active_latency_policy: None,
            active_routing_policy: None,
            active_bluetooth_mic_policy: None,
        }
    }

    #[test]
    fn shipped_xm4_profile_applies_never_hfp_policy() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut devices = vec![device(
            "bluez_output.AC_80_0A_72_BD_10.1",
            "WH-1000XM4",
            DeviceBus::Bluetooth,
        )];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("sony.wh-1000xm4")
        );
        assert_eq!(
            devices[0].active_bluetooth_mic_policy,
            Some(BluetoothMicPolicy::NeverIfHfp)
        );
    }

    #[test]
    fn shipped_profiles_load_from_individual_device_files() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);

        assert!(catalog.profiles.len() >= 25);
        assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);
        assert!(catalog.profiles.iter().any(|entry| {
            entry.source == ProfileSource::Shipped && entry.profile.id == "sony.wh-1000xm4"
        }));
        assert!(catalog.profiles.iter().any(|entry| {
            entry.source == ProfileSource::Shipped
                && entry.profile.id == "focusrite.scarlett-2i2-3rd-gen"
        }));
    }

    #[test]
    fn shipped_device_files_are_single_profile_objects() {
        fn assert_single_profile_files(dir: &Dir<'_>) {
            for file in dir.files() {
                if file.path().extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let data = file.contents_utf8().unwrap();
                let value: serde_json::Value = serde_json::from_str(data).unwrap();
                assert!(
                    value.get("profiles").is_none(),
                    "{} must be a single profile object",
                    file.path().display()
                );
                assert_eq!(
                    profiles_from_value(value).unwrap().len(),
                    1,
                    "{} must contain exactly one profile",
                    file.path().display()
                );
            }
            for child in dir.dirs() {
                assert_single_profile_files(child);
            }
        }

        assert_single_profile_files(&TEST_PROFILE_DIR);
    }

    #[test]
    fn shipped_logitech_receiver_does_not_match_webcam_audio_profile() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut dev = device(
            "alsa_input.usb-logitech-receiver",
            "Logitech USB Receiver",
            DeviceBus::Usb,
        );
        dev.vendor_id = Some("046d".into());
        dev.product_id = Some("c54d".into());
        let mut devices = vec![dev];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_ne!(
            devices[0].matched_profile_id.as_deref(),
            Some("logitech.usb-webcam-audio")
        );
    }

    #[test]
    fn shipped_dji_profile_requires_audio_identity_not_vid_pid_only() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut dev = device(
            "usb-2ca3-4011-control",
            "DJI Receiver Control Interface",
            DeviceBus::Usb,
        );
        dev.vendor_id = Some("2ca3".into());
        dev.product_id = Some("4011".into());
        let mut devices = vec![dev];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_ne!(
            devices[0].matched_profile_id.as_deref(),
            Some("dji.wireless-mic-rx")
        );
    }

    #[test]
    fn shipped_capture_card_audio_is_not_auto_voice_mic() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut devices = vec![device(
            "alsa_input.usb-elgato-cam-link",
            "Elgato Cam Link 4K Audio",
            DeviceBus::Usb,
        )];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("elgato.cam-link-4k")
        );
        assert_eq!(
            devices[0]
                .active_routing_policy
                .as_ref()
                .map(|policy| policy.allow_auto_select_input),
            Some(false)
        );
    }

    #[test]
    fn shipped_common_bluetooth_headsets_refuse_hfp_mic_optimization() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut devices = vec![device(
            "bluez_output.11_22_33_44_55_66.1",
            "Sony WH-1000XM5",
            DeviceBus::Bluetooth,
        )];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("sony.wh-1000xm5")
        );
        assert_eq!(
            devices[0].active_bluetooth_mic_policy,
            Some(BluetoothMicPolicy::NeverIfHfp)
        );
    }

    #[test]
    fn shipped_xm4_profile_prefers_stable_latency_codecs() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let profile = catalog
            .profiles
            .iter()
            .find(|entry| entry.profile.id == "sony.wh-1000xm4")
            .map(|entry| &entry.profile)
            .unwrap();

        assert_eq!(
            profile.codec_policy.preferred_a2dp_codecs,
            ["aac", "sbc_xq", "sbc", "ldac"]
        );
        assert_eq!(profile.latency_policy.stable_msec, Some(45));
        assert_eq!(profile.latency_policy.low_latency_msec, Some(25));
        assert_eq!(profile.latency_policy.bluetooth_floor_msec, Some(70));
        assert_eq!(
            profile.codec_policy.latency_floor_msec.get("aac"),
            Some(&80)
        );
        assert_eq!(
            profile.codec_policy.latency_floor_msec.get("sbc_xq"),
            Some(&100)
        );
        assert_eq!(
            profile.codec_policy.latency_floor_msec.get("sbc"),
            Some(&70)
        );
        assert_eq!(
            profile.codec_policy.latency_floor_msec.get("ldac"),
            Some(&120)
        );
        assert_eq!(profile.codec_policy.ldac_quality.as_deref(), Some("sq"));
        assert!(profile
            .codec_policy
            .avoid_codecs
            .iter()
            .any(|codec| codec == "ldac_hq"));
    }

    #[test]
    fn shipped_xm4_profile_applies_codec_specific_latency_floor() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let latency_for_codec = |codec: &str| {
            let mut dev = device(
                "bluez_output.AC_80_0A_72_BD_10.1",
                "WH-1000XM4",
                DeviceBus::Bluetooth,
            );
            dev.active_profile = Some(format!("a2dp-sink-{codec}"));
            dev.active_codec = Some(codec.into());
            let mut devices = vec![dev];

            apply_profiles_to_devices(&mut devices, &catalog);

            devices[0]
                .active_latency_policy
                .as_ref()
                .and_then(|policy| policy.bluetooth_floor_msec)
        };

        assert_eq!(latency_for_codec("aac"), Some(80));
        assert_eq!(latency_for_codec("sbc_xq"), Some(100));
        assert_eq!(latency_for_codec("sbc"), Some(70));
        assert_eq!(latency_for_codec("ldac"), Some(120));
    }

    #[test]
    fn shipped_bluetooth_profiles_define_safe_codec_latency_floors() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);

        for entry in catalog
            .profiles
            .iter()
            .filter(|entry| entry.profile.capabilities.bluetooth_a2dp)
        {
            assert!(
                entry.profile.latency_policy.bluetooth_floor_msec >= Some(50),
                "{} should keep an explicit Bluetooth floor",
                entry.profile.id
            );
            for codec in &entry.profile.codec_policy.preferred_a2dp_codecs {
                if matches!(codec.as_str(), "aac" | "ldac" | "sbc_xq" | "sbc") {
                    assert!(
                        entry
                            .profile
                            .codec_policy
                            .latency_floor_msec
                            .contains_key(codec),
                        "{} should define a safe latency floor for {codec}",
                        entry.profile.id
                    );
                    let floor = entry.profile.codec_policy.latency_floor_msec[codec];
                    assert!(
                        floor <= 300,
                        "{} {codec} floor should stay under PipeWire's common 16-buffer link budget",
                        entry.profile.id
                    );
                    assert!(
                        floor >= 50,
                        "{} {codec} floor should stay explicit enough to avoid zero-buffer churn",
                        entry.profile.id
                    );
                }
            }
        }
    }

    #[test]
    fn shipped_profiles_are_audio_endpoints_only() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);

        assert!(catalog.profiles.iter().all(|entry| {
            entry.profile.capabilities.input || entry.profile.capabilities.output
        }));
        for removed_id in [
            "alienware.aw-elc",
            "darfon.keyboard-d2b0",
            "intel.ax211.bluetooth-controller",
            "logitech.mx-anywhere-3s-bluetooth",
            "logitech.pro-x2-superstrike-mouse",
            "logitech.usb-receiver-c54d",
            "realtek.integrated-webcam-fhd-uvc",
        ] {
            assert!(
                catalog
                    .profiles
                    .iter()
                    .all(|entry| entry.profile.id != removed_id),
                "{removed_id} should not be shipped as an audio profile"
            );
        }

        let mut dev = device(
            "usb-046d-c0a8",
            "Logitech PRO X2 SUPERSTRIKE",
            DeviceBus::Usb,
        );
        dev.vendor_id = Some("046d".into());
        dev.product_id = Some("c0a8".into());
        let mut devices = vec![dev];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_eq!(devices[0].matched_profile_id.as_deref(), None);
    }

    #[test]
    fn shipped_realtek_alc3254_profile_matches_analog_endpoint_not_computer_model() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut dev = device(
            "alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Speaker__sink",
            "Raptor Lake High Definition Audio Controller Speaker",
            DeviceBus::Pci,
        );
        dev.vendor_id = Some("8086".into());
        dev.product_id = Some("7a50".into());
        dev.driver = Some("snd_soc_skl_hda_dsp".into());
        dev.pipewire_properties
            .insert("alsa.mixer_name".into(), "Realtek ALC3254".into());
        dev.pipewire_properties.insert(
            "alsa.long_card_name".into(),
            "OtherVendor-OtherModel".into(),
        );
        let mut devices = vec![dev];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("realtek.alc3254-hda")
        );
        assert_eq!(
            devices[0]
                .active_routing_policy
                .as_ref()
                .and_then(|policy| policy.input_priority),
            Some(58)
        );
    }

    #[test]
    fn shipped_realtek_alc3254_profile_does_not_match_hda_hdmi_endpoint() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut dev = device(
            "alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__HDMI1__sink",
            "Raptor Lake High Definition Audio Controller HDMI / DisplayPort",
            DeviceBus::Pci,
        );
        dev.vendor_id = Some("8086".into());
        dev.product_id = Some("7a50".into());
        dev.driver = Some("snd_soc_skl_hda_dsp".into());
        dev.pipewire_properties
            .insert("alsa.mixer_name".into(), "Realtek ALC3254".into());
        dev.pipewire_properties.insert(
            "device.profile.description".into(),
            "HDMI / DisplayPort 1".into(),
        );
        let mut devices = vec![dev];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_ne!(
            devices[0].matched_profile_id.as_deref(),
            Some("realtek.alc3254-hda")
        );
    }

    #[test]
    fn shipped_raptor_lake_controller_alone_does_not_match_alc3254() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut dev = device(
            "alsa_card.pci-0000_00_1f.3-platform-skl_hda_dsp_generic",
            "Raptor Lake High Definition Audio Controller",
            DeviceBus::Pci,
        );
        dev.vendor_id = Some("8086".into());
        dev.product_id = Some("7a50".into());
        dev.driver = Some("snd_soc_skl_hda_dsp".into());
        let mut devices = vec![dev];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_ne!(
            devices[0].matched_profile_id.as_deref(),
            Some("realtek.alc3254-hda")
        );
    }

    #[test]
    fn shipped_element_ii_is_output_only_usb_dac() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut devices = vec![device(
            "alsa_output.usb-jds-labs-element-ii",
            "JDS Labs Element II",
            DeviceBus::Usb,
        )];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("jds-labs.element-ii")
        );
        assert_eq!(
            devices[0]
                .active_routing_policy
                .as_ref()
                .and_then(|policy| policy.output_priority),
            Some(78)
        );
        assert_eq!(
            devices[0]
                .active_routing_policy
                .as_ref()
                .map(|policy| policy.allow_auto_select_input),
            Some(false)
        );
    }

    #[test]
    fn shipped_th_x00_profile_only_matches_explicit_alias() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut devices = vec![device(
            "alsa_output.pci-analog",
            "Massdrop Fostex TH-X00 on Element II",
            DeviceBus::Unknown,
        )];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("massdrop-fostex.th-x00")
        );
        assert_eq!(
            devices[0]
                .active_routing_policy
                .as_ref()
                .map(|policy| policy.allow_auto_select_input),
            Some(false)
        );
    }

    #[test]
    fn local_profile_can_override_safe_priority() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(paths.local_hardware_profiles_dir()).unwrap();
        fs::write(
            paths.local_hardware_profiles_dir().join("local-usb.json"),
            r#"{
              "id": "local.test-usb-mic",
              "name": "Local Test USB Mic",
              "matches": [{"bus": "usb", "vendor_id": "1234", "product_id": "5678"}],
              "capabilities": {"input": true, "usb_audio_class": true},
              "routing_policy": {"input_priority": 99},
              "confidence": "medium"
            }"#,
        )
        .unwrap();
        let catalog = load_hardware_profile_catalog(&paths);
        let mut dev = device("alsa_input.usb-test", "USB Test Microphone", DeviceBus::Usb);
        dev.vendor_id = Some("1234".into());
        dev.product_id = Some("5678".into());
        let mut devices = vec![dev];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("local.test-usb-mic")
        );
        assert_eq!(
            devices[0]
                .active_routing_policy
                .as_ref()
                .and_then(|policy| policy.input_priority),
            Some(99)
        );
    }

    #[test]
    fn local_profile_precedence_overrides_shipped_match() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(paths.local_hardware_profiles_dir()).unwrap();
        fs::write(
            paths.local_hardware_profiles_dir().join("xm4-local.json"),
            r#"{
              "id": "local.xm4-output-tuning",
              "name": "Local XM4 Output Tuning",
              "matches": [{"bus": "bluetooth", "description_contains": ["WH-1000XM4"]}],
              "capabilities": {"input": true, "output": true, "bluetooth_a2dp": true, "bluetooth_hfp": true},
              "routing_policy": {"output_priority": 99, "allow_auto_select_input": false},
              "bluetooth_mic_policy": "advisory_only",
              "confidence": "medium"
            }"#,
        )
        .unwrap();
        let catalog = load_hardware_profile_catalog(&paths);
        let mut devices = vec![device(
            "bluez_output.AC_80_0A_72_BD_10.1",
            "WH-1000XM4",
            DeviceBus::Bluetooth,
        )];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("local.xm4-output-tuning")
        );
        assert_eq!(
            devices[0]
                .active_routing_policy
                .as_ref()
                .and_then(|policy| policy.output_priority),
            Some(99)
        );
        assert_eq!(
            devices[0].active_bluetooth_mic_policy,
            Some(BluetoothMicPolicy::NeverIfHfp)
        );
    }

    #[test]
    fn stale_same_id_local_override_does_not_hide_newer_profile() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(paths.local_hardware_profiles_dir()).unwrap();
        fs::write(
            paths.local_hardware_profiles_dir().join("stale-xm4.json"),
            r#"{
              "id": "sony.wh-1000xm4",
              "name": "Sony WH-1000XM4",
              "revision": 7,
              "matches": [{"bus": "bluetooth", "description_contains": ["WH-1000XM4"]}],
              "capabilities": {"input": true, "output": true, "bluetooth_a2dp": true, "bluetooth_hfp": true},
              "latency_policy": {"stable_msec": 160, "low_latency_msec": 20, "bluetooth_floor_msec": 240},
              "routing_policy": {"output_priority": 70, "allow_auto_select_input": false, "allow_auto_select_output": true},
              "confidence": "high"
            }"#,
        )
        .unwrap();
        let catalog = load_hardware_profile_catalog(&paths);
        let ui_state = hardware_profile_ui_state(&catalog, &DevicePolicy::default());
        let profile = ui_state
            .profiles
            .iter()
            .find(|profile| profile.id == "sony.wh-1000xm4")
            .unwrap();
        let mut devices = vec![device(
            "bluez_output.AC_80_0A_72_BD_10.1",
            "WH-1000XM4",
            DeviceBus::Bluetooth,
        )];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_eq!(profile.latency_policy.stable_msec, Some(45));
        assert_eq!(profile.latency_policy.low_latency_msec, Some(25));
        assert_eq!(profile.latency_policy.bluetooth_floor_msec, Some(70));
        assert_eq!(
            devices[0]
                .active_latency_policy
                .as_ref()
                .and_then(|policy| policy.stable_msec),
            Some(45)
        );
    }

    #[test]
    fn newer_same_id_local_override_remains_selectable() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(paths.local_hardware_profiles_dir()).unwrap();
        fs::write(
            paths.local_hardware_profiles_dir().join("newer-xm4.json"),
            r#"{
              "id": "sony.wh-1000xm4",
              "name": "Sony WH-1000XM4 Tuned",
              "revision": 10,
              "matches": [{"bus": "bluetooth", "description_contains": ["WH-1000XM4"]}],
              "capabilities": {"input": true, "output": true, "bluetooth_a2dp": true, "bluetooth_hfp": true},
              "latency_policy": {"stable_msec": 90, "low_latency_msec": 40, "bluetooth_floor_msec": 120},
              "routing_policy": {"output_priority": 90, "allow_auto_select_input": false, "allow_auto_select_output": true},
              "confidence": "high"
            }"#,
        )
        .unwrap();
        let catalog = load_hardware_profile_catalog(&paths);
        let ui_state = hardware_profile_ui_state(&catalog, &DevicePolicy::default());
        let profile = ui_state
            .profiles
            .iter()
            .find(|profile| profile.id == "sony.wh-1000xm4")
            .unwrap();

        assert_eq!(profile.source, "local");
        assert_eq!(profile.name, "Sony WH-1000XM4 Tuned");
        assert_eq!(profile.latency_policy.stable_msec, Some(90));
        assert_eq!(profile.latency_policy.low_latency_msec, Some(40));
        assert_eq!(profile.latency_policy.bluetooth_floor_msec, Some(120));
    }

    #[test]
    fn local_bus_only_profile_is_ignored_as_too_broad() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(paths.local_hardware_profiles_dir()).unwrap();
        fs::write(
            paths.local_hardware_profiles_dir().join("bus-only.json"),
            r#"{
              "id": "local.all-usb",
              "name": "All USB",
              "matches": [{"bus": "usb"}],
              "capabilities": {"input": true},
              "confidence": "low"
            }"#,
        )
        .unwrap();

        let catalog = load_hardware_profile_catalog(&paths);

        assert!(catalog
            .profiles
            .iter()
            .all(|entry| entry.profile.id != "local.all-usb"));
        assert!(catalog
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("too broad")));
    }

    #[test]
    fn local_non_audio_profile_is_ignored() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(paths.local_hardware_profiles_dir()).unwrap();
        fs::write(
            paths.local_hardware_profiles_dir().join("mouse.json"),
            r#"{
              "id": "local.mouse",
              "name": "Mouse",
              "matches": [{"bus": "usb", "description_contains": ["Mouse"]}],
              "capabilities": {"input": false, "output": false, "duplex": false},
              "confidence": "low"
            }"#,
        )
        .unwrap();

        let catalog = load_hardware_profile_catalog(&paths);

        assert!(catalog
            .profiles
            .iter()
            .all(|entry| entry.profile.id != "local.mouse"));
        assert!(catalog
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("not an audio input")));
    }

    #[test]
    fn local_bluetooth_profile_cannot_override_hfp_guardrail() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(paths.local_hardware_profiles_dir()).unwrap();
        fs::write(
            paths
                .local_hardware_profiles_dir()
                .join("unsafe-bt.json"),
            r#"{
              "id": "local.unsafe-bt",
              "name": "Unsafe BT",
              "matches": [{"bus": "bluetooth", "description_contains": ["Unsafe BT"]}],
              "capabilities": {"input": true, "output": true, "bluetooth_hfp": true, "duplex_a2dp": false},
              "bluetooth_mic_policy": "advisory_only",
              "confidence": "medium"
            }"#,
        )
        .unwrap();
        let catalog = load_hardware_profile_catalog(&paths);
        let mut devices = vec![device(
            "bluez_input.AA_BB_CC_DD_EE_FF",
            "Unsafe BT",
            DeviceBus::Bluetooth,
        )];

        apply_profiles_to_devices(&mut devices, &catalog);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("local.unsafe-bt")
        );
        assert_eq!(
            devices[0].active_bluetooth_mic_policy,
            Some(BluetoothMicPolicy::NeverIfHfp)
        );
    }

    #[test]
    fn policy_mode_applies_fallback_profile_to_unknown_device() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let policy = DevicePolicy::default();
        let mut devices = vec![device(
            "alsa_input.unknown-usb-audio",
            "Unknown USB Audio",
            DeviceBus::Usb,
        )];

        apply_profile_policy_to_devices(&mut devices, &catalog, &policy);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("default.generic-audio")
        );
        assert_eq!(
            devices[0].matched_profile_source.as_deref(),
            Some("default")
        );
        assert_eq!(
            devices[0]
                .active_routing_policy
                .as_ref()
                .and_then(|policy| policy.input_priority),
            Some(35)
        );
    }

    #[test]
    fn profile_policy_ignores_wavelinux_and_monitor_devices() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let policy = DevicePolicy::default();
        let mut virtual_sink = device(
            "wavelinux_mix_stream",
            "WaveLinux Stream",
            DeviceBus::Virtual,
        );
        virtual_sink.is_virtual = true;
        let monitor_source = device(
            "alsa_output.usb_dac.analog-stereo.monitor",
            "Monitor of USB DAC",
            DeviceBus::Usb,
        );
        let mut devices = vec![virtual_sink, monitor_source];

        apply_profile_policy_to_devices(&mut devices, &catalog, &policy);

        assert!(devices
            .iter()
            .all(|device| device.matched_profile_id.is_none()));
        assert!(devices
            .iter()
            .all(|device| !should_lookup_remote_profile(device, &catalog)));
    }

    #[test]
    fn manual_profile_assignment_overrides_auto_match() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut policy = DevicePolicy::default();
        policy.hardware_profile_assignments.insert(
            "bluez_output.AC_80_0A_72_BD_10.1".into(),
            "jds-labs.element-ii".into(),
        );
        let mut devices = vec![device(
            "bluez_output.AC_80_0A_72_BD_10.1",
            "WH-1000XM4",
            DeviceBus::Bluetooth,
        )];

        apply_profile_policy_to_devices(&mut devices, &catalog, &policy);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("jds-labs.element-ii")
        );
        assert_eq!(
            devices[0].matched_profile_source.as_deref(),
            Some("assigned:shipped")
        );
    }

    #[test]
    fn fallback_profile_is_selectable_as_manual_assignment() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut policy = DevicePolicy::default();
        policy.hardware_profile_assignments.insert(
            "bluez_output.AC_80_0A_72_BD_10.1".into(),
            policy.fallback_hardware_profile.id.clone(),
        );
        let mut devices = vec![device(
            "bluez_output.AC_80_0A_72_BD_10.1",
            "WH-1000XM4",
            DeviceBus::Bluetooth,
        )];

        apply_profile_policy_to_devices(&mut devices, &catalog, &policy);

        assert_eq!(
            devices[0].matched_profile_id.as_deref(),
            Some("default.generic-audio")
        );
        assert_eq!(
            devices[0].matched_profile_source.as_deref(),
            Some("default")
        );
    }

    #[test]
    fn local_profile_with_command_is_ignored() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(paths.local_hardware_profiles_dir()).unwrap();
        fs::write(
            paths.local_hardware_profiles_dir().join("command.json"),
            r#"{
              "id": "local.command",
              "name": "Command Profile",
              "matches": [{"bus": "usb", "description_contains": ["Command"]}],
              "command": "pactl set-card-profile anything"
            }"#,
        )
        .unwrap();

        let catalog = load_hardware_profile_catalog(&paths);

        assert!(catalog
            .profiles
            .iter()
            .all(|entry| entry.profile.id != "local.command"));
        assert!(catalog
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("executable")));
    }

    #[test]
    fn unsigned_remote_profile_cache_is_ignored() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let remote_dir = paths
            .config_dir
            .join("hardware-profiles")
            .join("v1")
            .join("remote");
        fs::create_dir_all(&remote_dir).unwrap();
        fs::write(
            remote_dir.join("remote.json"),
            r#"{
              "id": "remote.unsigned",
              "name": "Unsigned Remote",
              "matches": [{"bus": "usb", "description_contains": ["Unsigned"]}],
              "capabilities": {"input": true},
              "confidence": "high"
            }"#,
        )
        .unwrap();

        let catalog = load_hardware_profile_catalog(&paths);

        assert!(catalog
            .profiles
            .iter()
            .all(|entry| entry.profile.id != "remote.unsigned"));
        assert!(catalog
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unsigned remote")));
    }

    #[test]
    fn invalid_remote_profile_signature_is_ignored() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let remote_dir = paths
            .config_dir
            .join("hardware-profiles")
            .join("v1")
            .join("remote");
        fs::create_dir_all(&remote_dir).unwrap();
        fs::write(
            remote_dir.join("remote.json"),
            r#"{
              "id": "remote.invalid-signature",
              "name": "Invalid Remote Signature",
              "matches": [{"bus": "usb", "description_contains": ["Invalid"]}],
              "capabilities": {"input": true},
              "confidence": "high"
            }"#,
        )
        .unwrap();
        fs::write(
            remote_dir.join("remote.json.sig"),
            "not a minisign signature",
        )
        .unwrap();

        let catalog = load_hardware_profile_catalog(&paths);

        assert!(catalog
            .profiles
            .iter()
            .all(|entry| entry.profile.id != "remote.invalid-signature"));
        assert!(catalog
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("invalid signature")));
    }

    #[test]
    fn base64_wrapped_tauri_minisign_signature_verifies() {
        let signature = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVSai94eDNzNDVyQjVtMmJVSzBmK2FtMEszSHNidkJua1FGNHo2QitrQlgyMlZkMFBGeVlOKzVlWldoR1BKK3dKT0g5LzFPUk1LaVhJRkdHUTV5YmR3eFNvVDB4ZVh6WHdFPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzc5ODg2OTA0CWZpbGU6aGFyZHdhcmUtcHJvZmlsZXMtdjEtaW5kZXguanNvbgpVTEtVdllVNGh4Y05GUFJSTXNHNWhBdml4SjFrRTdNK3hwaTFUcDVOcHdsOXo4RmZGWENQUlhCY1g5QWRsRWJtcUZWNXVad2x5Z2FPNitMV0txd2lEQT09Cg==";

        assert!(verify_profile_signature(
            include_bytes!("../../../profiles/v1/index.json"),
            signature,
        )
        .is_ok());
    }
}
