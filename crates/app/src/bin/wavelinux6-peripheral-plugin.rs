use std::path::PathBuf;

use wavelinux_app::peripheral_protocol::PeripheralKind;
use wavelinux_app::streamer_devices::run_peripheral_plugin;

fn main() {
    if let Err(error) = run() {
        eprintln!("WaveLinux peripheral plugin: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut kind = None;
    let mut socket = None;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--kind" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--kind requires hid or midi".to_string())?;
                kind = Some(value.parse::<PeripheralKind>()?);
            }
            "--socket" => {
                socket = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--socket requires a path".to_string())?,
                ));
            }
            "--version" => {
                println!("{}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            value => return Err(format!("unknown argument: {value}")),
        }
    }

    let kind = kind.ok_or_else(|| "missing --kind".to_string())?;
    let socket = socket.ok_or_else(|| "missing --socket".to_string())?;
    run_peripheral_plugin(kind, &socket)
}
