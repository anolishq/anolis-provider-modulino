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

/// What the schema cannot say.
pub fn check(config: &Config) -> Result<(), BoxError> {
    let mut ids = HashSet::new();
    let mut taken = HashSet::new();
    let hubs: HashSet<u8> = config.devices.iter().filter_map(|d| d.hub.map(|h| h.address)).collect();
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
        let segment = device.hub.map(|h| (h.address, h.port));
        let addresses: Vec<u8> = match (device.r#type, device.address) {
            (_, Some(a)) => vec![a],
            (BoardType::Thermo, None) => vec![0x44, 0x45],
            (t, None) => t.default_address().into_iter().collect(),
        };
        for address in addresses {
            if !taken.insert((segment, address)) {
                return Err(format!(
                    "device {:?}: another device is already at {address:#04x}{}",
                    device.id,
                    on_segment(segment)
                )
                .into());
            }
            if segment.is_none() && hubs.contains(&address) {
                return Err(format!("device {:?}: {address:#04x} is a hub's address on the bus", device.id).into());
            }
        }
    }
    Ok(())
}

fn on_segment(segment: Option<(u8, u8)>) -> String {
    match segment {
        None => " on the bus".to_owned(),
        Some((hub, port)) => format!(" on port {port} of the hub at {hub:#04x}"),
    }
}
