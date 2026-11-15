//! The provider's config: which bus, and which boards at which addresses.

use std::collections::HashSet;

use anolis_provider_sdk::BoxError;
use modulino_driver::{Address, Kind};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::bus::HubPort;

/// anolis-provider-modulino config
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The Linux I2C bus the boards are on
    #[serde(default = "default_bus")]
    pub bus: String,
    /// Simulated boards instead of the bus: no hardware is touched
    #[serde(default)]
    pub mock: bool,
    #[schemars(length(min = 1))]
    pub devices: Vec<DeviceConfig>,
}

fn default_bus() -> String {
    "/dev/i2c-1".to_owned()
}

/// A board's kind, as a config names it
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BoardType {
    Buttons,
    Buzzer,
    Vibro,
    Pixels,
    Knob,
    Joystick,
    OptoRelay,
    LatchRelay,
    LedMatrix,
    Motors,
    Thermo,
    Pressure,
    Light,
    Movement,
    Distance,
}

impl BoardType {
    pub const ALL: [BoardType; 15] = [
        BoardType::Buttons,
        BoardType::Buzzer,
        BoardType::Vibro,
        BoardType::Pixels,
        BoardType::Knob,
        BoardType::Joystick,
        BoardType::OptoRelay,
        BoardType::LatchRelay,
        BoardType::LedMatrix,
        BoardType::Motors,
        BoardType::Thermo,
        BoardType::Pressure,
        BoardType::Light,
        BoardType::Movement,
        BoardType::Distance,
    ];

    pub fn id(self) -> &'static str {
        match self {
            BoardType::Buttons => "buttons",
            BoardType::Buzzer => "buzzer",
            BoardType::Vibro => "vibro",
            BoardType::Pixels => "pixels",
            BoardType::Knob => "knob",
            BoardType::Joystick => "joystick",
            BoardType::OptoRelay => "opto_relay",
            BoardType::LatchRelay => "latch_relay",
            BoardType::LedMatrix => "led_matrix",
            BoardType::Motors => "motors",
            BoardType::Thermo => "thermo",
            BoardType::Pressure => "pressure",
            BoardType::Light => "light",
            BoardType::Movement => "movement",
            BoardType::Distance => "distance",
        }
    }

    /// The node firmware's kind, for the boards that run it.
    pub fn node_kind(self) -> Option<Kind> {
        Some(match self {
            BoardType::Buttons => Kind::Buttons,
            BoardType::Buzzer => Kind::Buzzer,
            BoardType::Vibro => Kind::Vibro,
            BoardType::Pixels => Kind::Pixels,
            BoardType::Knob => Kind::Knob,
            BoardType::Joystick => Kind::Joystick,
            BoardType::OptoRelay => Kind::OptoRelay,
            BoardType::LatchRelay => Kind::LatchRelay,
            BoardType::LedMatrix => Kind::LedMatrix,
            BoardType::Motors => Kind::Motors,
            _ => return None,
        })
    }

    /// The address a board answers at when the config gives none. Movement's
    /// is the first of the two it tries (0x6A, then 0x6B); Thermo has none to
    /// configure: it finds its HS3003 at 0x44 or its SHT31 at 0x45 itself.
    pub fn default_address(self) -> Option<u8> {
        if let Some(kind) = self.node_kind() {
            return Some(kind.default_address().get());
        }
        match self {
            BoardType::Pressure => Some(0x5C),
            BoardType::Light => Some(0x53),
            BoardType::Distance => Some(0x29),
            BoardType::Movement => Some(0x6A),
            _ => None,
        }
    }
}

/// One board
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    /// Device id, unique in this provider
    #[schemars(regex(pattern = r"^[a-z][a-z0-9_]*$"))]
    pub id: String,
    pub r#type: BoardType,
    pub label: Option<String>,
    /// 7-bit I2C address, when not the board's factory default
    #[schemars(range(min = 1, max = 0x77))]
    pub address: Option<u8>,
    /// The Modulino Hub port the board is on, when it is behind one
    pub hub: Option<HubConfig>,
}

/// A port of a Modulino Hub (TCA9548A) on the bus
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HubConfig {
    /// The hub's 7-bit address
    #[schemars(range(min = 0x70, max = 0x77))]
    pub address: u8,
    #[schemars(range(min = 0, max = 7))]
    pub port: u8,
}

impl DeviceConfig {
    pub fn via(&self) -> Option<HubPort> {
        self.hub.map(|h| HubPort {
            hub: Address::new(h.address).expect("the schema limits hub addresses to 0x70-0x77"),
            port: h.port,
        })
    }

    pub fn address(&self) -> Option<Address> {
        self.address.map(|a| Address::new(a).expect("the schema limits addresses to 0x01-0x77"))
    }
}

/// Where a board sits: on the bus (`None`), or a hub's address and port.
type Segment = Option<(u8, u8)>;

/// The addresses a board may answer at: its configured one, else its
/// defaults. Thermo finds its sensor at 0x44 or 0x45, and Movement with no
/// address tries 0x6A and then 0x6B, so each holds both.
fn addresses(device: &DeviceConfig) -> Vec<u8> {
    match (device.r#type, device.address) {
        (_, Some(a)) => vec![a],
        (BoardType::Thermo, None) => vec![0x44, 0x45],
        (BoardType::Movement, None) => vec![0x6A, 0x6B],
        (t, None) => t.default_address().into_iter().collect(),
    }
}

/// What the schema cannot say: unique ids, and no two boards a transfer could
/// reach at once. A hub port, while selected, is joined to the bus, so a board
/// behind a port shares the bus's addresses (modulino-driver
/// `docs/design.md`, section 4.8) and every hub's own address. Two different
/// ports are never selected together, so they may repeat an address.
pub fn check(config: &Config) -> Result<(), BoxError> {
    let mut ids = HashSet::new();
    for device in &config.devices {
        if !ids.insert(&device.id) {
            return Err(format!("device id {:?} is configured twice", device.id).into());
        }
        if device.r#type == BoardType::Thermo && device.address.is_some() {
            return Err(format!(
                "device {:?}: a thermo board's address is found, not configured (0x44 for an HS3003, 0x45 for an SHT31)",
                device.id
            )
            .into());
        }
    }
    let hubs: HashSet<u8> = config.devices.iter().filter_map(|d| d.hub.map(|h| h.address)).collect();
    let mut taken: Vec<(Segment, u8, &str)> = Vec::new();
    for device in &config.devices {
        let segment = device.hub.map(|h| (h.address, h.port));
        for address in addresses(device) {
            if hubs.contains(&address) {
                return Err(format!("device {:?}: {address:#04x} is a hub's address", device.id).into());
            }
            let clash = taken
                .iter()
                .find(|(other, a, _)| *a == address && (*other == segment || other.is_none() || segment.is_none()));
            if let Some((other, _, id)) = clash {
                return Err(format!(
                    "device {:?} at {address:#04x}{} would answer with {id:?} at {address:#04x}{}",
                    device.id,
                    on_segment(segment),
                    on_segment(*other)
                )
                .into());
            }
            taken.push((segment, address, &device.id));
        }
    }
    Ok(())
}

fn on_segment(segment: Segment) -> String {
    match segment {
        None => " on the bus".to_owned(),
        Some((hub, port)) => format!(" on port {port} of the hub at {hub:#04x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, t: BoardType, address: Option<u8>, hub: Option<(u8, u8)>) -> DeviceConfig {
        DeviceConfig {
            id: id.into(),
            r#type: t,
            label: None,
            address,
            hub: hub.map(|(address, port)| HubConfig { address, port }),
        }
    }

    fn checked(devices: Vec<DeviceConfig>) -> Result<(), String> {
        check(&Config { bus: default_bus(), mock: true, devices }).map_err(|e| e.to_string())
    }

    #[test]
    fn a_device_id_used_twice_is_refused() {
        let e = checked(vec![device("a", BoardType::Buttons, None, None), device("a", BoardType::Buzzer, None, None)])
            .unwrap_err();
        assert!(e.contains("configured twice"), "{e}");
    }

    #[test]
    fn the_same_address_on_the_bus_and_behind_a_port_is_refused() {
        let e = checked(vec![
            device("up", BoardType::OptoRelay, None, None),
            device("down", BoardType::OptoRelay, None, Some((0x70, 1))),
        ])
        .unwrap_err();
        assert!(e.contains("would answer with"), "{e}");
    }

    #[test]
    fn the_same_address_on_two_ports_is_accepted() {
        checked(vec![
            device("left", BoardType::OptoRelay, None, Some((0x70, 0))),
            device("right", BoardType::OptoRelay, None, Some((0x70, 1))),
        ])
        .unwrap();
    }

    #[test]
    fn a_board_at_a_hubs_address_is_refused_on_any_segment() {
        for hub in [None, Some((0x70, 3))] {
            let e = checked(vec![
                device("behind", BoardType::Buttons, None, Some((0x71, 0))),
                device("odd", BoardType::Buttons, Some(0x71), hub),
            ])
            .unwrap_err();
            assert!(e.contains("hub's address"), "{e}");
        }
    }

    #[test]
    fn movement_without_an_address_holds_both_of_its_defaults() {
        let e = checked(vec![
            device("imu", BoardType::Movement, None, None),
            device("clash", BoardType::Buttons, Some(0x6B), None),
        ])
        .unwrap_err();
        assert!(e.contains("0x6b"), "{e}");
    }

    #[test]
    fn a_thermo_address_is_refused() {
        let e = checked(vec![device("t", BoardType::Thermo, Some(0x44), None)]).unwrap_err();
        assert!(e.contains("found, not configured"), "{e}");
    }
}
