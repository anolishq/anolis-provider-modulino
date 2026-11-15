# anolis-provider-modulino

An [Anolis](https://github.com/anolishq/anolis) device provider for
[Arduino Modulino](https://docs.arduino.cc/hardware/modulinos/) boards. It puts
the [Rust provider SDK](https://github.com/anolishq/anolis-provider-sdk-rust)
on top of [modulino-driver](https://github.com/CameronBrooks11/modulino-driver),
a `no_std` Rust driver for the boards, and talks to them over a Linux I2C bus.

> **Status: unreleased, and not yet run on hardware.** It passes the
> anolis-protocol 1.10.0 conformance harness at level 2 in mock mode, against
> modulino-driver's simulated boards. Neither modulino-driver nor this provider
> has been run on a Modulino board yet; see
> [docs/hardware.md](docs/hardware.md#hardware-status). Which boards have run on
> real hardware, and on which release:
> [Hardware support](https://anolishq.github.io/get-started/hardware).

## Boards

| `type` | Board | Default address |
| --- | --- | --- |
| `buttons` | Buttons: three buttons with LEDs | `0x3E` |
| `buzzer` | Buzzer | `0x1E` |
| `vibro` | Vibro: vibration motor | `0x38` |
| `pixels` | Pixels: eight RGB LEDs | `0x36` |
| `knob` | Knob: rotary encoder with a push button | `0x3A`; a board strapped for `0x3B` needs `address: 0x3B` |
| `joystick` | Joystick | `0x2C` |
| `opto_relay` | Opto Relay | `0x14` |
| `latch_relay` | Latch Relay | `0x02` |
| `led_matrix` | LED Matrix: 12 by 8 | `0x39` |
| `motors` | Motors: two DC motors or one stepper | `0x24` |
| `thermo` | Thermo: temperature and humidity (HS3003, or SHT31) | found at `0x44` or `0x45` |
| `pressure` | Pressure (LPS22HB) | `0x5C` |
| `light` | Light: ambient light and colour (LTR-381RGB) | `0x53` |
| `movement` | Movement: accelerometer and gyroscope (LSM6DSOX) | `0x6A`, else `0x6B` |
| `distance` | Distance: time of flight (VL53L4CD or VL53L4ED) | `0x29` |

Each board's signals and functions, and the state its outputs are put in when
the provider connects and when it stops, are in [docs/boards.md](docs/boards.md).

A [Modulino Hub](https://docs.arduino.cc/hardware/modulinos/) (TCA9548A) on the
bus gives each of its eight ports its own bus, so two boards with the same
address can sit on two ports. Hubs must be directly on the bus: a hub behind
another hub's port is not supported.

## Config

```yaml
bus: /dev/i2c-1
devices:
  - id: panel_buttons
    type: buttons
  - id: room
    type: thermo
  - id: left_relay
    type: latch_relay
    hub: { address: 0x70, port: 0 }
  - id: right_relay
    type: latch_relay
    hub: { address: 0x70, port: 1 }
```

`address` overrides a board's default address, for a board whose address has
been changed (modulino-driver's `service` module does that; this provider never
does). `mock: true` replaces the bus with modulino-driver's simulated boards, so
the provider runs without hardware. The full schema:
`anolis-provider-modulino --config-schema`. Beyond the schema, `--check-config`
refuses a device id used twice, two boards on the same address of one bus or
hub port, a board on a hub's address, and an `address` on a `thermo` board
(which finds its own).

A board that does not answer, answers as another kind of board, or does not
finish starting is listed anyway and reported `UNREACHABLE`; the provider tries
it again on its next read or call.

## Develop

```sh
just check   # rustfmt, clippy
just test    # unit tests, then the conformance harness in mock mode
```

[docs/](docs/index.md) has the boards' contract, wiring and host preparation,
and the hardware status.

## Licence

Apache-2.0.
