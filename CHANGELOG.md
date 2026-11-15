# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A provider for Arduino Modulino boards on modulino-driver and the Rust
  provider SDK: buttons, buzzer, vibro, pixels, knob, joystick, opto and latch
  relays, LED matrix, motors, thermo, pressure, light, movement and distance,
  on a Linux I2C bus or behind a Modulino Hub port. Each output is put in its
  safe state the first time its board is connected and, for every board, when
  the session ends.
- Mock mode (`mock: true`) on modulino-driver's simulated boards. In it the
  provider passes the anolis-protocol 1.10.0 conformance harness at level 2,
  with four skips listed in `scripts/conformance.py`: no function declares
  results or takes a double argument.
- Not yet run on hardware: neither modulino-driver nor this provider has been
  run on a Modulino board (docs/hardware.md).
