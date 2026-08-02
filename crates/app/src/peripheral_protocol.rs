use std::io::{self, BufRead, Write};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::elgato::ElgatoWaveXlrState;
use wavelinux_model::{StreamerDeviceSummary, StreamerLearnResult};

pub const PERIPHERAL_PROTOCOL_VERSION: u16 = 1;
pub const MAX_PERIPHERAL_MESSAGE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeripheralKind {
    Elgato,
    Hid,
    Midi,
}

impl PeripheralKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Elgato => "elgato",
            Self::Hid => "hid",
            Self::Midi => "midi",
        }
    }
}

impl std::str::FromStr for PeripheralKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "elgato" => Ok(Self::Elgato),
            "hid" => Ok(Self::Hid),
            "midi" => Ok(Self::Midi),
            _ => Err(format!("unsupported peripheral plugin kind: {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    Configure {
        protocol_version: u16,
        config: wavelinux_model::StreamerDevicesConfig,
    },
    Shutdown {
        protocol_version: u16,
    },
    ElgatoRequest {
        protocol_version: u16,
        request_id: u64,
        command: ElgatoCommand,
    },
    LearnRequest {
        protocol_version: u16,
        request_id: u64,
        device: StreamerDeviceSummary,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ElgatoCommand {
    ReadWaveXlr,
    SetWaveXlrGain { gain_raw: u16 },
    SetWaveXlrMute { muted: bool },
    SetWaveXlrHeadphoneVolume { db: f32 },
    SetWaveXlrLowImpedance { enabled: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginMessage {
    Hello {
        protocol_version: u16,
        kind: PeripheralKind,
        pid: u32,
        capabilities: Vec<String>,
    },
    Event {
        protocol_version: u16,
        device_id: String,
        control_id: String,
        value: Option<f32>,
    },
    Status {
        protocol_version: u16,
        kind: PeripheralKind,
        state: PluginState,
        message: String,
    },
    ElgatoResponse {
        protocol_version: u16,
        request_id: u64,
        state: Option<ElgatoWaveXlrState>,
        error: Option<String>,
    },
    LearnResponse {
        protocol_version: u16,
        request_id: u64,
        result: Option<StreamerLearnResult>,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    Ready,
    Idle,
    Error,
    Stopping,
}

pub fn validate_protocol(version: u16) -> io::Result<()> {
    if version == PERIPHERAL_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "peripheral protocol mismatch: expected {}, received {version}",
                PERIPHERAL_PROTOCOL_VERSION
            ),
        ))
    }
}

pub fn write_message<T: Serialize>(writer: &mut impl Write, message: &T) -> io::Result<()> {
    let encoded = serde_json::to_vec(message)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if encoded.len() > MAX_PERIPHERAL_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "peripheral message exceeds the protocol limit",
        ));
    }
    writer.write_all(&encoded)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub fn read_message<T: DeserializeOwned>(reader: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut encoded = Vec::new();
    let mut limited = std::io::Read::take(reader, (MAX_PERIPHERAL_MESSAGE_BYTES + 2) as u64);
    let read = limited.read_until(b'\n', &mut encoded)?;
    if read == 0 {
        return Ok(None);
    }
    if encoded.len() > MAX_PERIPHERAL_MESSAGE_BYTES + 1 || !encoded.ends_with(b"\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "peripheral message is incomplete or exceeds the protocol limit",
        ));
    }
    encoded.pop();
    let message = serde_json::from_slice(&encoded)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn protocol_round_trips_a_bounded_event() {
        let message = PluginMessage::Event {
            protocol_version: PERIPHERAL_PROTOCOL_VERSION,
            device_id: "hid:demo".into(),
            control_id: "button:1".into(),
            value: Some(0.75),
        };
        let mut encoded = Vec::new();
        write_message(&mut encoded, &message).unwrap();
        let mut reader = BufReader::new(Cursor::new(encoded));
        assert_eq!(read_message(&mut reader).unwrap(), Some(message));
    }

    #[test]
    fn protocol_rejects_oversized_messages() {
        let message = PluginMessage::Status {
            protocol_version: PERIPHERAL_PROTOCOL_VERSION,
            kind: PeripheralKind::Hid,
            state: PluginState::Error,
            message: "x".repeat(MAX_PERIPHERAL_MESSAGE_BYTES),
        };
        assert_eq!(
            write_message(&mut Vec::new(), &message).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn protocol_rejects_unknown_versions() {
        assert_eq!(
            validate_protocol(PERIPHERAL_PROTOCOL_VERSION + 1)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn protocol_round_trips_elgato_commands() {
        let message = HostMessage::ElgatoRequest {
            protocol_version: PERIPHERAL_PROTOCOL_VERSION,
            request_id: 42,
            command: ElgatoCommand::SetWaveXlrHeadphoneVolume { db: -18.5 },
        };
        let mut encoded = Vec::new();
        write_message(&mut encoded, &message).unwrap();
        let mut reader = BufReader::new(Cursor::new(encoded));
        assert_eq!(read_message(&mut reader).unwrap(), Some(message));
    }
}
