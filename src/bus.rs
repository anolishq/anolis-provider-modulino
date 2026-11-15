//! The I2C bus every board shares, and the per-board handle that reaches a
//! board directly on the bus or behind a Modulino Hub port.

use std::cell::RefCell;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::{self, ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};
use linux_embedded_hal::I2cdev;
use linux_embedded_hal::i2cdev::linux::LinuxI2CError;
use modulino_driver::sim::SimBus;
use modulino_driver::{Address, Hub};

/// `EREMOTEIO`: what the Raspberry Pi's I2C controllers (i2c-bcm2835 on a Pi
/// 4, i2c-designware on a Pi 5) return for an address nothing acknowledges.
/// linux-embedded-hal 0.5 maps only `ENXIO` and `ENODEV` to a NACK and
/// reports this one as `Other`. modulino-driver acts on a NACK specifically:
/// the SHT31 Thermo path retries a NACKed read while a measurement runs, and
/// identity checks report a NACK differently from other failures. Read from
/// the kernel drivers' source; the modulino-driver bench checklist (step 1.1)
/// measures it on hardware.
const EREMOTEIO: i32 = 121;

/// A bus error, kept as its `embedded-hal` kind and a message for the log.
#[derive(Debug)]
pub struct BusError {
    kind: ErrorKind,
    message: String,
}

impl BusError {
    fn linux(error: &linux_embedded_hal::I2CError) -> Self {
        let errno = match error.inner() {
            LinuxI2CError::Errno(n) => Some(*n),
            LinuxI2CError::Io(io) => io.raw_os_error(),
        };
        let kind = if errno == Some(EREMOTEIO) {
            ErrorKind::NoAcknowledge(NoAcknowledgeSource::Unknown)
        } else {
            i2c::Error::kind(error)
        };
        Self { kind, message: error.to_string() }
    }

    fn sim(error: &impl i2c::Error) -> Self {
        Self { kind: error.kind(), message: format!("{error:?}") }
    }
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({:?})", self.message, self.kind)
    }
}

impl i2c::Error for BusError {
    fn kind(&self) -> ErrorKind {
        self.kind
    }
}

/// The bus the boards share: the Linux I2C device, or modulino-driver's
/// simulated boards in mock mode.
pub enum Bus {
    /// Opened at the first transfer, and again at each one until it opens,
    /// so a provider started before the bus can be opened (I2C not yet
    /// enabled, or no permission) stays up: in a session the SDK's host check
    /// then reports `bus.access` unmet, and any transfer fails with why.
    Linux {
        path: String,
        dev: Option<I2cdev>,
    },
    Sim(SimBus),
}

impl Bus {
    pub fn linux(path: &str) -> Self {
        Bus::Linux { path: path.to_owned(), dev: None }
    }
}

impl ErrorType for Bus {
    type Error = BusError;
}

impl I2c for Bus {
    fn transaction(&mut self, address: u8, operations: &mut [Operation<'_>]) -> Result<(), BusError> {
        match self {
            Bus::Linux { path, dev } => {
                let dev = match dev {
                    Some(dev) => dev,
                    None => dev.insert(I2cdev::new(&*path).map_err(|e| BusError {
                        kind: ErrorKind::Other,
                        message: format!("cannot open {path}: {e}"),
                    })?),
                };
                dev.transaction(address, operations).map_err(|e| BusError::linux(&e))
            }
            Bus::Sim(sim) => sim.transaction(address, operations).map_err(|e| BusError::sim(&e)),
        }
    }
}

/// Where a board sits: on the bus, or on one port of a hub on the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HubPort {
    pub hub: Address,
    pub port: u8,
}

/// One board's handle on the shared bus. It is `Send` (the bus is behind a
/// mutex) and owned by the board's driver. A board behind a hub is reached
/// through modulino-driver's own `Hub`, built for the length of one
/// transaction while the bus is locked, so the driver's port selection and
/// deselect-on-error apply unchanged.
#[derive(Clone)]
pub struct Route {
    bus: Arc<Mutex<Bus>>,
    via: Option<HubPort>,
}

impl Route {
    pub fn new(bus: &Arc<Mutex<Bus>>, via: Option<HubPort>) -> Self {
        Self { bus: Arc::clone(bus), via }
    }
}

impl ErrorType for Route {
    type Error = BusError;
}

impl I2c for Route {
    fn transaction(&mut self, address: u8, operations: &mut [Operation<'_>]) -> Result<(), BusError> {
        let mut bus = self.bus.lock().unwrap_or_else(PoisonError::into_inner);
        match self.via {
            None => I2c::transaction(&mut *bus, address, operations),
            Some(via) => {
                let cell = RefCell::new(&mut *bus);
                let hub = Hub::with_address(&cell, via.hub).expect("hub addresses are checked against 0x70-0x77");
                let mut port = hub.port(via.port).expect("hub ports are checked against 0-7");
                port.transaction(address, operations)
            }
        }
    }
}

/// A delay that sleeps the thread: the provider serves one request at a time,
/// so a driver's wait (the latch relay's coil pulse, a sensor's conversion)
/// holds the session for its length, as it would on the board.
pub struct Sleep;

impl DelayNs for Sleep {
    fn delay_ns(&mut self, ns: u32) {
        std::thread::sleep(Duration::from_nanos(ns.into()));
    }
}
