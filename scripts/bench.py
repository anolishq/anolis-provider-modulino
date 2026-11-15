#!/usr/bin/env python3
"""Drive the bench run (docs/bench.md) through the Anolis runtime's HTTP API.

    scripts/bench.py [--url URL] [--provider ID] [--out FILE] [--no-prompt]

For each board the runtime lists under the provider: read every signal, then
call each function with the arguments in CALLS. After each step it asks what
you saw, and writes every answer to a Markdown results file. Then it runs
the runtime's software stop with the outputs on, and takes each --unplug
board off the bus and puts it back. --no-prompt runs the calls and the stop,
skips the unplugging, and records nothing observed: a check of the script
and the configs against mock mode.

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
    "knob": [("set_position", {"position": 0}, "the next position read is 0")],
    "joystick": [("set_deadzone", {"deadzone": 26}, "the centre reads 0 on both axes")],
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

# Outputs left on for the software stop: each has a hook in
# config/bench-runtime.yaml.
STOP_SETUP: list[tuple[str, str, dict]] = [
    ("buttons", "set_leds", {"a": True, "b": True, "c": True}),
    ("pixels", "set_all", {"red": 0, "green": 255, "blue": 0, "brightness": 20}),
    ("opto_relay", "set", {"on": True}),
    ("latch_relay", "set", {"on": True}),
    ("led_matrix", "show_mono", {"pixels": bytes([0xFF] * 12)}),
    ("buzzer", "tone", {"frequency_hz": 440, "duration_ms": 0}),
    ("motors", "set_mode", {"mode": "dc"}),
    ("motors", "set_speeds", {"a": 8000, "b": 8000}),
]


class Bench:
    def __init__(self, url: str, provider: str, prompt: bool) -> None:
        self.url = url.rstrip("/")
        self.provider = provider
        self.prompt = prompt
        self.rows: list[tuple[str, str, str, str]] = []
        self.caps: dict[str, dict] = {}

    def http(self, method: str, path: str, body: dict | None = None) -> dict:
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.url + path, data=data, method=method)
        if data is not None:
            request.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.load(response)
        except urllib.error.HTTPError as e:
            return json.load(e)

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
        status = reply.get("status", {})
        return status.get("code", "?") + ("" if status.get("code") == "OK" else f": {status.get('message', '')}")

    def read(self, device: str) -> tuple[str, str]:
        state = self.http("GET", f"/v0/state/{self.provider}/{device}")
        values = []
        for v in state.get("values", []):
            value = v["value"]
            shown = value.get(value["type"], value)
            values.append(f"{v['signal_id']}={shown} ({v['quality']})")
        return state.get("status", {}).get("code", "?"), ", ".join(values)

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
                code, values = self.read(device)
                print(f"  read: {code}: {values}")
                self.record("read", device, f"{code}: {values}", self.ask("Plausible readings?"))
            if kind in INPUTS:
                self.ask(f"Now {INPUTS[kind]}, then press Enter.")
                code, values = self.read(device)
                print(f"  read: {code}: {values}")
                self.record("read with input", device, f"{code}: {values}", self.ask("Did the readings follow?"))
            for name, args, expect in CALLS.get(kind, []):
                if name == PAUSE:
                    self.ask(f"{expect}, then press Enter.")
                    continue
                result = self.call(device, name, args)
                print(f"  {name}: {result}. Look for: {expect}")
                self.record(f"call {name}", device, result, self.ask("Seen?"))

        print("\n== software stop")
        for kind, name, args in STOP_SETUP:
            if kind in by_type:
                print(f"  {by_type[kind]}.{name}: {self.call(by_type[kind], name, args)}")
        self.ask("Outputs on as set up (LEDs, relays, matrix, tone, motors)? Enter to stop them.")
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
            self.ask(f"Unplug {device}, then press Enter.")
            self.record("unplugged", device, self.wait(device, want_ok=False), self.ask("Anything else on the bus affected?"))
            self.ask(f"Plug {device} back in, then press Enter.")
            first = self.wait(device, want_ok=True)
            self.record("plugged back", device, first, self.ask("First readings plausible, not zeros?"))

    def wait(self, device: str, want_ok: bool) -> str:
        deadline = time.monotonic() + 15
        while True:
            code, values = self.read(device)
            reached = (code == "OK") == want_ok
            if reached or time.monotonic() > deadline:
                result = f"{code}: {values}" + ("" if reached else " (still so after 15 s)")
                print(f"  read: {result}")
                return result
            time.sleep(0.5)

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
    try:
        return Path("/sys/bus/i2c/devices/i2c-1/../driver").resolve().name
    except OSError:
        return "unknown"


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
    finally:
        options.out.parent.mkdir(parents=True, exist_ok=True)
        options.out.write_text(bench.report())
        print(f"\nresults: {options.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
