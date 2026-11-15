//! Each Modulino board: its declaration (what the runtime can read and call)
//! and how a read or call maps onto modulino-driver.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime};

use anolis_provider_sdk::{
    Arg, Args, Category, DeviceType, Function, ProviderError, Quality, Reading, Readings, Results, Signal, StatusCode,
    ValueType,
};
use modulino_driver::{
    Buttons, Buzzer, Color, DistanceReading, Error, Joystick, Knob, LatchRelay, LedMatrix, Lps22hb, Lsm6dsox,
    Ltr381rgb, MatrixMode, MotorMode, Motors, OptoRelay, Pixels, Thermo, Vibro, Vl53l4,
};

use crate::bus::{BusError, Route, Sleep};
use crate::config::{BoardType, DeviceConfig};

/// LED matrix size, from the driver.
const COLS: usize = 12;
const ROWS: usize = 8;
const MONO_BYTES: usize = COLS * ROWS / 8;
const GRAY_BYTES: usize = COLS * ROWS;
/// How long a read waits for a first range from the distance sensor.
const FIRST_RANGE_WAIT: Duration = Duration::from_millis(300);
/// How long setup waits for a sensor's first sample. The slowest, the
/// pressure sensor at 10 Hz, takes 100 ms.
const FIRST_SAMPLE_WAIT: Duration = Duration::from_millis(500);

fn sensor(id: &str, value_type: ValueType, unit: &str, description: &str) -> Signal {
    Signal::new(id, value_type).unit(unit).description(description)
}

fn declare(t: BoardType) -> Arc<DeviceType> {
    let d = DeviceType::new(t.id());
    let d = match t {
        BoardType::Buttons => d
            .signal(Signal::new("a", ValueType::Bool).description("Button A is pressed"))
            .signal(Signal::new("b", ValueType::Bool).description("Button B is pressed"))
            .signal(Signal::new("c", ValueType::Bool).description("Button C is pressed"))
            .function(
                Function::new("set_leds")
                    .arg(Arg::bool("a").description("LED above button A"))
                    .arg(Arg::bool("b").description("LED above button B"))
                    .arg(Arg::bool("c").description("LED above button C"))
                    .description("Light or clear the three button LEDs"),
            ),
        BoardType::Buzzer => d
            .function(
                Function::new("tone")
                    .arg(Arg::uint64("frequency_hz").range(180, 5_898_150).unit("Hz"))
                    .arg(
                        Arg::uint64("duration_ms")
                            .max(u64::from(u32::MAX))
                            .unit("ms")
                            .description("0 plays until the next call"),
                    )
                    .description("Play a tone"),
            )
            .function(Function::new("no_tone").description("Stop the tone")),
        BoardType::Vibro => d
            .function(
                Function::new("vibrate")
                    .arg(Arg::uint64("duration_ms").range(1, u64::from(u32::MAX)).unit("ms"))
                    .arg(Arg::uint64("power").range(0, 100).unit("%"))
                    .description("Vibrate for a duration at a power level"),
            )
            .function(Function::new("off").description("Stop vibrating")),
        BoardType::Pixels => {
            let colour = |f: Function| {
                f.arg(Arg::uint64("red").range(0, 255))
                    .arg(Arg::uint64("green").range(0, 255))
                    .arg(Arg::uint64("blue").range(0, 255))
                    .arg(Arg::uint64("brightness").range(0, 100).unit("%"))
            };
            d.function(
                colour(Function::new("set").arg(Arg::uint64("index").range(0, 7).description("LED 0-7")))
                    .description("Set one LED and show the frame"),
            )
            .function(colour(Function::new("set_all")).description("Set every LED and show the frame"))
            .function(Function::new("off").description("Turn every LED off"))
        }
        BoardType::Knob => d
            .signal(
                Signal::new("position", ValueType::Int64)
                    .description("Position, including the offset set_position gives"),
            )
            .signal(Signal::new("pressed", ValueType::Bool).description("The shaft is pressed"))
            .function(
                Function::new("set_position")
                    .arg(Arg::int64("position").range(i64::from(i16::MIN), i64::from(i16::MAX)))
                    .description("Call the current position this value (kept on the host)")
                    .category(Category::Config),
            ),
        BoardType::Joystick => d
            .signal(Signal::new("x", ValueType::Int64).description("X, -127 to 127, 0 at rest"))
            .signal(Signal::new("y", ValueType::Int64).description("Y, -127 to 127, 0 at rest"))
            .signal(Signal::new("pressed", ValueType::Bool).description("The stick is pressed"))
            .function(
                Function::new("set_deadzone")
                    .arg(Arg::uint64("deadzone").range(0, 255).description("Raw counts around the centre read as 0"))
                    .category(Category::Config),
            ),
        BoardType::OptoRelay | BoardType::LatchRelay => d
            .signal(Signal::new("on", ValueType::Bool).description("The relay is on"))
            .function(Function::new("set").arg(Arg::bool("on")).description("Switch the relay on or off")),
        BoardType::LedMatrix => d
            .signal(Signal::new("mode", ValueType::String).description("mono or grayscale"))
            .function(Function::new("clear").description("Turn every LED off"))
            .function(
                Function::new("show_mono")
                    .arg(Arg::bytes("pixels").description(
                        "12 bytes, one bit per LED, row by row from the top left, most significant bit first",
                    ))
                    .description("Show a one-bit picture"),
            )
            .function(
                Function::new("show_gray")
                    .arg(
                        Arg::bytes("levels")
                            .description("96 bytes, one level 0-15 per LED, row by row from the top left"),
                    )
                    .description("Show a 16-level picture"),
            ),
        BoardType::Motors => d
            .signal(Signal::new("mode", ValueType::String).description("dc or stepper"))
            .signal(Signal::new("busy", ValueType::Bool).description("A stepper move is in progress"))
            .signal(
                Signal::new("current_a_raw", ValueType::Uint64).description("Motor A current, ADC counts above idle"),
            )
            .signal(
                Signal::new("current_b_raw", ValueType::Uint64).description("Motor B current, ADC counts above idle"),
            )
            // ACTUATE with required numeric arguments whose 0 is safe: the
            // runtime's zero_is_safe rung can stop both motors with them.
            .function(
                Function::new("set_speeds")
                    .arg(Arg::int64("a").range(-32767, 32767).description("Motor A, -32767 to 32767"))
                    .arg(Arg::int64("b").range(-32767, 32767).description("Motor B, -32767 to 32767"))
                    .description("Drive both DC motors (dc mode)"),
            )
            .function(Function::new("stop").description("Stop both DC motors (dc mode)"))
            .function(
                Function::new("step")
                    .arg(
                        Arg::int64("steps")
                            .range(i64::from(i32::MIN), i64::from(i32::MAX))
                            .description("Negative turns backwards"),
                    )
                    .arg(Arg::uint64("period_ticks").range(1, 65535).unit("0.1 ms"))
                    .description("Move the stepper, then hold its coils (stepper mode)"),
            )
            .function(Function::new("release_coils").description("De-energise the stepper coils (stepper mode)"))
            .function(
                Function::new("set_mode")
                    .arg(Arg::string("mode").allowed_values(["dc", "stepper"]))
                    .description("Switch between two DC motors and one stepper")
                    .category(Category::Config),
            ),
        BoardType::Thermo => d.signal(sensor("temperature", ValueType::Double, "degC", "Temperature")).signal(sensor(
            "humidity",
            ValueType::Double,
            "%",
            "Relative humidity",
        )),
        BoardType::Pressure => d
            .signal(sensor("pressure", ValueType::Double, "hPa", "Barometric pressure"))
            .signal(sensor("temperature", ValueType::Double, "degC", "Temperature of the pressure sensor")),
        BoardType::Light => d
            .signal(sensor("lux", ValueType::Double, "lx", "Ambient light"))
            .signal(sensor("red", ValueType::Uint64, "", "Red channel, raw counts").not_default())
            .signal(sensor("green", ValueType::Uint64, "", "Green channel, raw counts").not_default())
            .signal(sensor("blue", ValueType::Uint64, "", "Blue channel, raw counts").not_default())
            .signal(sensor("ir", ValueType::Uint64, "", "Infrared channel, raw counts").not_default()),
        BoardType::Movement => d
            .signal(sensor("accel_x", ValueType::Double, "g", "Acceleration, X"))
            .signal(sensor("accel_y", ValueType::Double, "g", "Acceleration, Y"))
            .signal(sensor("accel_z", ValueType::Double, "g", "Acceleration, Z"))
            .signal(sensor("gyro_x", ValueType::Double, "deg/s", "Angular velocity, X"))
            .signal(sensor("gyro_y", ValueType::Double, "deg/s", "Angular velocity, Y"))
            .signal(sensor("gyro_z", ValueType::Double, "deg/s", "Angular velocity, Z")),
        BoardType::Distance => d
            .signal(sensor("distance_mm", ValueType::Uint64, "mm", "Distance to the target").stale_after_ms(1000))
            .signal(
                sensor("range_status", ValueType::Uint64, "", "ST's range status: 0 is a valid measurement")
                    .stale_after_ms(1000),
            )
            .signal(
                sensor("signal_kcps", ValueType::Uint64, "kcps", "Return signal rate")
                    .not_default()
                    .stale_after_ms(1000),
            )
            .signal(
                sensor("sigma_mm", ValueType::Uint64, "mm", "Distance standard deviation estimate")
                    .not_default()
                    .stale_after_ms(1000),
            ),
    };
    d.build()
}

static TYPES: LazyLock<HashMap<BoardType, Arc<DeviceType>>> =
    LazyLock::new(|| BoardType::ALL.iter().map(|&t| (t, declare(t))).collect());

pub fn device_type(t: BoardType) -> &'static Arc<DeviceType> {
    &TYPES[&t]
}

/// Why an operation on a board failed.
pub enum Failure {
    /// The board did not answer, is another board, or did not finish
    /// starting: it is reported UNREACHABLE and tried again.
    Unreachable(String),
    /// Anything else, answered as this status.
    Status(ProviderError),
}

fn classify(error: Error<BusError>) -> Failure {
    match error {
        Error::I2c(e) => Failure::Unreachable(format!("bus: {e}")),
        e @ (Error::WrongIdentity { .. } | Error::WrongChip { .. } | Error::Timeout) => {
            Failure::Unreachable(e.to_string())
        }
        Error::OutOfRange => {
            Failure::Status(ProviderError::new(StatusCode::OutOfRange, "value out of range for the board"))
        }
        e @ (Error::ReservedPrefix | Error::FrameLength { .. }) => {
            Failure::Status(ProviderError::new(StatusCode::InvalidArgument, e.to_string()))
        }
        e => Failure::Status(ProviderError::internal(e.to_string())),
    }
}

impl From<Error<BusError>> for Failure {
    fn from(error: Error<BusError>) -> Self {
        classify(error)
    }
}

impl From<ProviderError> for Failure {
    fn from(error: ProviderError) -> Self {
        Failure::Status(error)
    }
}

/// A board's driver, owning its route to the board.
pub enum Board {
    Buttons(Buttons<Route>),
    Buzzer(Buzzer<Route>),
    Vibro(Vibro<Route>),
    Pixels(Pixels<Route>),
    Knob(Knob<Route>),
    Joystick(Joystick<Route>),
    OptoRelay(OptoRelay<Route>),
    LatchRelay(LatchRelay<Route>),
    LedMatrix(LedMatrix<Route>),
    Motors(Motors<Route>),
    Thermo(Thermo<Route>),
    Pressure(Lps22hb<Route>),
    Light(Ltr381rgb<Route>),
    Movement(Lsm6dsox<Route>),
    Distance(Vl53l4<Route>, Option<(DistanceReading, SystemTime)>),
}

macro_rules! at {
    ($ty:ident, $route:expr, $addr:expr) => {
        match $addr {
            Some(a) => $ty::with_address($route, a),
            None => $ty::new($route),
        }
    };
}

impl Board {
    /// The driver for a configured board. Does no I/O.
    pub fn new(config: &DeviceConfig, route: Route) -> Self {
        let a = config.address();
        match config.r#type {
            BoardType::Buttons => Board::Buttons(at!(Buttons, route, a)),
            BoardType::Buzzer => Board::Buzzer(at!(Buzzer, route, a)),
            BoardType::Vibro => Board::Vibro(at!(Vibro, route, a)),
            BoardType::Pixels => Board::Pixels(at!(Pixels, route, a)),
            BoardType::Knob => Board::Knob(at!(Knob, route, a)),
            BoardType::Joystick => Board::Joystick(at!(Joystick, route, a)),
            BoardType::OptoRelay => Board::OptoRelay(at!(OptoRelay, route, a)),
            BoardType::LatchRelay => Board::LatchRelay(at!(LatchRelay, route, a)),
            BoardType::LedMatrix => Board::LedMatrix(at!(LedMatrix, route, a)),
            BoardType::Motors => Board::Motors(at!(Motors, route, a)),
            BoardType::Thermo => Board::Thermo(Thermo::new(route)),
            BoardType::Pressure => Board::Pressure(at!(Lps22hb, route, a)),
            BoardType::Light => Board::Light(at!(Ltr381rgb, route, a)),
            BoardType::Movement => Board::Movement(at!(Lsm6dsox, route, a)),
            BoardType::Distance => Board::Distance(at!(Vl53l4, route, a), None),
        }
    }

    /// Check only that the board at the address is the one configured, with no
    /// set-up: used at stop before writing a safe state to a board that is not
    /// connected, so nothing is written to another device.
    pub fn verify(&mut self) -> Result<(), Failure> {
        match self {
            Board::Buttons(b) => b.verify()?,
            Board::Buzzer(b) => b.verify()?,
            Board::Vibro(b) => b.verify()?,
            Board::Pixels(b) => b.verify()?,
            Board::Knob(b) => b.verify()?,
            Board::Joystick(b) => b.verify()?,
            Board::OptoRelay(b) => b.verify()?,
            Board::LatchRelay(b) => b.verify()?,
            Board::LedMatrix(b) => b.verify()?,
            Board::Motors(b) => b.verify()?,
            Board::Thermo(b) => {
                b.verify()?;
            }
            Board::Pressure(b) => b.verify()?,
            Board::Light(b) => b.verify()?,
            Board::Movement(b) => b.verify()?,
            Board::Distance(b, _) => {
                b.verify()?;
            }
        }
        Ok(())
    }

    /// Check the board is the one configured and set it up: on every connect,
    /// the first and each one after the board was lost. It leaves outputs as
    /// they are, so a board that missed a transfer keeps what it was last
    /// commanded; a board that reset is in its power-on state anyway.
    pub fn setup(&mut self) -> Result<(), Failure> {
        let delay = &mut Sleep;
        match self {
            Board::Buttons(b) => b.verify()?,
            Board::Buzzer(b) => b.verify()?,
            Board::Vibro(b) => b.verify()?,
            Board::Pixels(b) => b.verify()?,
            Board::Knob(b) => b.verify()?,
            Board::Joystick(b) => b.verify()?,
            Board::OptoRelay(b) => b.verify()?,
            Board::LatchRelay(b) => b.verify()?,
            Board::LedMatrix(b) => b.verify()?,
            Board::Motors(b) => b.verify()?,
            Board::Thermo(b) => {
                b.verify()?;
            }
            Board::Pressure(b) => {
                b.init()?;
                first_sample("pressure sensor", FIRST_SAMPLE_WAIT, || {
                    Ok(b.pressure_ready()? && b.temperature_ready()?)
                })?;
            }
            Board::Light(b) => {
                b.init()?;
                first_sample("light sensor", FIRST_SAMPLE_WAIT, || Ok(b.ready()?))?;
            }
            Board::Movement(b) => {
                b.init()?;
                first_sample("movement sensor", FIRST_SAMPLE_WAIT, || {
                    Ok(b.acceleration_ready()? && b.angular_velocity_ready()?)
                })?;
            }
            Board::Distance(b, cache) => {
                *cache = None;
                b.init(delay)?;
                b.start_ranging(delay)?;
            }
        }
        Ok(())
    }

    /// Outputs to their safe state: every LED off, relays off (the latching
    /// relay included), motors stopped and stepper coils released, the buzzer
    /// and the vibration motor stopped, the distance sensor's ranging stopped.
    pub fn make_safe(&mut self) -> Result<(), Failure> {
        match self {
            Board::Buttons(b) => b.set_leds(false, false, false)?,
            Board::Buzzer(b) => b.no_tone()?,
            Board::Vibro(b) => b.off()?,
            Board::Pixels(b) => {
                b.clear_all();
                b.show()?;
            }
            Board::OptoRelay(b) => b.off()?,
            Board::LatchRelay(b) => b.off(&mut Sleep)?,
            Board::LedMatrix(b) => clear_matrix(b)?,
            Board::Motors(b) => match b.status() {
                Ok(s) if s.mode == MotorMode::Dc => b.stop()?,
                Ok(_) => b.release_coils()?,
                // The mode is unknown: send both, then report the read that failed.
                Err(e) => {
                    let stopped = b.stop();
                    let released = b.release_coils();
                    return Err(stopped.and(released).err().unwrap_or(e).into());
                }
            },
            _ => {}
        }
        Ok(())
    }

    /// At session end: the safe state, and the distance sensor stopped.
    pub fn stop(&mut self) -> Result<(), Failure> {
        if let Board::Distance(b, _) = self {
            b.stop_ranging()?;
        }
        self.make_safe()
    }

    pub fn read(&mut self, signal_ids: &[&str]) -> Result<Readings, Failure> {
        let wants = |id: &str| signal_ids.contains(&id);
        let mut out = Readings::new();
        let mut put = |id: &str, v: Reading| {
            out.insert(id.to_owned(), v);
        };
        match self {
            Board::Buttons(b) => {
                let s = b.read()?;
                put("a", s.a.into());
                put("b", s.b.into());
                put("c", s.c.into());
            }
            Board::Knob(b) => {
                let s = b.read()?;
                put("position", i64::from(s.position).into());
                put("pressed", s.pressed.into());
            }
            Board::Joystick(b) => {
                let s = b.read()?;
                put("x", i64::from(s.x).into());
                put("y", i64::from(s.y).into());
                put("pressed", s.pressed.into());
            }
            Board::OptoRelay(b) => put("on", b.is_on()?.into()),
            Board::LatchRelay(b) => put(
                "on",
                match b.state()? {
                    Some(on) => on.into(),
                    // The board reports nothing until it has been commanded since power-up.
                    None => Reading::new(false).quality(Quality::Unknown),
                },
            ),
            Board::LedMatrix(b) => put("mode", matrix_mode(b.mode()?).into()),
            Board::Motors(b) => {
                let s = b.status()?;
                put("mode", motor_mode(s.mode).into());
                put("busy", s.busy.into());
                put("current_a_raw", u64::from(s.current_a_raw).into());
                put("current_b_raw", u64::from(s.current_b_raw).into());
            }
            Board::Thermo(b) => {
                let r = b.read(&mut Sleep)?;
                put("temperature", f64::from(r.temperature).into());
                put("humidity", f64::from(r.humidity).into());
            }
            Board::Pressure(b) => {
                if wants("pressure") {
                    put("pressure", f64::from(b.pressure()?).into());
                }
                if wants("temperature") {
                    put("temperature", f64::from(b.temperature()?).into());
                }
            }
            Board::Light(b) => {
                let r = b.read()?;
                put("lux", f64::from(r.lux).into());
                put("red", u64::from(r.red).into());
                put("green", u64::from(r.green).into());
                put("blue", u64::from(r.blue).into());
                put("ir", u64::from(r.ir).into());
            }
            Board::Movement(b) => {
                if signal_ids.iter().any(|id| id.starts_with("accel_")) {
                    let a = b.acceleration()?;
                    put("accel_x", f64::from(a.x).into());
                    put("accel_y", f64::from(a.y).into());
                    put("accel_z", f64::from(a.z).into());
                }
                if signal_ids.iter().any(|id| id.starts_with("gyro_")) {
                    let g = b.angular_velocity()?;
                    put("gyro_x", f64::from(g.x).into());
                    put("gyro_y", f64::from(g.y).into());
                    put("gyro_z", f64::from(g.z).into());
                }
            }
            Board::Distance(b, cache) => {
                let (r, at) = latest_range(b, cache)?;
                let at_time = |v: u64| Reading::new(v).observed_at(at);
                put("distance_mm", at_time(u64::from(r.distance_mm)));
                put("range_status", at_time(u64::from(r.status)));
                put("signal_kcps", at_time(u64::from(r.signal_kcps)));
                put("sigma_mm", at_time(u64::from(r.sigma_mm)));
            }
            Board::Buzzer(_) | Board::Vibro(_) | Board::Pixels(_) => {}
        }
        out.retain(|id, _| wants(id));
        Ok(out)
    }

    pub fn call(&mut self, function: &str, args: &Args) -> Result<Results, Failure> {
        // The SDK has checked every argument's presence, type and bounds.
        let bool_arg = |n: &str| args.bool(n).expect("a declared bool argument");
        let u64_arg = |n: &str| args.u64(n).expect("a declared uint64 argument");
        let i64_arg = |n: &str| args.i64(n).expect("a declared int64 argument");
        let narrow = |v: u64| u32::try_from(v).expect("bounded by the declaration");
        let byte = |v: u64| u8::try_from(v).expect("bounded by the declaration");
        match (self, function) {
            (Board::Buttons(b), "set_leds") => b.set_leds(bool_arg("a"), bool_arg("b"), bool_arg("c"))?,
            (Board::Buzzer(b), "tone") => b.tone(narrow(u64_arg("frequency_hz")), narrow(u64_arg("duration_ms")))?,
            (Board::Buzzer(b), "no_tone") => b.no_tone()?,
            (Board::Vibro(b), "vibrate") => b.on_with_power(narrow(u64_arg("duration_ms")), byte(u64_arg("power")))?,
            (Board::Vibro(b), "off") => b.off()?,
            (Board::Pixels(b), name @ ("set" | "set_all")) => {
                let colour = Color::new(byte(u64_arg("red")), byte(u64_arg("green")), byte(u64_arg("blue")));
                let brightness = byte(u64_arg("brightness"));
                if name == "set" {
                    b.set(usize::from(byte(u64_arg("index"))), colour, brightness)?;
                } else {
                    b.set_all(colour, brightness)?;
                }
                b.show()?;
            }
            (Board::Pixels(b), "off") => {
                b.clear_all();
                b.show()?;
            }
            (Board::Knob(b), "set_position") => {
                b.set_position(i16::try_from(i64_arg("position")).expect("bounded by the declaration"))?;
            }
            (Board::Joystick(b), "set_deadzone") => b.set_deadzone(byte(u64_arg("deadzone"))),
            (Board::OptoRelay(b), "set") => b.set(bool_arg("on"))?,
            (Board::LatchRelay(b), "set") => b.set(bool_arg("on"), &mut Sleep)?,
            (Board::LedMatrix(b), "clear") => clear_matrix(b)?,
            (Board::LedMatrix(b), "show_mono") => {
                let pixels = mono_picture(args.bytes("pixels").expect("a declared bytes argument"))?;
                if b.mode()? != MatrixMode::Mono {
                    b.set_mode(MatrixMode::Mono)?;
                }
                b.show_mono(&pixels)?;
            }
            (Board::LedMatrix(b), "show_gray") => {
                let levels = gray_picture(args.bytes("levels").expect("a declared bytes argument"))?;
                if b.mode()? != MatrixMode::Grayscale {
                    b.set_mode(MatrixMode::Grayscale)?;
                }
                b.show_gray(&levels)?;
            }
            (Board::Motors(b), "set_speeds") => {
                let speed = |n| i16::try_from(i64_arg(n)).expect("bounded by the declaration");
                b.set_speeds(speed("a"), speed("b"))?;
            }
            (Board::Motors(b), "stop") => b.stop()?,
            (Board::Motors(b), "step") => {
                let steps = i32::try_from(i64_arg("steps")).expect("bounded by the declaration");
                let period = u16::try_from(u64_arg("period_ticks")).expect("bounded by the declaration");
                b.step(steps, period, 0)?;
            }
            (Board::Motors(b), "release_coils") => b.release_coils()?,
            (Board::Motors(b), "set_mode") => {
                let mode = match args.str("mode") {
                    Some("stepper") => MotorMode::Stepper,
                    _ => MotorMode::Dc,
                };
                b.set_mode(mode)?;
            }
            (_, other) => unreachable!("the SDK passes only declared functions, not {other}"),
        }
        Ok(Results::new())
    }
}

fn clear_matrix(b: &mut LedMatrix<Route>) -> Result<(), Error<BusError>> {
    match b.mode()? {
        MatrixMode::Mono => b.show_mono(&[[false; COLS]; ROWS]),
        MatrixMode::Grayscale => b.show_gray(&[[0; COLS]; ROWS]),
    }
}

fn matrix_mode(mode: MatrixMode) -> &'static str {
    match mode {
        MatrixMode::Mono => "mono",
        MatrixMode::Grayscale => "grayscale",
    }
}

fn motor_mode(mode: MotorMode) -> &'static str {
    match mode {
        MotorMode::Dc => "dc",
        MotorMode::Stepper => "stepper",
    }
}

fn mono_picture(bytes: &[u8]) -> Result<[[bool; COLS]; ROWS], ProviderError> {
    if bytes.len() != MONO_BYTES {
        return Err(ProviderError::new(
            StatusCode::InvalidArgument,
            format!("pixels must be {MONO_BYTES} bytes, got {}", bytes.len()),
        ));
    }
    let mut picture = [[false; COLS]; ROWS];
    for (i, led) in picture.iter_mut().flatten().enumerate() {
        *led = bytes[i / 8] & (0x80 >> (i % 8)) != 0;
    }
    Ok(picture)
}

fn gray_picture(bytes: &[u8]) -> Result<[[u8; COLS]; ROWS], ProviderError> {
    if bytes.len() != GRAY_BYTES {
        return Err(ProviderError::new(
            StatusCode::InvalidArgument,
            format!("levels must be {GRAY_BYTES} bytes, got {}", bytes.len()),
        ));
    }
    if let Some(level) = bytes.iter().find(|&&l| l > 15) {
        return Err(ProviderError::new(StatusCode::OutOfRange, format!("a level is 0-15, got {level}")));
    }
    let mut picture = [[0; COLS]; ROWS];
    for (i, led) in picture.iter_mut().flatten().enumerate() {
        *led = bytes[i];
    }
    Ok(picture)
}

/// Wait for a sensor's first sample after `init`, until which its data
/// registers hold their reset values.
fn first_sample(what: &str, wait: Duration, mut ready: impl FnMut() -> Result<bool, Failure>) -> Result<(), Failure> {
    let deadline = Instant::now() + wait;
    loop {
        if ready()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Failure::Unreachable(format!("the {what} has produced no sample")));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The newest range: a new measurement if the sensor has one, otherwise the
/// last one with its own time. Waits briefly for the first after connect.
fn latest_range(
    b: &mut Vl53l4<Route>,
    cache: &mut Option<(DistanceReading, SystemTime)>,
) -> Result<(DistanceReading, SystemTime), Failure> {
    let deadline = Instant::now() + FIRST_RANGE_WAIT;
    loop {
        if b.data_ready()? {
            let reading = b.read()?;
            *cache = Some((reading, SystemTime::now()));
            break;
        }
        if cache.is_some() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cache.ok_or_else(|| Failure::Unreachable("the distance sensor has produced no range".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_waits_for_the_flag() {
        let mut polls = 0;
        let r = first_sample("sensor", Duration::from_secs(1), || {
            polls += 1;
            Ok(polls == 3)
        });
        assert!(r.is_ok());
        assert_eq!(polls, 3);
    }

    #[test]
    fn first_sample_gives_up_unreachable() {
        let r = first_sample("sensor", Duration::from_millis(20), || Ok(false));
        assert!(matches!(r, Err(Failure::Unreachable(m)) if m.contains("no sample")));
    }

    #[test]
    fn every_type_declares() {
        for t in BoardType::ALL {
            assert_eq!(device_type(t).id(), t.id());
        }
    }

    /// The text of a `- Label:` bullet in a section, up to the next bullet.
    fn bullet<'a>(section: &'a str, label: &str) -> &'a str {
        let Some(start) = section.find(&format!("- {label}:")) else { return "" };
        let rest = &section[start + label.len() + 3..];
        &rest[..rest.find("\n- ").unwrap_or(rest.len())]
    }

    fn code_spans(text: &str) -> impl Iterator<Item = &str> {
        text.split('`').skip(1).step_by(2)
    }

    /// docs/boards.md has a section per board type whose Signals bullet names
    /// exactly the declared signals, and whose Functions bullet names exactly
    /// the declared functions, each with its argument names in order.
    #[test]
    fn docs_list_every_signal_and_function() {
        let doc = include_str!("../docs/boards.md");
        for t in BoardType::ALL {
            let header = format!("\n## {}\n", t.id());
            let start = doc.find(&header).unwrap_or_else(|| panic!("docs/boards.md has no section for {}", t.id()));
            let rest = &doc[start + header.len()..];
            let section = &rest[..rest.find("\n## ").unwrap_or(rest.len())];
            let declared = device_type(t);

            let mut documented: Vec<&str> = code_spans(bullet(section, "Signals")).collect();
            documented.sort_unstable();
            let mut signals: Vec<&str> = declared.signals().iter().map(|s| s.id()).collect();
            signals.sort_unstable();
            assert_eq!(documented, signals, "{}: documented signals differ from declared", t.id());

            let mut documented: Vec<(String, Vec<String>)> = code_spans(bullet(section, "Functions"))
                .filter_map(|code| code.split_once('('))
                .map(|(name, args)| {
                    let args = args.trim_end_matches(')').split(',').map(str::trim).filter(|a| !a.is_empty());
                    (name.to_owned(), args.map(str::to_owned).collect())
                })
                .collect();
            documented.sort();
            let mut functions: Vec<(String, Vec<String>)> = declared
                .functions()
                .iter()
                .map(|f| (f.name().to_owned(), f.args().iter().map(|a| a.name().to_owned()).collect()))
                .collect();
            functions.sort();
            assert_eq!(documented, functions, "{}: documented functions differ from declared", t.id());
        }
    }

    #[test]
    fn a_mono_picture_is_row_major_msb_first() {
        let mut bytes = [0u8; MONO_BYTES];
        bytes[0] = 0x80; // row 0, column 0
        bytes[1] = 0x08; // bit 12: row 1, column 0
        let p = mono_picture(&bytes).unwrap();
        assert!(p[0][0] && p[1][0]);
        assert_eq!(p.iter().flatten().filter(|&&on| on).count(), 2);
    }

    #[test]
    fn pictures_of_the_wrong_length_or_level_are_refused() {
        assert!(mono_picture(&[0; 11]).is_err());
        assert_eq!(gray_picture(&[16; GRAY_BYTES]).unwrap_err().code, StatusCode::OutOfRange);
        assert_eq!(gray_picture(&[0; 95]).unwrap_err().code, StatusCode::InvalidArgument);
    }

    #[test]
    fn a_missing_board_is_unreachable_and_a_bad_value_is_not() {
        assert!(matches!(classify(Error::Timeout), Failure::Unreachable(_)));
        assert!(matches!(
            classify(Error::OutOfRange),
            Failure::Status(ProviderError { code: StatusCode::OutOfRange, .. })
        ));
        assert!(matches!(
            classify(Error::ReservedPrefix),
            Failure::Status(ProviderError { code: StatusCode::InvalidArgument, .. })
        ));
    }
}
