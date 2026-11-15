# Boards

What each board type exposes to the runtime. Signals are read; functions are
called with the arguments shown, which the SDK checks against their bounds
before the provider sees them. Every signal is in the default read set unless
marked *not default*. A test (`docs_list_every_signal_and_function` in
`src/board.rs`) checks that each board's Signals line names exactly its declared
signals, and its Functions line exactly its declared functions with their
argument names; the types, ranges and descriptions here are not checked.

**Safe state** is what the provider puts a board's outputs in the first time it
connects the board, and again when its session ends (see
[hardware.md](hardware.md)). A board that misses a transfer and is connected
again keeps what it was last commanded.

## buttons

- Signals: `a`, `b`, `c` (bool): each button is pressed.
- Functions: `set_leds(a, b, c)` (bool each, ACTUATE): light or clear the LED
  above each button.
- Safe state: LEDs off.

## buzzer

- Functions: `tone(frequency_hz, duration_ms)` (180 to 5 898 150 Hz; 0 ms plays
  until the next call; ACTUATE), `no_tone()` (ACTUATE). A frequency whose frame
  the firmware would take as a command (17 987 Hz is one) is refused with
  `INVALID_ARGUMENT`.
- Safe state: no tone.

## vibro

- Functions: `vibrate(duration_ms, power)` (1 ms or more; power 0-100 %;
  ACTUATE), `off()` (ACTUATE).
- Safe state: off.

## pixels

- Functions: `set(index, red, green, blue, brightness)` (LED 0-7; each colour
  0-255; brightness 0-100 %; ACTUATE), `set_all(red, green, blue, brightness)`
  (ACTUATE), `off()` (ACTUATE). `set` and `set_all` show the frame at once.
- Safe state: every LED off.

## knob

- Signals: `position` (int64; wraps at -32768 and 32767), `pressed` (bool).
- Functions: `set_position(position)` (-32768 to 32767, CONFIG): call the current
  position this value. The offset is kept by the provider, not the board, so it
  starts again from the board's count when the provider restarts.

## joystick

- Signals: `x`, `y` (int64, -127 to 127, 0 at rest), `pressed` (bool).
- Functions: `set_deadzone(deadzone)` (0-255 raw counts, CONFIG): readings this
  close to the centre read as 0. Kept by the provider (default 26).

## opto_relay

- Signals: `on` (bool).
- Functions: `set(on)` (ACTUATE).
- Safe state: off.

## latch_relay

- Signals: `on` (bool). Quality UNKNOWN until the relay has been commanded
  since the board powered up: the board does not report its contacts before
  then.
- Functions: `set(on)` (ACTUATE). Takes 120 ms: the firmware pulses the coil,
  and a transfer to the board in that time can drive the wrong coil, so the
  provider waits it out.
- Safe state: off. The relay latches: it keeps its contacts without power, so
  if the provider is killed before it can switch the relay off, the relay stays
  where it was.

## led_matrix

- Signals: `mode` (string: mono or grayscale): the mode the provider last read
  or set, read from the board each time the board is connected.
- Functions: `clear()` (ACTUATE), `show_mono(pixels)` (12 bytes, one bit per
  LED, row by row from the top left, most significant bit first; ACTUATE),
  `show_gray(levels)` (96 bytes, one level 0-15 per LED, row by row from the top
  left; ACTUATE). Each `show_` switches the board to its mode first. A picture
  whose frame the firmware would take as a mode command is refused with
  `INVALID_ARGUMENT`.
- Safe state: cleared, in its current mode.

## motors

- Signals: `mode` (string: dc or stepper), `busy` (bool: a stepper move is
  running), `current_a_raw`, `current_b_raw` (uint64, ADC counts above idle).
- Functions: `set_speeds(a, b)` (-32767 to 32767 each, dc mode, ACTUATE; 0 is
  stopped, so the runtime's software stop can call it with 0), `stop()` (dc
  mode, ACTUATE), `step(steps, period_ticks)` (signed steps; one step every
  `period_ticks` × 0.1 ms, 1-65535; stepper mode; the coils stay energised
  afterwards; ACTUATE), `release_coils()` (stepper mode, ACTUATE),
  `set_mode(mode)` (`dc` or `stepper`, CONFIG).
- Safe state: in dc mode, both motors stopped; in stepper mode, coils released.
  If the board's mode cannot be read, both are sent.

## thermo

- Signals: `temperature` (degC), `humidity` (%), doubles.

## pressure

- Signals: `pressure` (hPa), `temperature` (degC), doubles.

## light

- Signals: `lux` (double); `red`, `green`, `blue`, `ir` (uint64 raw counts, *not
  default*).

## movement

- Signals: `accel_x`, `accel_y`, `accel_z` (g), `gyro_x`, `gyro_y`, `gyro_z`
  (deg/s), doubles.

## distance

- Signals: `distance_mm`, `range_status` (uint64; status 0 is a valid
  measurement); `signal_kcps`, `sigma_mm` (uint64, *not default*). A read
  returns the newest range, with the time it was measured; a range more than
  1 s old is reported STALE.
- Ranging starts when the provider connects the board and stops when its session
  ends.
