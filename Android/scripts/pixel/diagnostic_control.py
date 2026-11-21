#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""Pixel-only observation admission and bounded, scoped precleanup readback."""
import hashlib
import json
from pathlib import Path
import re
import shlex
import subprocess
import time

PACKAGE = "org.lighttouch.android"
SERIAL = "emulator-5554"
READBACK_AT = 810
STOP_AT = 840
HARD_AT = 900
OBSERVE_MS = 300_000
READBACK_RESERVE_MS = 20_000
CLOCK_MARGIN_MS = 1_000
MAX_BYTES = 32 * 1024 * 1024
MAX_FILE_BYTES = 4 * 1024 * 1024


def integer(value, minimum=0):
    if type(value) is not int or value < minimum:
        raise ValueError("Invalid integral identity/time")
    return value


def validate_binding(binding):
    for key, minimum in (("workerPid", 2), ("workerStartTicks", 1), ("appUid", 10000), ("originNs", 1)):
        integer(binding[key], minimum)
    for key, pattern in (("recordId", r"[0-9a-f]{32}"), ("attemptToken", r"[0-9a-f]{32}"),
                         ("originToken", r"[0-9a-f]{64}"),
                         ("bootId", r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")):
        if not isinstance(binding[key], str) or not re.fullmatch(pattern, binding[key]):
            raise ValueError("Invalid bound " + key)
    return binding


def make_admission(binding, bracket):
    """Called AFTER Start returned, using a fresh BOOTTIME read bracket.

    BOOTTIME and supervisor MONOTONIC remain distinct. This conservative device
    deadline supplements the independent original host watchdog; completion must
    still be observed in time by that host. It is not a clock-rate equivalence proof.
    """
    validate_binding(binding)
    before = integer(bracket["hostBeforeNs"], binding["originNs"])
    after = integer(bracket["hostAfterNs"], before)
    device = integer(bracket["deviceElapsedMs"])
    quantum = integer(bracket["quantizationMs"], 1)
    if bracket["bootId"] != binding["bootId"]:
        raise ValueError("Device boot changed")
    if after - before > 2_000_000_000 or quantum > 100:
        raise ValueError("Clock bracket too uncertain")
    # Reserve full300s,20s readback and1s clock/delivery margin after actual Start.
    host_observe_limit = binding["originNs"] + READBACK_AT * 1_000_000_000 - READBACK_RESERVE_MS * 1_000_000
    remaining_ms = (host_observe_limit - after) // 1_000_000 - CLOCK_MARGIN_MS - quantum
    if remaining_ms < OBSERVE_MS:
        raise ValueError("Insufficient original budget after Start; no shortened interval")
    return {"schema": 1, **binding, "observationMs": OBSERVE_MS,
            "deviceObservationLimitMs": device + remaining_ms,
            "hostObservationLimitNs": host_observe_limit,
            "hostReadbackLimitNs": binding["originNs"] + READBACK_AT * 1_000_000_000,
            "clockBracket": bracket, "guestAcceptancePassed": False}


def readback_deadline(origin_ns, now_ns):
    integer(origin_ns, 1); integer(now_ns, origin_ns)
    return min(now_ns + 10_000_000_000, origin_ns + READBACK_AT * 1_000_000_000)


def scoped_paths(binding, timeline=None):
    validate_binding(binding)
    folder = "pixel-boot-" + binding["attemptToken"]
    root = "files/" + folder
    pid, record = binding["workerPid"], binding["recordId"]
    paths = [root + "/result.json", root + "/ready.json", root + "/timeline.json",
             "files/logs/engine.log", "files/logs/engine.tail.1.log", "files/logs/engine.tail.log",
             f"files/worker-shutdown-{pid}.json", f"files/worker-stop-error-{pid}.txt"]
    if timeline is not None:
        if not isinstance(timeline, list) or len(timeline) > 40:
            raise ValueError("Invalid bounded diagnostic timeline")
        indexes = []
        for item in timeline:
            index = integer(item["index"])
            if index > 39:
                raise ValueError("Frame index outside bounded observation")
            state = item.get("status", {})
            if state.get("pid") != pid or state.get("uid") != binding["appUid"]:
                raise ValueError("Timeline worker identity differs")
            indexes.append(index)
        # Priority: first and latest captures, then other retained samples.
        order = list(dict.fromkeys(([indexes[0], indexes[-1]] if indexes else []) + indexes))
        for index in order:
            paths.extend((f"files/screenshots/{folder}/frame-{index}.png", root + f"/qmp-{index}.jsonl"))
    return paths


class Capture:
    def __init__(self, adb, binding, output, now=time.monotonic_ns, run=subprocess.run):
        self.adb, self.binding, self.output = adb, validate_binding(binding), Path(output)
        resolved = self.output.resolve()
        if any((p / ".git").exists() for p in (resolved, *resolved.parents)):
            raise ValueError("Private capture output must be outside Git")
        self.now, self.run = now, run
        self.deadline = readback_deadline(binding["originNs"], now())
        self.bytes = 0
        self.commands = []

    def read(self, path, limit=MAX_FILE_BYTES):
        if self.now() >= self.deadline or self.bytes >= MAX_BYTES:
            raise TimeoutError("Original bounded readback deadline/byte cap reached")
        if not re.fullmatch(r"(?:files/[A-Za-z0-9_./-]+|/proc/(?:[0-9]+/(?:stat|status|cmdline)|self/status|sys/kernel/random/boot_id))", path):
            raise ValueError("Unapproved read path")
        if ".." in path.split("/"):
            raise ValueError("Traversal refused")
        cap = min(limit, MAX_BYTES - self.bytes)
        marker = "__LT_PIXEL_REMOTE_EXIT__="
        checks = ""
        if path.startswith("files/"):
            parts = path.split("/")
            for i in range(1, len(parts) + 1):
                component = shlex.quote("/".join(parts[:i]))
                checks += f"if [ -L {component} ]; then printf '\\n{marker}193\\n'; exit 0; fi; "
        script = (checks + f"head -c {cap + 1} {shlex.quote(path)}; lt_pixel_exit=$?; "
                  f"printf '\\n{marker}%s\\n' \"$lt_pixel_exit\"")
        argv = [self.adb, "-s", SERIAL, "exec-out",
                shlex.join(["run-as", PACKAGE, "sh", "-c", script])]
        before = self.now()
        if before >= self.deadline:
            raise TimeoutError("Readback expired before command dispatch")
        timeout = min(1.5, (self.deadline - before) / 1e9)
        try:
            result = self.run(argv, capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            result = subprocess.CompletedProcess(argv, 124, error.stdout or b"", error.stderr or b"")
        match = re.search(rb"\n__LT_PIXEL_REMOTE_EXIT__=(\d+)\n$", result.stdout)
        data = result.stdout[:match.start()] if match else result.stdout
        remote = int(match.group(1)) if match else None
        truncated = len(data) > cap
        data = data[:cap]
        self.bytes += len(data)
        entry = {"argv": argv, "beginNs": before, "endNs": self.now(), "adbExit": result.returncode,
                 "remoteExit": remote, "stderr": result.stderr.decode(errors="replace"),
                 "bytes": len(data), "truncated": truncated, "sha256": hashlib.sha256(data).hexdigest()}
        self.commands.append(entry)
        if self.output.is_dir():
            self.output.joinpath(f"remote-read-{len(self.commands):03}.bin").write_bytes(data)
        return data, entry

    def collect(self):
        self.output.mkdir(parents=True, exist_ok=False)
        receipt = {"scope": self.binding, "deadlineNs": self.deadline, "commands": self.commands,
                   "guestAcceptancePassed": False, "shutdownConfirmed": False, "workerIdentity": "unavailable"}
        try:
            if self.now() >= self.deadline:
                raise TimeoutError("Readback already past original810 gate; no ADB command")
            # Direct app-UID reads; explicit remote sentinels survive adb outer exit0.
            data, meta = self.read("/proc/sys/kernel/random/boot_id", 128)
            if meta["remoteExit"] != 0 or data.decode().strip() != self.binding["bootId"]:
                raise ValueError("Device boot identity unavailable/different")
            status, meta = self.read("/proc/self/status", 16384)
            uid = re.search(rb"^Uid:\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)$", status, re.M)
            if meta["remoteExit"] != 0 or not uid or set(map(int, uid.groups())) != {self.binding["appUid"]}:
                raise ValueError("Current run-as UID differs/unavailable")
            data, meta = self.read("files/library/selected", 128)
            if meta["remoteExit"] != 0 or data.decode().strip() != self.binding["recordId"]:
                raise ValueError("Selected record differs")
            data, meta = self.read(f"/proc/{self.binding['workerPid']}/stat", 4096)
            if meta["remoteExit"] == 0:
                fields = data.decode().rsplit(")", 1)[1].split()
                if int(fields[19]) != self.binding["workerStartTicks"]:
                    raise ValueError("Worker PID reused")
                status, smeta = self.read(f"/proc/{self.binding['workerPid']}/status", 16384)
                uid = re.search(rb"^Uid:\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)$", status, re.M)
                if smeta["remoteExit"] != 0 or not uid or set(map(int, uid.groups())) != {self.binding["appUid"]}:
                    raise ValueError("Worker UID differs/unavailable")
                cmdline, cmeta = self.read(f"/proc/{self.binding['workerPid']}/cmdline", 4096)
                if cmeta["remoteExit"] != 0 or cmdline.rstrip(b"\0") != (PACKAGE + ":engine").encode():
                    raise ValueError("Worker process role differs/unavailable")
                receipt["workerIdentity"] = "same-live-worker"
            elif meta["remoteExit"] == 1 and "No such file" in meta["stderr"]:
                receipt["workerIdentity"] = "bound-worker-absent-historical-files-only"
            else:
                raise ValueError("Worker identity unavailable; not evidence of absence")
            # Bind the diagnostic files themselves to the original source/worker.
            folder = "files/pixel-boot-" + self.binding["attemptToken"]
            data, meta = self.read(folder + "/ready.json", 65536)
            ready = json.loads(data)
            for key in ("appUid", "workerPid", "workerStartTicks", "bootId", "recordId", "attemptToken", "originToken"):
                if ready.get(key) != self.binding[key]:
                    raise ValueError("Diagnostic ready identity differs: " + key)
            if meta["remoteExit"] != 0 or meta["truncated"]:
                raise ValueError("Diagnostic ready metadata incomplete")
            self.output.joinpath("ready.json").write_bytes(data)
            timeline = None
            for path in scoped_paths(self.binding):
                data, meta = self.read(path)
                name = path.rsplit("/", 1)[-1]
                self.output.joinpath(name).write_bytes(data)
                if name == "timeline.json" and meta["remoteExit"] == 0 and not meta["truncated"]:
                    timeline = json.loads(data)
            for path in scoped_paths(self.binding, timeline)[6:]:
                data, meta = self.read(path)
                self.output.joinpath(path.rsplit("/", 1)[-1]).write_bytes(data)
        except Exception as error:
            receipt["failure"] = str(error)
            receipt["failureType"] = type(error).__name__
        finally:
            receipt.update(endedNs=self.now(), totalBytes=self.bytes)
            self.output.joinpath("receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        return receipt


def capture_then_dispose(capture, stop, cleanup):
    """Capture failure cannot suppress Stop or owned cleanup."""
    capture_error = None
    try:
        try:
            capture()
        except Exception as error:
            capture_error = str(error)
        stop()
    finally:
        cleanup()
    return {"captureError": capture_error, "guestAcceptancePassed": False}


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adb", required=True)
    parser.add_argument("--attempt", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    origin_raw = (args.attempt / "clock-context.json").read_bytes()
    context = json.loads(origin_raw)
    bound = json.loads((args.attempt / "pixel-bound.json").read_text())
    if bound["originToken"] != hashlib.sha256(origin_raw).hexdigest() or bound["originNs"] != context["originNs"]:
        raise ValueError("Bound original context differs")
    if context["originNs"] != context["supervisorStartTicks"] * 1_000_000_000 // context["clockTicksPerSecond"]:
        raise ValueError("Original process clock differs")
    print(json.dumps(Capture(args.adb, bound, args.output).collect(), indent=2))
