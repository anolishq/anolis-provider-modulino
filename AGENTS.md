# AGENTS.md: anolis-provider-modulino

An Anolis device provider for Arduino Modulino boards: the Rust provider SDK on
top of modulino-driver, over a Linux I2C bus.

## Build / test

- `just check` (rustfmt, clippy with `-D warnings`) and `just test` (unit tests, then the conformance harness against
  the debug build in mock mode). CI also builds the static musl binary on x86_64 and arm64 and runs the harness
  against it.
- The required CI status check is the **`ok`** job; never merge red.
- `scripts/conformance.py` requires the harness to pass with no skips beyond `EXPECTED_SKIPS`, each with its reason.
  The harness reads and calls the *first* device in `config/conformance.yaml` that declares signals or functions:
  keep a board first that has signals, a function with required arguments and one without (motors).

## Design rules

- **The SDK on top of the driver.** `src/board.rs` declares each board and calls modulino-driver; no protocol code
  (frames, registers, addresses' meaning) in this repository. A missing or wrong protocol fact is fixed in
  modulino-driver, with its citation, not worked around here.
- **Vendor drivers follow the rules in the SDK guide's
  [Vendor drivers](https://github.com/anolishq/anolis-provider-sdk-python/blob/main/docs/writing-a-provider.md#vendor-drivers)
  section** (written for the Python SDK; the rules are the same). modulino-driver is the in-house driver for a vendor
  with no Rust driver: it lives in its own repository and is pinned here by commit until it is tagged.
- **Mock mode runs the real driver** on modulino-driver's `sim::SimBus`. Never stub the driver.
- **Only a board that does not answer is UNREACHABLE.** `classify` in `src/board.rs`: a bus error, a wrong identity
  or chip, or a timeout marks the board lost and it is retried; any other driver error is answered with its status.
- **Outputs: safe at connect and at stop.** `Board::make_safe`; the list is in `docs/boards.md`.
- **Only what the config names.** No scanning, no address changes (that is modulino-driver's `service`, an operator
  step), no nested hubs.
- **The board contract is public.** Signals and functions are what automation is written against: change them only
  with the maintainer's approval, and keep `docs/boards.md` in step (a test checks it).

## Repo-specific gotchas

- **Hub ports and `Send`.** modulino-driver's `HubPort` borrows a `&RefCell` bus, which is not `Send`, while the SDK's
  `Provider` must be. `Route` (`src/bus.rs`) owns the bus behind an `Arc<Mutex>` and builds the driver's `Hub` for
  the length of each transaction. Keep hub logic in the driver.
- **`EREMOTEIO` is a NACK.** linux-embedded-hal 0.5 maps only `ENXIO`/`ENODEV` to `NoAcknowledge`; the Pi's I2C
  controllers return `EREMOTEIO` (121). `BusError::linux` maps it, or an absent board would be a fault.
- **linux-embedded-hal with `default-features = false, features = ["i2c"]`.** Its defaults pull in serial and GPIO
  crates; `i2c` alone keeps the release binary static.
- **The simulated Motors board always reads idle in dc mode** (a SimBus limit), so the stepper branch of
  `make_safe` never runs in mock mode.
- **Hardware status lives on the docs site's Hardware support table**, not here; `docs/hardware.md` says what has
  and has not run.
- The SDK is pinned by release tag in `Cargo.toml`; `Cargo.lock` records the commit the tag resolved to.
