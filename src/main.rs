//! An Anolis device provider for Arduino Modulino boards, on
//! [modulino-driver](https://github.com/CameronBrooks11/modulino-driver).
//!
//! - `board.rs` declares each board type and maps reads and calls onto the
//!   driver; `config.rs` is the config and what its schema cannot say;
//!   `bus.rs` is the shared bus and each board's route on it.
//! - A board that does not answer, answers as another board, or does not
//!   finish starting is reported UNREACHABLE and tried again on its next
//!   read or call. Any other failure is answered with its own status.
//! - `stop` puts every output in its safe state however the session ends.

mod board;
mod bus;
mod config;

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anolis_provider_sdk::{
    Args, BoxError, CallContext, Device, DeviceHealth, DeviceState, Function, HostRequirement, Provider, ProviderError,
    Readings, Results,
};
use linux_embedded_hal::I2cdev;
use modulino_driver::sim::{SimBus, SimHub};
use modulino_driver::{Address, ChipKind};
use std::collections::BTreeMap;

use crate::board::{Board, Failure, device_type};
use crate::bus::{Bus, Route};
use crate::config::{BoardType, Config};

/// One configured board and what the provider knows of it.
struct Slot {
    board: Board,
    connected: bool,
    /// Why the board was last found unreachable, while it still is.
    lost: Option<String>,
}

struct ModulinoProvider {
    devices: Vec<Device>,
    slots: Vec<Slot>,
}

impl ModulinoProvider {
    fn slot(&mut self, device: &Device) -> &mut Slot {
        let index = self.devices.iter().position(|d| d.id() == device.id()).expect("the SDK passes known devices");
        &mut self.slots[index]
    }
}

impl Slot {
    /// Run `op` on the board, connecting it first if it is not connected,
    /// and record it unreachable when it does not answer.
    fn with<T>(&mut self, op: impl FnOnce(&mut Board) -> Result<T, Failure>) -> Result<T, ProviderError> {
        if !self.connected {
            self.run(Board::connect)?;
            self.connected = true;
        }
        self.run(op)
    }

    fn run<T>(&mut self, op: impl FnOnce(&mut Board) -> Result<T, Failure>) -> Result<T, ProviderError> {
        match op(&mut self.board) {
            Ok(value) => {
                self.lost = None;
                Ok(value)
            }
            Err(Failure::Unreachable(why)) => {
                self.connected = false;
                self.lost = Some(why.clone());
                Err(ProviderError::unavailable(why))
            }
            Err(Failure::Status(error)) => Err(error),
        }
    }
}

/// The simulated bus mock mode serves: each configured board, at its
/// configured address, on the bus or on its hub port. The boards are
/// modulino-driver's models, which follow the node firmware source and the
/// chips' datasheets and have not been checked against hardware.
fn simulated(config: &Config) -> SimBus {
    let mut top = SimBus::new();
    let mut ports: BTreeMap<(u8, u8), SimBus> = BTreeMap::new();
    for d in &config.devices {
        let segment = match d.hub {
            None => &mut top,
            Some(h) => ports.entry((h.address, h.port)).or_default(),
        };
        let bus = std::mem::replace(segment, SimBus::new());
        *segment = match (d.r#type.node_kind(), d.address()) {
            (Some(kind), Some(addr)) => bus.with_node_at(kind, addr),
            (Some(kind), None) => bus.with_node(kind),
            (None, addr) => {
                let chip = match d.r#type {
                    BoardType::Thermo => ChipKind::Hs3003,
                    BoardType::Pressure => ChipKind::Lps22hb,
                    BoardType::Light => ChipKind::Ltr381rgb,
                    BoardType::Movement => ChipKind::Lsm6dsox,
                    _ => ChipKind::Vl53l4cd,
                };
                match addr {
                    Some(addr) => bus.with_chip_at(chip, addr),
                    None => bus.with_chip(chip),
                }
            }
        };
    }
    let mut hubs: BTreeMap<u8, Vec<(u8, SimBus)>> = BTreeMap::new();
    for ((hub, port), bus) in ports {
        hubs.entry(hub).or_default().push((port, bus));
    }
    hubs.into_iter().fold(top, |top, (hub, ports)| {
        let empty = Address::new(hub).and_then(SimHub::new).expect("the schema limits hub addresses to 0x70-0x77");
        top.with_hub(ports.into_iter().fold(empty, |h, (port, bus)| h.with_port(port, bus)))
    })
}

fn open(config: &Config) -> Result<Bus, BoxError> {
    if config.mock {
        return Ok(Bus::Sim(Box::new(simulated(config))));
    }
    let dev = I2cdev::new(&config.bus).map_err(|e| format!("cannot open {}: {e}", config.bus))?;
    Ok(Bus::Linux(dev))
}

impl Provider for ModulinoProvider {
    type Config = Config;
    const NAME: &'static str = env!("CARGO_PKG_NAME");
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const CONFORMANCE_LEVEL: Option<u32> = Some(2);

    fn check_config(config: &Config) -> Result<(), BoxError> {
        config::check(config)
    }

    /// The bus device node must exist and be readable and writable by this
    /// user. Opening it does no bus transaction.
    fn check_host(config: &Config) -> Result<Vec<HostRequirement>, BoxError> {
        if config.mock {
            return Ok(Vec::new());
        }
        let path = &config.bus;
        let requirement = match std::fs::OpenOptions::new().read(true).write(true).open(path) {
            Ok(_) => HostRequirement::met("bus.access").detail(format!("{path} opens for reading and writing")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HostRequirement::unmet("bus.access")
                .detail(format!("{path} does not exist"))
                .remedy("enable I2C (on a Raspberry Pi: raspi-config, Interface Options, I2C) and reboot"),
            Err(e) => HostRequirement::unmet("bus.access")
                .detail(format!("cannot open {path}: {e}"))
                .remedy("run the provider as a user in the i2c group"),
        };
        Ok(vec![requirement])
    }

    fn new(config: Config) -> Result<Self, BoxError> {
        let bus = Arc::new(Mutex::new(open(&config)?));
        let mut devices = Vec::new();
        let mut slots = Vec::new();
        for d in &config.devices {
            let mut device =
                Device::new(&d.id, device_type(d.r#type)).label(d.label.clone().unwrap_or_else(|| d.id.clone()));
            if let Some(a) = d.address.or_else(|| d.r#type.default_address()) {
                device = device.address(format!("{a:#04x}"));
            }
            if let Some(h) = d.hub {
                device = device.tag("hub", format!("{:#04x}", h.address)).tag("hub_port", h.port.to_string());
            }
            devices.push(device);
            slots.push(Slot { board: Board::new(d, Route::new(&bus, d.via())), connected: false, lost: None });
        }
        Ok(Self { devices, slots })
    }

    /// Connect every board, putting its outputs in their safe state. A board
    /// that does not answer stays listed, UNREACHABLE, and is tried again.
    fn start(&mut self, _budget: Duration) -> Result<(), BoxError> {
        for (device, slot) in self.devices.iter().zip(&mut self.slots) {
            if let Err(e) = slot.with(|_| Ok(())) {
                eprintln!("{}: WARNING {} not connected: {}", Self::NAME, device.id(), e.message);
            }
        }
        Ok(())
    }

    fn devices(&self) -> &[Device] {
        &self.devices
    }

    fn read(&mut self, device: &Device, signal_ids: &[&str]) -> Result<Readings, ProviderError> {
        self.slot(device).with(|b| b.read(signal_ids))
    }

    fn call(
        &mut self,
        device: &Device,
        function: &Function,
        args: &Args,
        _ctx: &CallContext,
    ) -> Result<Results, ProviderError> {
        self.slot(device).with(|b| b.call(function.name(), args))
    }

    fn device_health(&self, device: &Device) -> Option<DeviceHealth> {
        let index = self.devices.iter().position(|d| d.id() == device.id())?;
        self.slots[index].lost.as_ref().map(|why| DeviceHealth::new(DeviceState::Unreachable, why.clone()))
    }

    /// The session is ending, however it ended: every connected board's
    /// outputs to their safe state. A board that does not answer is logged
    /// and skipped, and the others are still made safe.
    fn stop(&mut self) {
        for (device, slot) in self.devices.iter().zip(&mut self.slots) {
            if !slot.connected {
                continue;
            }
            match slot.board.stop() {
                Ok(()) => {}
                Err(Failure::Unreachable(why)) => {
                    eprintln!("{}: WARNING {} not made safe: {why}", Self::NAME, device.id())
                }
                Err(Failure::Status(e)) => {
                    eprintln!("{}: WARNING {} not made safe: {}", Self::NAME, device.id(), e.message)
                }
            }
        }
    }
}

fn main() -> ExitCode {
    anolis_provider_sdk::run::<ModulinoProvider>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DeviceConfig, HubConfig};
    use anolis_provider_sdk::StatusCode;
    use modulino_driver::Kind;

    /// A Buttons board at its default address, with buttons pressed as given.
    fn buttons(pressed: [u8; 3]) -> SimBus {
        let mut bus = SimBus::new().with_node(Kind::Buttons);
        assert!(bus.set_reading(Kind::Buttons.default_address(), &pressed));
        bus
    }

    fn buttons_config(hub: Option<HubConfig>) -> DeviceConfig {
        DeviceConfig { id: "b0".into(), r#type: BoardType::Buttons, label: None, address: None, hub }
    }

    fn slot(bus: SimBus, config: &DeviceConfig) -> Slot {
        let bus = Arc::new(Mutex::new(Bus::Sim(Box::new(bus))));
        Slot { board: Board::new(config, Route::new(&bus, config.via())), connected: false, lost: None }
    }

    fn hub(port: u8, bus: SimBus) -> SimBus {
        let hub = Address::new(0x70).and_then(SimHub::new).expect("0x70 is a hub address");
        SimBus::new().with_hub(hub.with_port(port, bus))
    }

    #[test]
    fn a_board_behind_a_hub_port_is_read_through_it() {
        let mut slot = slot(hub(2, buttons([1, 0, 1])), &buttons_config(Some(HubConfig { address: 0x70, port: 2 })));
        let read = slot.with(|b| b.read(&["a", "b", "c"])).unwrap();
        assert_eq!(read["a"].value, true.into());
        assert_eq!(read["b"].value, false.into());
        assert_eq!(read["c"].value, true.into());
        assert!(slot.connected && slot.lost.is_none());
    }

    #[test]
    fn a_board_on_another_port_is_unreachable_not_a_fault() {
        let mut slot = slot(hub(2, buttons([0, 0, 0])), &buttons_config(Some(HubConfig { address: 0x70, port: 3 })));
        let err = slot.with(|b| b.read(&["a"])).unwrap_err();
        assert_eq!(err.code, StatusCode::Unavailable);
        assert!(!slot.connected && slot.lost.is_some());
    }

    #[test]
    fn a_lost_board_is_tried_again_and_recovers() {
        let mut slot = slot(SimBus::new(), &buttons_config(None));
        assert!(slot.with(|b| b.read(&["a"])).is_err());
        assert!(slot.lost.is_some());
        // Put the board on the bus: the next read connects it again.
        let bus = Arc::new(Mutex::new(Bus::Sim(Box::new(buttons([0, 1, 0])))));
        slot.board = Board::new(&buttons_config(None), Route::new(&bus, None));
        let read = slot.with(|b| b.read(&["b"])).unwrap();
        assert_eq!(read["b"].value, true.into());
        assert!(slot.lost.is_none() && slot.connected);
    }
}
