# Hardware

## Hardware status

**Nothing here has run on a Modulino board yet.** The boards this provider
supports are on order, and both layers under it are unverified on hardware:

- **modulino-driver** says so itself: "No module has been verified on hardware
  yet." Its protocol facts are read from Arduino's node firmware source and the
  chips' datasheets, with citations; its own bench checklist (`docs/bench.md` in
  that repository) is what will verify them.
- **This provider** has run only against modulino-driver's simulated boards
  (`mock: true`). Those models follow the same firmware source and datasheets,
  and are themselves unverified on hardware.

What the tests and the conformance harness do show: the provider speaks ADPP
correctly (anolis-protocol 1.10.0 harness at level 2), maps each read and call
onto the driver as [boards.md](boards.md) says, routes through a hub port, and
reports a board that does not answer as `UNREACHABLE`. What they cannot show is
that a real board behaves as the models do: the frame lengths and timings, a
sensor's readings, or what a relay or motor actually does.

The [Hardware support](https://anolishq.github.io/get-started/hardware) table is
the one place each board's status is kept. A board moves to `tested` only when
it has run on real hardware, driven by the Anolis runtime through this provider,
with the release, the date and what was run recorded there. Two runs come first,
in this order:

1. modulino-driver's bench checklist, on a Raspberry Pi with the boards on its
   I2C header. It settles the open questions the driver lists (among them
   whether the Pi reports an unanswered address as a NACK; see
   [Host preparation](#host-preparation)).
2. This provider under the runtime, for each board: reads, each function, the
   runtime's software stop on the outputs, and a board unplugged and plugged
   back in while the provider runs.

## Wiring

Modulino boards use the Qwiic/STEMMA QT connector (4-pin JST SH) at 3.3 V and
daisy-chain on one cable run. On a Raspberry Pi, use a Qwiic-to-female-jumper
cable or a Qwiic adapter on the header:

| Cable wire | Pi header pin |
| --- | --- |
| red, 3.3 V | 1 |
| blue, SDA | 3 (GPIO 2) |
| yellow, SCL | 5 (GPIO 3) |
| black, GND | 6 |

Each board on one cable run needs its own address. Two boards of the same kind
share a default address: put them on two ports of a Modulino Hub, or change one
board's address with modulino-driver's `service` module, and give the new
address in the config.

## Host preparation

- Enable I2C (on a Raspberry Pi: `raspi-config`, Interface Options, I2C) and
  reboot. `/dev/i2c-1` must exist.
- Run the provider as a user in the `i2c` group, so it can open the bus.
- `anolis-provider-modulino --check-host <config>` reports both: it opens the
  bus device and does no bus transfer.

On a Raspberry Pi the I2C controller reports an address that nothing answers as
`EREMOTEIO`, which linux-embedded-hal 0.5 does not treat as a NACK. The
provider maps `EREMOTEIO` to a NACK itself, so an absent board reads as
`UNREACHABLE` rather than as a bus fault. That is read from the kernel drivers'
source; the driver's bench checklist measures it.

## When the provider exits

Every output is put in its safe state (see [boards.md](boards.md)) when the
provider connects a board, and again when its session ends:

- the runtime stops it, shuts down or crashes, so the provider's stdin closes;
- the provider receives `SIGTERM`;
- the session fails: a malformed frame, a closed output, or an error in the
  serving loop.

A board that does not answer at that moment is logged and skipped, and the
other boards are still made safe.

Nothing makes the outputs safe if the provider is killed with `SIGKILL`
(including systemd's `SIGKILL` after a stop times out) or the host loses power
with the boards still powered. The provider arms no watchdog on any board, and
whether a board stops an output on its own after losing its host has not been
established; assume each output holds its last state until the board loses
power. The latch relay keeps its contacts even then. Wire a cut-off on the
supply of anything a board switches or drives, for an emergency stop.
