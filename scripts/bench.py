#!/usr/bin/env python3
"""Drive the bench run (docs/bench.md) through the Anolis runtime's HTTP API.

    scripts/bench.py [--url URL] [--provider ID] [--out FILE] [--no-prompt]

For each board the runtime lists under the provider: read every signal, then
call each function with the arguments in CALLS. After each step it asks what
you saw, and writes every answer to a Markdown results file. Then it runs
the runtime's software stop with the outputs on, takes each --unplug board
off the bus and puts it back, and leaves outputs on for the provider's own
stop when the runtime is stopped. --no-prompt runs the calls and the
software stop, skips the rest, and records nothing observed: a check of the
script and the configs against mock mode.

If the script stops early (Ctrl-C, an error), it runs the software stop
before it exits.

Uses only the standard library, so it runs on the bench host as is.
"""

from __future__ import annotations

import argparse
import base64
import datetime
import json
import platform
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]

# What each function is called with on the bench, and what to look for. A
# function named PAUSE is a step for the person at the bench, not a call.
PAUSE = "pause"
CALLS: dict[str, list[tuple[str, dict, str]]] = {
    "buttons": [("set_leds", {"a": True, "b": False, "c": True}, "LEDs A and C on, B off")],
    "buzzer": [
        ("tone", {"frequency_hz": 440, "duration_ms": 0}, "a steady 440 Hz tone"),
        ("no_tone", {}, "silence"),
    ],
    "vibro": [
        ("vibrate", {"duration_ms": 2000, "power": 50}, "about 2 s of vibration at half power"),
        ("off", {}, "nothing (it already stopped)"),
    ],
    "pixels": [
        ("set", {"index": 0, "red": 255, "green": 0, "blue": 0, "brightness": 20}, "LED 0 red"),
        ("set_all", {"red": 0, "green": 0, "blue": 255, "brightness": 20}, "all 8 LEDs blue"),
        ("off", {}, "all LEDs off"),
    ],
    "knob": [("set_position", {"position": 0}, "nothing yet: the read after shows it")],
    "joystick": [("set_deadzone", {"deadzone": 100}, "nothing yet: the read after shows it")],
    "opto_relay": [("set", {"on": True}, "the relay closes (click, or continuity)")],
    "latch_relay": [
        ("set", {"on": True}, "the relay latches on"),
        ("set", {"on": False}, "the relay latches off"),
    ],
    "led_matrix": [
        ("show_mono", {"pixels": bytes([0xFF] * 12)}, "every LED on"),
        ("show_gray", {"levels": bytes(range(16)) * 6}, "a brightness ramp, one level per LED"),
        ("clear", {}, "every LED off"),
    ],
    "motors": [
        (PAUSE, {}, "Put the two DC motors on the outputs, free to turn"),
        ("set_mode", {"mode": "dc"}, "nothing visible"),
        ("set_speeds", {"a": 8000, "b": -8000}, "A turns one way, B the other, slowly"),
        ("stop", {}, "both stop"),
        (PAUSE, {}, "Swap the DC motors for a stepper on the outputs"),
        ("set_mode", {"mode": "stepper"}, "nothing visible"),
        ("step", {"steps": 50, "period_ticks": 100}, "the stepper turns 50 steps, 10 ms each"),
        ("release_coils", {}, "the stepper shaft turns freely by hand"),
        ("set_mode", {"mode": "dc"}, "nothing visible"),
        (PAUSE, {}, "Swap the stepper back for the DC motors, for the software stop"),
    ],
}

# What to do with a board whose readings follow an input, before reading it.
INPUTS: dict[str, str] = {
    "buttons": "hold button B",
    "knob": "turn the knob a little, and hold it pressed",
    "joystick": "push the stick to one corner and press it",
    "distance": "hold a flat target a measured distance away",
    "movement": "hold the board still and flat",
}

# What to do after a board's calls, before reading it again.
AFTER: dict[str, str] = {
    "knob": "leave the knob where it is (position should read 0)",
    "joystick": "push the stick a little off centre (with a deadzone of 100, it should read 0)",
}

# Outputs left on for the software stop: each has a hook in
# config/bench-runtime.yaml.
STOP_SETUP: list[tuple[str, str, dict]] = [
    ("buttons", "set_leds", {"a": True, "b": True, "c": True}),
    ("pixels", "set_all", {"red": 0, "green": 255, "blue": 0, "brightness": 20}),
    ("opto_relay", "set", {"on": True}),
    ("latch_relay", "set", {"on": True}),
    ("led_matrix", "show_mono", {"pixels": bytes([0xFF] * 12)}),
    ("buzzer", "tone", {"frequency_hz": 440, "duration_ms": 0}),
    # Long enough to still be running when the runtime is stopped by hand.
    ("vibro", "vibrate", {"duration_ms": 600000, "power": 50}),
    ("motors", "set_mode", {"mode": "dc"}),
    ("motors", "set_speeds", {"a": 8000, "b": 8000}),
]

# Outputs left on for the provider's own stop when the runtime stops: the
# same, but the motors in stepper mode with the coils held, so the provider's
# safe state has to read the mode and release them.
PROVIDER_STOP_SETUP: list[tuple[str, str, dict]] = [
    ("motors", PAUSE, {"text": "Swap the DC motors for the stepper again"}),
    ("motors", "set_mode", {"mode": "stepper"}),
    ("motors", "step", {"steps": 50, "period_ticks": 100}),
] + [entry for entry in STOP_SETUP if entry[0] != "motors"]

# Boards whose first reading after a reconnect must be a real sample (#7).
SENSORS = {"thermo", "pressure", "light", "movement", "distance"}


class Bench:
    def __init__(self, url: str, provider: str, prompt: bool) -> None:
        self.url = url.rstrip("/")
        self.provider = provider
        self.prompt = prompt
        self.rows: list[tuple[str, str, str, str]] = []
        self.caps: dict[str, dict] = {}
        self.stopped = False
        self.contacted = False

    def http(self, method: str, path: str, body: dict | None = None) -> dict:
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.url + path, data=data, method=method)
        if data is not None:
            request.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                self.contacted = True
                return json.load(response)
        except urllib.error.HTTPError as e:
            try:
                return json.load(e)
            except ValueError:
                return {"status": {"code": f"HTTP {e.code}", "message": e.reason}}
        except urllib.error.URLError as e:
            raise SystemExit(f"cannot reach the runtime at {self.url}: {e.reason}") from None
        except OSError as e:  # a timeout from a runtime that stopped answering
            raise SystemExit(f"no answer from the runtime at {self.url}: {e}") from None

    def ask(self, question: str) -> str:
        if not self.prompt:
            return "not observed (--no-prompt)"
        return input(f"  {question} ").strip() or "(no answer)"

    def record(self, step: str, device: str, result: str, observed: str) -> None:
        self.rows.append((step, device, result, observed))

    def typed(self, function: dict, args: dict) -> dict:
        out = {}
        for name, value in args.items():
            kind = function["args"][name]["type"]
            if kind == "bytes":
                out[name] = {"type": "bytes", "base64": base64.b64encode(value).decode()}
            else:
                out[name] = {"type": kind, kind: value}
        return out

    def call(self, device: str, name: str, args: dict) -> str:
        functions = self.caps[device]["capabilities"]["functions"]
        function = next(f for f in functions if f["name"] == name)
        reply = self.http(
            "POST",
            "/v0/call",
            {
                "provider_id": self.provider,
                "device_id": device,
                "function_id": function["function_id"],
                "args": self.typed(function, args),
            },
        )
        status = reply.get("status", {"code": "?", "message": reply.get("error", "")})
        return status.get("code", "?") + ("" if status.get("code") == "OK" else f": {status.get('message', '')}")

    def read(self, device: str) -> tuple[bool, str]:
        """Whether the runtime has readings for the device, and them.

        The runtime answers from its cache with status OK either way: a device
        it cannot reach has quality UNAVAILABLE and no values."""
        state = self.http("GET", f"/v0/state/{self.provider}/{device}")
        quality = state.get("quality", state.get("status", {}).get("code", "?"))
        values = []
        for v in state.get("values", []):
            value = v["value"]
            shown = value.get(value["type"], value)
            values.append(f"{v['signal_id']}={shown} ({v['quality']})")
        present = quality == "OK" and bool(values)
        return present, f"{quality}: {', '.join(values) or 'no values'}"

    def run(self, unplug: list[str]) -> None:
        status = self.http("GET", "/v0/runtime/status")
        startup = status.get("startup_safe_state", {})
        print(f"runtime {status.get('version')}, startup safe state achieved: {startup.get('safe_state_achieved')}")
        self.record("startup safe state", "all", str(startup.get("safe_state_achieved")), self.ask("Every output off at startup?"))

        devices = [d for d in self.http("GET", "/v0/devices")["devices"] if d["provider_id"] == self.provider]
        by_type = {d["type"]: d["device_id"] for d in devices}
        for d in devices:
            device, kind = d["device_id"], d["type"]
            self.caps[device] = self.http("GET", f"/v0/devices/{self.provider}/{device}/capabilities")
            print(f"\n== {device} ({kind})")
            if self.caps[device]["capabilities"]["signals"]:
                _, values = self.read(device)
                print(f"  read: {values}")
                self.record("read", device, values, self.ask("Plausible readings?"))
            if kind in INPUTS:
                self.ask(f"Now {INPUTS[kind]}, then press Enter.")
                _, values = self.read(device)
                print(f"  read: {values}")
                self.record("read with input", device, values, self.ask("Did the readings follow?"))
            for name, args, expect in CALLS.get(kind, []):
                if name == PAUSE:
                    self.ask(f"{expect}, then press Enter.")
                    continue
                result = self.call(device, name, args)
                print(f"  {name}: {result}. Look for: {expect}")
                self.record(f"call {name}", device, result, self.ask("Seen?"))
            if kind in AFTER:
                self.ask(f"Now {AFTER[kind]}, then press Enter.")
                _, values = self.read(device)
                print(f"  read: {values}")
                self.record("read after calls", device, values, self.ask("As expected?"))

        print("\n== software stop")
        for kind, name, args in STOP_SETUP:
            if kind in by_type:
                print(f"  {by_type[kind]}.{name}: {self.call(by_type[kind], name, args)}")
        outputs = ", ".join(by_type[kind] for kind in dict.fromkeys(k for k, _, _ in STOP_SETUP) if kind in by_type)
        self.ask(f"Outputs on as set up ({outputs})? Enter to stop them.")
        stop = self.http("POST", "/v0/estop")
        failed = [f"{a['device_handle']}.{a['function']}" for a in stop.get("actions", []) if not a.get("success")]
        result = f"achieved={stop.get('safe_state_achieved')}, failed={failed or 'none'}"
        print(f"  estop: {result}")
        self.record("software stop", "all", result, self.ask("Every output off?"))
        clear = self.http("POST", "/v0/estop/clear")
        print(f"  clear: latched={clear.get('latched')}")

        for device in unplug if self.prompt else []:
            if device not in self.caps:
                print(f"\n{device} is not listed; skipped")
                continue
            print(f"\n== {device} off the bus and back")
            self.ask(f"Unplug {device}, keep it out until asked, then press Enter.")
            self.record("unplugged", device, self.wait(device, present=False), self.ask("Anything else on the bus affected?"))
            # Watch from before the replug, so the first reading seen is the
            # first the runtime took, not one from a later poll.
            print(f"  Plug {device} back in now; watching for its first reading.")
            first = self.wait(device, present=True)
            kind = next(d["type"] for d in devices if d["device_id"] == device)
            if "(already back" in first or "(still so after" in first:
                question = "Anything to note?"
            elif kind in SENSORS:
                question = "First readings a real sample, not zeros?"
            else:
                question = "Readings back, and right?"
            self.record("plugged back", device, first, self.ask(question))

        if self.prompt:
            print("\n== the provider's own stop")
            for kind, name, args in PROVIDER_STOP_SETUP:
                if kind not in by_type:
                    continue
                if name == PAUSE:
                    self.ask(f"{args['text']}, then press Enter.")
                    continue
                print(f"  {by_type[kind]}.{name}: {self.call(by_type[kind], name, args)}")
            holding = " The stepper is holding: after the stop it should turn freely by hand." if "motors" in by_type else ""
            self.ask(f"Outputs on again ({outputs}).{holding} Stop the runtime with Ctrl-C in its terminal, then press Enter.")
            self.stopped = True
            self.record("runtime stopped", "all", "the provider's stop at session end", self.ask("Every output off?"))

    def wait(self, device: str, present: bool) -> str:
        """Poll every 100 ms, faster than the runtime polls, until the
        device's readings are gone, or back. Started before the board is
        plugged back in, the first readings seen are the first the runtime
        took on reconnecting."""
        deadline = time.monotonic() + 60
        first_poll = True
        while True:
            now_present, values = self.read(device)
            if present and now_present and first_poll:
                result = values + " (already back when the watch began: not the first reading)"
                print(f"  read: {result}")
                return result
            first_poll = False
            if now_present == present or time.monotonic() > deadline:
                result = values + ("" if now_present == present else " (still so after 60 s)")
                print(f"  read: {result}")
                return result
            time.sleep(0.1)

    def abort(self) -> None:
        """Outputs off after an early stop: the software stop, then its latch
        released, so a later run does not start latched."""
        if not self.contacted:
            return
        print("\nstopped early: running the software stop")
        try:
            stop = self.http("POST", "/v0/estop")
        except SystemExit as e:
            print(f"  {e}. Outputs may still be on: stop the runtime, or run")
            print(f"  curl -s -X POST {self.url}/v0/estop")
            print("  and before the next run, release its latch:")
            print(f"  curl -s -X POST {self.url}/v0/estop/clear")
            self.record("stopped early", "all", "software stop not run: runtime unreachable", "")
            return
        result = f"software stop achieved={stop.get('safe_state_achieved')}"
        try:
            self.http("POST", "/v0/estop/clear")
        except SystemExit as e:
            print(f"  {e}. The latch is still set: before the next run,")
            print(f"  curl -s -X POST {self.url}/v0/estop/clear")
            result += ", latch not released"
        print(f"  estop: {result}")
        self.record("stopped early", "all", result, "")

    def report(self) -> str:
        lines = [
            f"# Bench run: {datetime.date.today().isoformat()}",
            "",
            f"- Host: {host()}",
            f"- Kernel: {platform.release()}",
            f"- I2C adapter driver: {adapter()}",
            "",
            "| Step | Device | Result | Observed |",
            "| --- | --- | --- | --- |",
        ]
        for row in self.rows:
            lines.append("| " + " | ".join(cell.replace("|", "\\|") for cell in row) + " |")
        return "\n".join(lines) + "\n"


def host() -> str:
    try:
        return Path("/proc/device-tree/model").read_text().rstrip("\0\n")
    except OSError:
        return platform.machine()


def adapter() -> str:
    # As modulino-driver's bench checklist records it (its setup step 4).
    driver = Path("/sys/bus/i2c/devices/i2c-1/../driver")
    return driver.resolve().name if driver.exists() else "unknown"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--url", default="http://127.0.0.1:8080")
    parser.add_argument("--provider", default="modulino0")
    parser.add_argument(
        "--unplug",
        nargs="*",
        default=["buttons0", "pressure0", "light0", "movement0"],
        help="devices to take off the bus and put back, one at a time",
    )
    parser.add_argument("--out", type=Path, default=REPO / "target" / "bench-results.md")
    parser.add_argument("--no-prompt", action="store_true")
    options = parser.parse_args()
    bench = Bench(options.url, options.provider, prompt=not options.no_prompt)
    try:
        bench.run(options.unplug)
    except BaseException:
        if not bench.stopped:
            bench.abort()
        raise
    finally:
        options.out.parent.mkdir(parents=True, exist_ok=True)
        options.out.write_text(bench.report())
        print(f"\nresults: {options.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
