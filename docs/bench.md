# Bench run

How to run this provider on the boards, for
[#5](https://github.com/anolishq/anolis-provider-modulino/issues/5):
modulino-driver's own bench checklist first, then every board through the
Anolis runtime. The results go on that issue, and a board moves to `tested` on
the site's Hardware support table only from them
([hardware.md](hardware.md#hardware-status)).

Nothing here has run on a Modulino board yet. The configs and
`scripts/bench.py` have run only against the simulated boards (`mock: true`),
with anolis-runtime 0.16.0.

## Hosts

Run it on a Raspberry Pi 5 and on a Pi 4 B. Their I2C controllers have
different kernel drivers (`i2c-designware` and `i2c-bcm2835`; the driver's
checklist has the table), and how each reports an address nobody answers is
one of the open questions. A Pi 3 A+ uses the same driver as the Pi 4 B, so a
run there repeats it on a slower host with less memory.

The runtime is released for 64-bit Linux only (`linux-arm64`), so each Pi needs
the 64-bit Raspberry Pi OS.

## Order, on each host

1. modulino-driver's bench checklist (its
   [`docs/bench.md`](https://github.com/CameronBrooks11/modulino-driver/blob/main/docs/bench.md)).
   Its step 1.1 measures whether the Pi reports an empty address as
   `EREMOTEIO`, which this provider maps to a NACK
   ([hardware.md](hardware.md#host-preparation)).
2. Every board through the runtime: [All boards](#all-boards) below.
3. The [hub step](#hub-step).

## Set up

On the Pi, with I2C enabled and Rust 1.85 or later (the driver checklist's
setup steps 1 and 2 cover both):

```bash
git clone https://github.com/anolishq/anolis-provider-modulino
cd anolis-provider-modulino
cargo build --release --locked
```

If the build runs out of memory (the Pi 3 A+ has 512 MB), build on the Pi 5
with `just release` and copy
`target/aarch64-unknown-linux-musl/release/anolis-provider-modulino`, which is
static, into this clone's `target/release/` on the smaller Pi.

The runtime, from its release, into this clone:

```bash
ANOLIS=0.16.0
curl -fsSLO https://github.com/anolishq/anolis/releases/download/v$ANOLIS/anolis-$ANOLIS-linux-arm64.tar.gz
curl -fsSL -o anolis.SHA256SUMS https://github.com/anolishq/anolis/releases/download/v$ANOLIS/SHA256SUMS
sha256sum -c --ignore-missing anolis.SHA256SUMS
tar -xzf anolis-$ANOLIS-linux-arm64.tar.gz   # bin/anolis-runtime
```

## All boards

1. With the Pi off, chain one of each board on the header (wiring in
   [hardware.md](hardware.md#wiring)). Put the two DC motors on the Motors
   board's outputs, free to turn, and have a stepper ready to swap in.
2. Power on and list the addresses:

   ```bash
   i2cdetect -y -a 1 0x01 0x77
   ```

   Compare them with `config/bench.yaml`, and give an `address` to any board
   found away from its factory address there.
3. Check the host and the config:

   ```bash
   ./target/release/anolis-provider-modulino --check-host config/bench.yaml
   ./target/release/anolis-provider-modulino --check-config config/bench.yaml
   ```

4. Start the runtime from the root of the clone, and leave it running:

   ```bash
   bin/anolis-runtime --config config/bench-runtime.yaml
   ```

   At startup it runs the safe state in `config/bench-runtime.yaml`, so every
   output should be off.
5. In a second terminal:

   ```bash
   scripts/bench.py
   ```

   It reads every board and calls every function, saying what to look for,
   and asks what you saw. It pauses to swap the DC motors for the stepper and
   back. Then it turns outputs on, runs the runtime's software stop
   (`POST /v0/estop`), and asks whether everything went off. Last, it asks you
   to unplug and replug, one at a time, the Buttons board and the three
   sensors: the first reading after each replug must be a real one, not
   zeros ([#7](https://github.com/anolishq/anolis-provider-modulino/issues/7)).
   Answers go into `target/bench-results.md`.
6. Stop the runtime with Ctrl-C. The provider then puts every output in its
   safe state again: check that.

## Hub step

Only the Modulino Hub on the Pi's bus, at 0x70, with the Buttons board on
port 0 and the opto relay on port 1:

```bash
bin/anolis-runtime --config config/bench-hub-runtime.yaml
scripts/bench.py --unplug buttons0 --out target/bench-results-hub.md
```

Each board must answer through its own port only; the driver checklist's step
2.2 checks the same thing below the provider.

## Record

Post on [#5](https://github.com/anolishq/anolis-provider-modulino/issues/5) a
comment for each host: both results files, the provider and driver commits
(`git rev-parse HEAD`; `Cargo.lock` pins the driver), the runtime version, and
anything that did not match what the script said to look for. A mismatch is an
issue of its own, linked from there.

## Not covered here

When the Motors board's mode cannot be read at stop, the provider sends the
stop and then the release. The tests check that the release is sent, but not
that the stop went first, and a bench cannot produce a read that fails while
writes still land. That stays unverified.
