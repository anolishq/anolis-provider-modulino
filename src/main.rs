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
    /// The board has been connected and its outputs made safe once. Later
    /// connects, after it was lost, leave its outputs as commanded.
    made_safe: bool,
    /// Why the board was last found unreachable, while it still is.
    lost: Option<String>,
}

struct ModulinoProvider {
    devices: Vec<Device>,
    slots: Vec<Slot>,
}

impl ModulinoProvider {
    /// The provider for `config`, its boards sharing `bus`.
    fn on_bus(config: &Config, bus: &Arc<Mutex<Bus>>) -> Self {
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
            slots.push(Slot {
                board: Board::new(d, Route::new(bus, d.via())),
                connected: false,
                made_safe: false,
                lost: None,
            });
        }
        Self { devices, slots }
    }

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
            let first = !self.made_safe;
            self.run(|b| {
                b.setup()?;
                if first {
                    b.make_safe()?;
                }
                Ok(())
            })?;
            self.connected = true;
            self.made_safe = true;
        }
        self.run(op)
    }

    /// At session end, whatever state the board was last in: a board that is
    /// not connected is verified first, so nothing is sent to an address that
    /// may hold another device, then its outputs are made safe.
    fn finish(&mut self) -> Result<(), Failure> {
        if !self.connected {
            self.board.verify()?;
        }
        self.board.stop()
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

fn open(config: &Config) -> Bus {
    if config.mock { Bus::Sim(simulated(config)) } else { Bus::linux(&config.bus) }
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
        Ok(Self::on_bus(&config, &Arc::new(Mutex::new(open(&config)))))
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

    /// The session is ending, however it ended: every board's outputs to their
    /// safe state, including a board that was lost or never answered, which
    /// is tried once more. A board that does not answer is logged and
    /// skipped, and the others are still made safe.
    fn stop(&mut self) {
        for (device, slot) in self.devices.iter().zip(&mut self.slots) {
            match slot.finish() {
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
    use anolis_provider_sdk::{Quality, StatusCode};
    use modulino_driver::Kind;
    use std::collections::BTreeSet;

    type Shared = Arc<Mutex<Bus>>;

    fn config(t: BoardType, hub: Option<HubConfig>) -> DeviceConfig {
        DeviceConfig { id: "d0".into(), r#type: t, label: None, address: None, hub }
    }

    fn slot(bus: SimBus, config: &DeviceConfig) -> (Slot, Shared) {
        let shared = Arc::new(Mutex::new(Bus::Sim(bus)));
        let slot = Slot {
            board: Board::new(config, Route::new(&shared, config.via())),
            connected: false,
            made_safe: false,
            lost: None,
        };
        (slot, shared)
    }

    /// Run `f` on the simulated bus inside the shared bus.
    fn sim<R>(shared: &Shared, f: impl FnOnce(&mut SimBus) -> R) -> R {
        match &mut *shared.lock().unwrap() {
            Bus::Sim(sim) => f(sim),
            Bus::Linux { .. } => unreachable!("tests run on the simulated bus"),
        }
    }

    /// A Buttons board at its default address, with buttons pressed as given.
    fn buttons(pressed: [u8; 3]) -> SimBus {
        let mut bus = SimBus::new().with_node(Kind::Buttons);
        assert!(bus.set_reading(Kind::Buttons.default_address(), &pressed));
        bus
    }

    fn hub(port: u8, bus: SimBus) -> SimBus {
        let hub = Address::new(0x70).and_then(SimHub::new).expect("0x70 is a hub address");
        SimBus::new().with_hub(hub.with_port(port, bus))
    }

    /// Switch the relay through its driver.
    fn set_relay(slot: &mut Slot, on: bool) {
        slot.with(|b| match b {
            Board::OptoRelay(r) => r.set(on).map_err(Failure::from),
            _ => unreachable!("an opto relay"),
        })
        .unwrap();
    }

    fn relay_frame(shared: &Shared) -> Option<Vec<u8>> {
        sim(shared, |s| s.last_write(Kind::OptoRelay.default_address()).map(<[u8]>::to_vec))
    }

    fn relay_present(shared: &Shared, present: bool) {
        sim(shared, |s| assert!(s.set_present(Kind::OptoRelay.default_address(), present)));
    }

    #[test]
    fn a_board_behind_a_hub_port_is_read_through_it() {
        let (mut slot, _) =
            slot(hub(2, buttons([1, 0, 1])), &config(BoardType::Buttons, Some(HubConfig { address: 0x70, port: 2 })));
        let read = slot.with(|b| b.read(&["a", "b", "c"])).unwrap();
        assert_eq!(read["a"].value, true.into());
        assert_eq!(read["b"].value, false.into());
        assert_eq!(read["c"].value, true.into());
        assert!(slot.connected && slot.lost.is_none());
    }

    #[test]
    fn a_board_on_another_port_is_unreachable_not_a_fault() {
        let (mut slot, _) =
            slot(hub(2, buttons([0, 0, 0])), &config(BoardType::Buttons, Some(HubConfig { address: 0x70, port: 3 })));
        let err = slot.with(|b| b.read(&["a"])).unwrap_err();
        assert_eq!(err.code, StatusCode::Unavailable);
        assert!(!slot.connected && slot.lost.is_some());
    }

    #[test]
    fn connecting_a_sensor_waits_for_its_first_sample() {
        for (t, chip, addr) in [
            (BoardType::Pressure, ChipKind::Lps22hb, 0x5C),
            (BoardType::Light, ChipKind::Ltr381rgb, 0x53),
            (BoardType::Movement, ChipKind::Lsm6dsox, 0x6A),
        ] {
            let addr = Address::new(addr).unwrap();
            let (mut slot, shared) = slot(SimBus::new().with_chip(chip), &config(t, None));
            assert!(sim(&shared, |s| s.set_ready(addr, false)));
            let err = slot.with(|_| Ok(())).unwrap_err();
            assert_eq!(err.code, StatusCode::Unavailable, "{t:?} connected with no sample");
            assert!(!slot.connected && slot.lost.is_some(), "{t:?}");
            sim(&shared, |s| s.set_ready(addr, true));
            slot.with(|_| Ok(())).unwrap();
            assert!(slot.connected && slot.lost.is_none(), "{t:?}");
        }
    }

    #[test]
    fn a_lost_board_is_tried_again_and_recovers() {
        let (mut slot, shared) = slot(buttons([0, 1, 0]), &config(BoardType::Buttons, None));
        sim(&shared, |s| s.set_present(Kind::Buttons.default_address(), false));
        assert!(slot.with(|b| b.read(&["b"])).is_err());
        assert!(slot.lost.is_some());
        sim(&shared, |s| s.set_present(Kind::Buttons.default_address(), true));
        let read = slot.with(|b| b.read(&["b"])).unwrap();
        assert_eq!(read["b"].value, true.into());
        assert!(slot.lost.is_none() && slot.connected);
    }

    #[test]
    fn the_first_connect_makes_outputs_safe() {
        let (mut slot, shared) = slot(SimBus::new().with_node(Kind::OptoRelay), &config(BoardType::OptoRelay, None));
        assert_eq!(relay_frame(&shared), None, "nothing written before connect");
        slot.with(|_| Ok(())).unwrap();
        let safe = relay_frame(&shared).expect("connect writes the safe state");
        set_relay(&mut slot, true);
        assert_ne!(relay_frame(&shared).unwrap(), safe);
        set_relay(&mut slot, false);
        assert_eq!(relay_frame(&shared).unwrap(), safe, "the frame connect wrote is the off frame");
    }

    #[test]
    fn a_reconnect_keeps_a_commanded_output() {
        let (mut slot, shared) = slot(SimBus::new().with_node(Kind::OptoRelay), &config(BoardType::OptoRelay, None));
        set_relay(&mut slot, true);
        let on = relay_frame(&shared).unwrap();
        relay_present(&shared, false);
        assert!(slot.with(|b| b.read(&["on"])).is_err());
        relay_present(&shared, true);
        let read = slot.with(|b| b.read(&["on"])).unwrap();
        assert_eq!(read["on"].value, true.into(), "the relay stays on after a missed transfer");
        assert_eq!(relay_frame(&shared).unwrap(), on);
    }

    #[test]
    fn stop_makes_safe_a_board_lost_during_the_session() {
        let (mut slot, shared) = slot(SimBus::new().with_node(Kind::OptoRelay), &config(BoardType::OptoRelay, None));
        slot.with(|_| Ok(())).unwrap();
        let off = relay_frame(&shared).unwrap();
        set_relay(&mut slot, true);
        relay_present(&shared, false);
        assert!(slot.with(|b| b.read(&["on"])).is_err());
        relay_present(&shared, true);
        assert!(slot.finish().is_ok());
        assert_eq!(relay_frame(&shared).unwrap(), off, "stop switched the relay off");
    }

    /// Before I2C is enabled, the provider still starts, and each board is
    /// unreachable with the reason, as a provider installed before host prep
    /// should report it.
    #[test]
    fn a_missing_bus_leaves_the_provider_up_and_its_boards_unreachable() {
        let bus = "/nonexistent/i2c-1";
        let config = Config {
            bus: bus.into(),
            mock: false,
            devices: vec![DeviceConfig { id: "relay".into(), ..config(BoardType::OptoRelay, None) }],
        };
        let mut provider = ModulinoProvider::new(config).expect("starts without the bus");
        provider.start(Duration::ZERO).unwrap();
        let device = provider.devices()[0].clone();
        let health = provider.device_health(&device).expect("the board is reported");
        assert_eq!(health.state, DeviceState::Unreachable);
        assert!(health.message.contains(&format!("cannot open {bus}")), "{}", health.message);
    }

    /// Through the provider itself, as the SDK calls it at session end.
    #[test]
    fn provider_stop_makes_safe_a_board_marked_lost() {
        let config = Config {
            bus: String::new(),
            mock: true,
            devices: vec![DeviceConfig { id: "relay".into(), ..config(BoardType::OptoRelay, None) }],
        };
        let mut provider = ModulinoProvider::new(config).unwrap();
        provider.start(Duration::ZERO).unwrap();
        set_relay(&mut provider.slots[0], true);
        // A transfer failed since: the board is not connected at stop.
        provider.slots[0].connected = false;
        provider.stop();
        let on = match &mut provider.slots[0].board {
            Board::OptoRelay(r) => r.is_on().unwrap(),
            _ => unreachable!("an opto relay"),
        };
        assert!(!on, "the provider's stop switched the relay off");
    }

    /// The safe state follows the mode the Motors board reports: DC motors
    /// stopped, stepper coils released. Checked at the first connect and
    /// at stop, with another frame written between the two.
    #[test]
    fn motors_safe_state_follows_the_reported_mode() {
        use modulino_driver::DecayMode;
        use modulino_driver::codec::motors::{encode_dc_speed, encode_decay, encode_stepper};
        let at = Kind::Motors.default_address();
        for (flags, safe) in [(0x00, encode_dc_speed(0, 0).unwrap()), (0x02, encode_stepper(0, 1, 1).unwrap())] {
            let mut bus = SimBus::new().with_node(Kind::Motors);
            assert!(bus.set_reading(at, &[0, 0, 0, 0, flags]));
            let (mut slot, shared) = slot(bus, &config(BoardType::Motors, None));
            let last = || sim(&shared, |s| s.last_write(at).map(<[u8]>::to_vec));
            slot.with(|_| Ok(())).unwrap();
            assert_eq!(last(), Some(safe.to_vec()), "first connect, flags {flags:#04x}");
            slot.with(|b| match b {
                Board::Motors(m) => m.set_decay(DecayMode::Slow).map_err(Failure::from),
                _ => unreachable!("a motors board"),
            })
            .unwrap();
            assert_eq!(last(), Some(encode_decay(DecayMode::Slow).to_vec()));
            assert!(slot.finish().is_ok());
            assert_eq!(last(), Some(safe.to_vec()), "stop, flags {flags:#04x}");
        }
    }

    /// A provider in mock mode with one of every board type on the bus.
    fn every_board() -> (ModulinoProvider, Shared) {
        let devices = BoardType::ALL
            .iter()
            .enumerate()
            .map(|(i, &t)| DeviceConfig { id: format!("d{i}"), ..config(t, None) })
            .collect();
        let config = Config { bus: String::new(), mock: true, devices };
        let shared = Arc::new(Mutex::new(Bus::Sim(simulated(&config))));
        let mut provider = ModulinoProvider::on_bus(&config, &shared);
        provider.start(Duration::ZERO).unwrap();
        (provider, shared)
    }

    #[test]
    fn every_board_reads_every_signal() {
        let (mut provider, _) = every_board();
        for device in provider.devices().to_vec() {
            let signals = device.device_type().signals();
            let ids: Vec<&str> = signals.iter().map(|s| s.id()).collect();
            let read =
                provider.read(&device, &ids).unwrap_or_else(|e| panic!("{}: {}", device.device_type().id(), e.message));
            assert_eq!(read.len(), ids.len(), "{}", device.device_type().id());
            for s in signals {
                let r = &read[s.id()];
                assert_eq!(r.value.value_type(), s.value_type(), "{}.{}", device.device_type().id(), s.id());
                assert_eq!(r.quality, Quality::Ok, "{}.{}", device.device_type().id(), s.id());
            }
        }
    }

    /// Every declared function, called through the provider with valid
    /// arguments in an order where each frame differs from the one before.
    #[test]
    fn every_board_runs_every_function() {
        use anolis_provider_sdk::{Args, CallContext, Value};
        let v = |pairs: &[(&str, Value)]| Args::from_iter(pairs.iter().cloned());
        // Type, function, arguments, and whether the call writes to the board
        // (knob and joystick settings are kept by the provider).
        let calls: Vec<(&str, &str, Args, bool)> = vec![
            ("buttons", "set_leds", v(&[("a", true.into()), ("b", false.into()), ("c", true.into())]), true),
            ("buzzer", "tone", v(&[("frequency_hz", 440u64.into()), ("duration_ms", 100u64.into())]), true),
            ("buzzer", "no_tone", Args::default(), true),
            ("vibro", "vibrate", v(&[("duration_ms", 100u64.into()), ("power", 50u64.into())]), true),
            ("vibro", "off", Args::default(), true),
            (
                "pixels",
                "set",
                v(&[
                    ("index", 1u64.into()),
                    ("red", 10u64.into()),
                    ("green", 20u64.into()),
                    ("blue", 30u64.into()),
                    ("brightness", 50u64.into()),
                ]),
                true,
            ),
            (
                "pixels",
                "set_all",
                v(&[("red", 1u64.into()), ("green", 2u64.into()), ("blue", 3u64.into()), ("brightness", 40u64.into())]),
                true,
            ),
            ("pixels", "off", Args::default(), true),
            ("knob", "set_position", v(&[("position", 5i64.into())]), false),
            ("joystick", "set_deadzone", v(&[("deadzone", 10u64.into())]), false),
            ("opto_relay", "set", v(&[("on", true.into())]), true),
            ("latch_relay", "set", v(&[("on", true.into())]), true),
            ("led_matrix", "show_mono", v(&[("pixels", vec![0xFFu8; 12].into())]), true),
            ("led_matrix", "show_gray", v(&[("levels", vec![15u8; 96].into())]), true),
            ("led_matrix", "clear", Args::default(), true),
            ("motors", "set_speeds", v(&[("a", 100i64.into()), ("b", (-100i64).into())]), true),
            ("motors", "stop", Args::default(), true),
            ("motors", "set_mode", v(&[("mode", "stepper".into())]), true),
            ("motors", "step", v(&[("steps", 10i64.into()), ("period_ticks", 5u64.into())]), true),
            ("motors", "release_coils", Args::default(), true),
            ("motors", "set_mode", v(&[("mode", "dc".into())]), true),
        ];
        let (mut provider, shared) = every_board();
        let devices = provider.devices().to_vec();
        let declared: BTreeSet<(String, String)> = devices
            .iter()
            .flat_map(|d| {
                d.device_type().functions().iter().map(|f| (d.device_type().id().to_owned(), f.name().to_owned()))
            })
            .collect();
        let called: BTreeSet<(String, String)> =
            calls.iter().map(|(t, f, _, _)| (t.to_string(), f.to_string())).collect();
        assert_eq!(called, declared, "the table names every declared function");
        for (t, name, args, writes) in &calls {
            let index = devices.iter().position(|d| d.device_type().id() == *t).unwrap();
            let device = &devices[index];
            let function = device.device_type().functions().iter().find(|f| f.name() == *name).unwrap();
            let at =
                BoardType::ALL[index].node_kind().expect("every board with a function is a node").default_address();
            let frame = || sim(&shared, |s| s.last_write(at).map(<[u8]>::to_vec));
            let before = frame();
            let results = provider
                .call(device, function, args, &CallContext::default())
                .unwrap_or_else(|e| panic!("{t}.{name}: {}", e.message));
            assert!(results.is_empty(), "{t}.{name} declares no results");
            if *writes {
                assert_ne!(frame(), before, "{t}.{name} wrote a new frame");
            }
        }
    }

    /// A mode switch while a stepper move runs is refused, and allowed once
    /// the move has ended.
    #[test]
    fn motors_set_mode_is_refused_mid_move() {
        use anolis_provider_sdk::{Args, Value};
        let at = Kind::Motors.default_address();
        let mut bus = SimBus::new().with_node(Kind::Motors);
        assert!(bus.set_reading(at, &[0, 0, 0, 0, 0x03]), "a busy stepper");
        let (mut slot, shared) = slot(bus, &config(BoardType::Motors, None));
        let dc = Args::from_iter([("mode", Value::from("dc"))]);
        let err = slot.with(|b| b.call("set_mode", &dc)).unwrap_err();
        assert_eq!(err.code, StatusCode::FailedPrecondition);
        let before = sim(&shared, |s| s.last_write(at).map(<[u8]>::to_vec));
        assert!(sim(&shared, |s| s.set_reading(at, &[0, 0, 0, 0, 0x02])), "the move ended");
        slot.with(|b| b.call("set_mode", &dc)).unwrap();
        assert_ne!(sim(&shared, |s| s.last_write(at).map(<[u8]>::to_vec)), before, "the switch was sent");
    }

    /// When the Motors board's mode cannot be read at stop, both safe
    /// commands go out and the failed read is reported.
    #[test]
    fn motors_stop_sends_both_when_the_mode_cannot_be_read() {
        use embedded_hal::i2c::ErrorKind;
        use modulino_driver::codec::motors::{encode_dc_speed, encode_stepper};
        let at = Kind::Motors.default_address();
        let mut bus = SimBus::new().with_node(Kind::Motors);
        assert!(bus.set_reading(at, &[0, 0, 0, 0, 0x01]));
        let (mut slot, shared) = slot(bus, &config(BoardType::Motors, None));
        slot.with(|b| match b {
            Board::Motors(m) => m.set_speeds(100, 0).map_err(Failure::from),
            _ => unreachable!("a motors board"),
        })
        .unwrap();
        assert_eq!(
            sim(&shared, |s| s.last_write(at).map(<[u8]>::to_vec)),
            Some(encode_dc_speed(100, 0).unwrap().to_vec())
        );
        assert!(sim(&shared, |s| s.set_read_fault(at, Some(ErrorKind::Other))));
        assert!(matches!(slot.finish(), Err(Failure::Unreachable(_))), "the failed read is reported");
        // Only the last frame is kept: the release, sent after the stop.
        assert_eq!(
            sim(&shared, |s| s.last_write(at).map(<[u8]>::to_vec)),
            Some(encode_stepper(0, 1, 1).unwrap().to_vec())
        );
    }

    #[test]
    fn stop_writes_nothing_to_another_kind_of_board() {
        let at = Kind::OptoRelay.default_address();
        let (mut slot, shared) =
            slot(SimBus::new().with_node_at(Kind::Buttons, at), &config(BoardType::OptoRelay, None));
        assert!(matches!(slot.finish(), Err(Failure::Unreachable(_))));
        assert_eq!(
            sim(&shared, |s| s.last_write(at).map(<[u8]>::to_vec)),
            None,
            "nothing was written to the buttons board"
        );
    }

    #[test]
    fn stop_makes_safe_a_board_absent_at_start() {
        let (mut slot, shared) = slot(SimBus::new().with_node(Kind::OptoRelay), &config(BoardType::OptoRelay, None));
        relay_present(&shared, false);
        assert!(slot.with(|_| Ok(())).is_err());
        relay_present(&shared, true);
        assert!(slot.finish().is_ok());
        assert!(relay_frame(&shared).is_some(), "stop reached the board and wrote its safe state");
    }
}
