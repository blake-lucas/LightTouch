# SPDX-License-Identifier: GPL-2.0-or-later
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SOURCE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SOURCE))
import diagnostic_control as control
import pixel_supervisor
import admit_observation
import capture_origin


def binding():
    return {"workerPid": 42, "workerStartTicks": 1234, "appUid": 10215,
            "originNs": 1_000_000_000_000, "recordId": "a" * 32, "attemptToken": "b" * 32,
            "originToken": "c" * 64, "bootId": "11111111-2222-3333-4444-555555555555"}


def stat(ticks):
    return ("42 (org.lighttouch.android:engine) " + " ".join(["S"] + ["0"] * 18 + [str(ticks)] + ["0"] * 5)).encode()


def bracket(offset=400):
    b = binding()
    return {"hostBeforeNs": b["originNs"] + offset * 1_000_000_000,
            "hostAfterNs": b["originNs"] + offset * 1_000_000_000 + 10_000_000,
            "deviceElapsedMs": 900000, "quantizationMs": 10, "bootId": b["bootId"]}


class Control(unittest.TestCase):
    def test_after_start_admission_keeps_full_interval_and_separate_epochs(self):
        gate = control.make_admission(binding(), bracket())
        self.assertEqual(gate["observationMs"], 300000)
        self.assertGreaterEqual(gate["deviceObservationLimitMs"] - 900000, 300000)
        self.assertEqual(gate["hostReadbackLimitNs"], binding()["originNs"] + 810_000_000_000)
        self.assertFalse(gate["guestAcceptancePassed"])

    def test_pre_start488_guard_does_not_admit_actual_after_start500(self):
        with self.assertRaisesRegex(ValueError, "after Start"):
            control.make_admission(binding(), bracket(500))

    def test_clock_reboot_wide_bracket_and_before_origin_refused(self):
        for change in ({"bootId": "changed"}, {"hostAfterNs": bracket()["hostBeforeNs"] + 3_000_000_000},
                       {"hostBeforeNs": binding()["originNs"] - 1}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                control.make_admission(binding(), {**bracket(), **change})

    def test_invalid_scope_cannot_form_path_or_do_adb(self):
        for field, value in (("recordId", "../other"), ("attemptToken", "x;cat"),
                             ("workerPid", True), ("appUid", 0)):
            with self.subTest(field=field), self.assertRaises(ValueError):
                control.scoped_paths({**binding(), field: value})

    def test_timeline_reuse_and_large_index_refused(self):
        for item in ({"index": 40, "status": {"pid": 42, "uid": 10215}},
                     {"index": 0, "status": {"pid": 43, "uid": 10215}},
                     {"index": 0, "status": {"pid": 42, "uid": 10216}}):
            with self.subTest(item=item), self.assertRaises(ValueError):
                control.scoped_paths(binding(), [item])

    def test_only_derived_record_worker_and_sample_paths(self):
        paths = control.scoped_paths(binding(), [{"index": 0, "status": {"pid": 42, "uid": 10215}}])
        self.assertIn("files/logs/engine.log", paths)
        self.assertIn("files/screenshots/pixel-boot-" + "b" * 32 + "/frame-0.png", paths)
        self.assertFalse(any("base/" in p or "itbase" in p for p in paths))

    def test_deadline_never_extends_original810_or10_seconds(self):
        origin = binding()["originNs"]
        self.assertEqual(control.readback_deadline(origin, origin + 100_000_000_000), origin + 110_000_000_000)
        self.assertEqual(control.readback_deadline(origin, origin + 807_000_000_000), origin + 810_000_000_000)

    def test_expired_gate_makes_no_adb_call(self):
        with tempfile.TemporaryDirectory() as d:
            run = mock.Mock()
            now = lambda: binding()["originNs"] + 811_000_000_000
            receipt = control.Capture("/synthetic/adb", binding(), Path(d) / "out", now=now, run=run).collect()
            run.assert_not_called()
            self.assertEqual(receipt["failureType"], "TimeoutError")
            self.assertFalse(receipt["shutdownConfirmed"])

    def test_private_output_refuses_git_directory_and_worktree_file(self):
        for is_file in (False, True):
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                (root / ".git").write_text("gitdir: /synthetic") if is_file else (root / ".git").mkdir()
                run = mock.Mock()
                with self.assertRaisesRegex(ValueError, "outside Git"):
                    control.Capture("/synthetic/adb", binding(), root / "private", run=run)
                run.assert_not_called()

    def test_capture_fail_and_stop_fail_still_cleanup_in_order(self):
        calls = []
        def capture(): calls.append("capture"); raise OSError("capture failed")
        def stop(): calls.append("stop"); raise RuntimeError("stop failed")
        with self.assertRaisesRegex(RuntimeError, "stop failed"):
            control.capture_then_dispose(capture, stop, lambda: calls.append("cleanup"))
        self.assertEqual(calls, ["capture", "stop", "cleanup"])

    def test_adapter_capture_exception_cannot_suppress_stop_or_cleanup(self):
        with tempfile.TemporaryDirectory() as d:
            args = type("Args", (), {"output": str(Path(d) / "attempt"), "adb": "/synthetic/adb"})()
            with mock.patch.object(pixel_supervisor.capture_origin, 'capture'), mock.patch.object(pixel_supervisor.host_lifecycle, 'install'):
                args.avd = 'synthetic-pixel'
                attempt = pixel_supervisor.PixelAttempt(args)
            calls = []
            attempt.pixel_readback = mock.Mock(side_effect=OSError("unavailable"))
            with mock.patch.object(pixel_supervisor.base.Attempt, "stop", lambda s: calls.append("stop")):
                with self.assertRaises(OSError): attempt.stop()
            with mock.patch.object(pixel_supervisor.base.Attempt, "cleanup", lambda s: calls.append("cleanup")):
                with self.assertRaises(OSError): attempt.cleanup()
            self.assertEqual(calls, ["stop", "cleanup"])


class CaptureIdentity(unittest.TestCase):
    def scenario(self, change=None):
        b = binding(); folder = "files/pixel-boot-" + b["attemptToken"]
        uid = b"Uid:\t10215\t10215\t10215\t10215\n"
        data = {"/proc/sys/kernel/random/boot_id": b["bootId"].encode(), "/proc/self/status": uid,
                "files/library/selected": b["recordId"].encode(), "/proc/42/stat": stat(1234),
                "/proc/42/status": uid, "/proc/42/cmdline": b"org.lighttouch.android:engine\0",
                folder + "/ready.json": json.dumps(b).encode(), folder + "/result.json": b"{}",
                folder + "/timeline.json": b'[{"index":0,"status":{"pid":42,"uid":10215}}]',
                folder + "/qmp-0.jsonl": b"synthetic-host-test-only",
                "files/screenshots/pixel-boot-" + b["attemptToken"] + "/frame-0.png": b"synthetic-host-test-only"}
        if change: data.update(change)
        calls = []
        def run(argv, **kwargs):
            calls.append(argv)
            self.assertLessEqual(kwargs["timeout"], 1.5)
            self.assertEqual(argv[:4], ["/synthetic/adb", "-s", "emulator-5554", "exec-out"])
            remote = shlex.split(argv[-1]); self.assertEqual(remote[:4], ["run-as", control.PACKAGE, "sh", "-c"])
            match = re.search(r"head -c (\d+) ([A-Za-z0-9_./-]+);", remote[-1]); self.assertIsNotNone(match)
            cap, path = int(match.group(1)), match.group(2)
            if path in data and isinstance(data[path], tuple):
                payload, code, stderr = data[path]
            elif path in data: payload, code, stderr = data[path][:cap], 0, b""
            else: payload, code, stderr = b"", 1, b"head: No such file or directory\n"
            return subprocess.CompletedProcess(argv, 0, payload + f"\n__LT_PIXEL_REMOTE_EXIT__={code}\n".encode(), stderr)
        return b, data, calls, run

    def collect(self, change=None):
        b, data, calls, run = self.scenario(change)
        with tempfile.TemporaryDirectory() as d:
            receipt = control.Capture("/synthetic/adb", b, Path(d) / "out",
                now=lambda: b["originNs"] + 500_000_000_000, run=run).collect()
            return receipt, calls

    def test_live_worker_scope_and_missing_optional_files_preserved(self):
        receipt, calls = self.collect()
        self.assertEqual(receipt["workerIdentity"], "same-live-worker")
        self.assertNotIn("failure", receipt)
        self.assertTrue(any(x["remoteExit"] == 1 and x["adbExit"] == 0 for x in receipt["commands"]))
        self.assertFalse(receipt["guestAcceptancePassed"])

    def test_reused_pid_wrong_uid_or_role_stops_before_raw_copy(self):
        for change in ({"/proc/42/stat": stat(9999)},
                       {"/proc/42/status": b"Uid:\t10216\t10216\t10216\t10216\n"},
                       {"/proc/42/cmdline": b"org.lighttouch.android:firmware\0"}):
            receipt, calls = self.collect(change)
            self.assertIn("failure", receipt)
            self.assertFalse(any("frame-0.png" in " ".join(c) for c in calls))

    def test_permission_denied_is_not_worker_absence(self):
        receipt, calls = self.collect({"/proc/42/stat": (b"", 1, b"Permission denied\n")})
        self.assertEqual(receipt["workerIdentity"], "unavailable")
        self.assertIn("failure", receipt)

    def test_verified_absence_collects_historical_files_not_live_result(self):
        receipt, calls = self.collect({"/proc/42/stat": (b"", 1, b"No such file or directory\n")})
        self.assertEqual(receipt["workerIdentity"], "bound-worker-absent-historical-files-only")
        self.assertNotIn("failure", receipt)
        self.assertFalse(receipt["guestAcceptancePassed"])

    def test_wrong_run_as_uid_and_boot_refuse_scoped_copies(self):
        for change in ({"/proc/self/status": b"Uid:\t1\t1\t1\t1\n"},
                       {"/proc/sys/kernel/random/boot_id": b"different"}):
            receipt, calls = self.collect(change)
            self.assertIn("failure", receipt)
            self.assertFalse(any("logs/engine.log" in " ".join(c) for c in calls))

    def test_symlink_remote193_is_not_treated_as_success(self):
        path = "files/pixel-boot-" + binding()["attemptToken"] + "/ready.json"
        receipt, calls = self.collect({path: (b"", 193, b"")})
        self.assertIn("failure", receipt)
        self.assertTrue(any(x["remoteExit"] == 193 for x in receipt["commands"]))

    def test_timed_out_partial_remote_bytes_are_retained(self):
        b, data, calls, run = self.scenario()
        with tempfile.TemporaryDirectory() as d:
            out = Path(d) / "out"; out.mkdir()
            fail = mock.Mock(side_effect=subprocess.TimeoutExpired("synthetic", 1, output=b"partial"))
            capture = control.Capture("/synthetic/adb", b, out,
                now=lambda: b["originNs"] + 500_000_000_000, run=fail)
            payload, meta = capture.read("files/library/selected")
            self.assertEqual(payload, b"partial")
            self.assertEqual(meta["adbExit"], 124)
            self.assertIsNone(meta["remoteExit"])
            self.assertEqual((out / "remote-read-001.bin").read_bytes(), b"partial")


class Admission(unittest.TestCase):
    def test_public_clock_reads_actual_host_proc_protocol_without_app_uid_context(self):
        # Actual Linux proc read exercises the protocol; this is not Android evidence.
        import time
        with tempfile.TemporaryDirectory() as d:
            b=binding();b['bootId']=Path('/proc/sys/kernel/random/boot_id').read_text().strip()
            calls=[]
            def run(argv,**kw):
                calls.append(argv);self.assertNotIn('run-as',argv)
                self.assertEqual(argv[3:5],['exec-out','cat'])
                return subprocess.run(argv[4:],**kw)
            output=Path(d)/'clock.json'
            bracket=admit_observation.public_device_clock('/synthetic/adb',b,output,time.monotonic_ns()+3*10**9,run=run)
            self.assertEqual(bracket['bootId'],b['bootId']);self.assertEqual(bracket['quantizationMs'],10)
            receipt=json.loads(output.read_text());self.assertEqual(receipt['exit'],0)
            self.assertEqual(bytes.fromhex(receipt['stdoutHex']).decode(),receipt['stdout'])
            self.assertLessEqual(bracket['hostBeforeNs'],bracket['hostAfterNs'])

    def test_public_clock_preserves_denied_merged_malformed_changed_boot_and_timeout_before_rejection(self):
        b=binding();boot=b['bootId'].encode();good=boot+b'\n215.58 331.98\n'+boot+b'\n'
        cases=[(0,b'cat: /proc/uptime: Permission denied\n',b''),
            (0,good,b'Permission denied'),(1,good,b''),(0,b'other\n215.58 331.98\n'+boot+b'\n',b''),
            (0,boot+b'\n215.58 331.98\nother\n',b''),(0,boot+b'\n215.5800 331.98\n'+boot+b'\n',b''),
            (0,good+b'extra\n',b''),(0,b'\xff',b''),'timeout']
        for case in cases:
            with self.subTest(case=case),tempfile.TemporaryDirectory() as d:
                out=Path(d)/'clock.json'
                def run(argv,**kw):
                    if case=='timeout':raise subprocess.TimeoutExpired(argv,1,output=b'partial clock',stderr=b'partial error')
                    return subprocess.CompletedProcess(argv,*case)
                with self.assertRaises(ValueError):admit_observation.public_device_clock('/synthetic/adb',b,out,10**9,now=lambda:1,run=run)
                receipt=json.loads(out.read_text());self.assertFalse(receipt['guestAcceptancePassed'])
                self.assertEqual(bytes.fromhex(receipt['stdoutHex']),b'partial clock' if case=='timeout' else case[1])
                if case=='timeout':self.assertEqual(receipt['exit'],124)

    def test_public_clock_expired_never_dispatches_and_late_positive_is_rejected(self):
        b=binding();boot=b['bootId'].encode();run=mock.Mock()
        with tempfile.TemporaryDirectory() as d:
            out=Path(d)/'clock.json'
            with self.assertRaises(TimeoutError):admit_observation.public_device_clock('/synthetic/adb',b,out,1,now=lambda:1,run=run)
            run.assert_not_called();self.assertFalse(out.exists())
            run.return_value=subprocess.CompletedProcess([],0,boot+b'\n215.58 331.98\n'+boot+b'\n',b'')
            with self.assertRaises(ValueError):admit_observation.public_device_clock('/synthetic/adb',b,out,10,now=mock.Mock(side_effect=[1,11]),run=run)
            self.assertTrue(out.exists())

    def execute(self, directory, offset=400, change=None, publication=0, ready_change=None):
        b = binding(); out = Path(directory)
        origin = {"originNs": b["originNs"], "supervisorStartTicks": 100000, "clockTicksPerSecond": 100}
        raw = json.dumps(origin).encode(); (out / "clock-context.json").write_bytes(raw)
        b["originToken"] = hashlib.sha256(raw).hexdigest()
        b.update(graphicsProviderRequested='platform',graphicsProviderObserved='platform',graphicsProviderFixed=True)
        (out/'normal-record-bound.json').write_text(json.dumps({**b,'schema':1,'priorDirty':False,'coldRecoveryUsed':False,'pinsVerified':True}))
        ready = dict(b)
        ready.update(priorDirty=False,coldRecoveryUsed=False)
        if ready_change:ready.update(ready_change)
        responses = {"ready": json.dumps(ready).encode(), "id": b"10215\n",
                     "/proc/sys/kernel/random/boot_id": b["bootId"].encode(),
                     "files/library/selected": b["recordId"].encode(), "/proc/42/stat": stat(1234),
                     "/proc/42/status": b"Uid:\t10215\t10215\t10215\t10215\n",
                     "/proc/42/cmdline": b"org.lighttouch.android:engine\0", "/proc/uptime": b"900.00 1200.00\n"}
        if change: responses.update(change)
        calls = []
        clock = b["originNs"] + offset * 1_000_000_000
        def now():
            nonlocal clock
            clock += 10_000_000
            return clock
        def run(argv, **kwargs):
            calls.append(argv)
            if argv[3] == "shell":
                return subprocess.CompletedProcess(argv, 0, f"\n__LT_GATE_EXIT__={publication}\n".encode(), b"")
            if argv[3:]==['exec-out','cat','/proc/sys/kernel/random/boot_id','/proc/uptime','/proc/sys/kernel/random/boot_id']:
                return subprocess.CompletedProcess(argv,0,responses['/proc/sys/kernel/random/boot_id']+b'\n'+responses['/proc/uptime']+responses['/proc/sys/kernel/random/boot_id']+b'\n',b'')
            tail = argv[-1]
            key = "ready" if tail.endswith("/ready.json") else "id" if tail == "-u" else tail
            return subprocess.CompletedProcess(argv, 0, responses[key], b"")
        result = admit_observation.admit("/synthetic/adb", out, b["attemptToken"], now=now, run=run)
        return result, calls

    def test_cli_binds_origin_ready_and_actual_clock_without_reset(self):
        with tempfile.TemporaryDirectory() as d:
            result, calls = self.execute(d)
            self.assertEqual(result["gate"]["observationMs"], 300000)
            self.assertTrue(result["originalClockUnchanged"])
            self.assertFalse(result["guestAcceptancePassed"])
            self.assertTrue((Path(d) / "pixel-bound.json").exists())
            self.assertEqual(sum(c[3] == "shell" for c in calls), 1)
            self.assertEqual(result['gate']['graphicsProviderRequested'],'platform')
            self.assertEqual(result['gate']['graphicsProviderObserved'],'platform')
            self.assertIs(result['gate']['graphicsProviderFixed'],True)

    def test_cli_late_after_start_refuses_gate_but_keeps_capture_binding(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaisesRegex(ValueError, "after Start"):
                self.execute(d, offset=500)
            self.assertTrue((Path(d) / "pixel-bound.json").exists())
            self.assertFalse((Path(d) / "pixel-admission.json").exists())

    def test_provider_mismatch_or_unfixed_selection_never_publishes_admission(self):
        for change in ({'graphicsProviderRequested':'angle-gl'}, {'graphicsProviderObserved':'angle-gl'},
                {'graphicsProviderObserved':None}, {'graphicsProviderFixed':False}):
            with self.subTest(change=change),tempfile.TemporaryDirectory() as d:
                with self.assertRaisesRegex(ValueError,'graphics provider'):self.execute(d,ready_change=change)
                self.assertFalse((Path(d)/'pixel-admission.json').exists())
                self.assertFalse((Path(d)/'pixel-bound.json').exists())

    def test_cli_boot_or_worker_replacement_refuses_publication(self):
        for change in ({"/proc/sys/kernel/random/boot_id": b"other"},
                       {"/proc/42/stat": stat(9999)}, {"/proc/42/cmdline": b"other\0"}):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as d:
                with self.assertRaises(ValueError): self.execute(d, change=change)
                self.assertFalse((Path(d) / "pixel-admission.json").exists())

    def test_outer_adb_zero_does_not_mask_failed_remote_gate_write(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaisesRegex(ValueError, "publication failed"):
                self.execute(d, publication=1)
            self.assertFalse((Path(d) / "pixel-admission.json").exists())


class BoundedChild(unittest.TestCase):
    def test_start_and_identity_interrupt_retire_only_actual_owned_child(self):
        for phase in ('start','identity'):
            with self.subTest(phase=phase):
                child=mock.Mock(pid=543210,exitcode=-9)
                child.is_alive.return_value=True
                if phase=='start':child.start.side_effect=KeyboardInterrupt('start interrupted after spawn')
                identities=[KeyboardInterrupt('identity interrupted'),(1,123)] if phase=='identity' else [(1,123)]
                before=pixel_supervisor.signal.pthread_sigmask(pixel_supervisor.signal.SIG_BLOCK,set())
                with mock.patch.object(pixel_supervisor.base,'process_identity',side_effect=identities), \
                     mock.patch.object(pixel_supervisor.os,'getpgid',return_value=999), \
                     mock.patch.object(pixel_supervisor.os,'kill') as kill, \
                     mock.patch.object(pixel_supervisor.os,'killpg') as group:
                    with self.assertRaises(KeyboardInterrupt):
                        pixel_supervisor.bounded_capture('/synthetic/adb',binding(),'/synthetic/out',
                            now=lambda:binding()['originNs']+809_000_000_000,process_factory=lambda **kw:child)
                kill.assert_called_once_with(543210,pixel_supervisor.signal.SIGKILL)
                group.assert_not_called()
                child.join.assert_called_once()
                self.assertLessEqual(child.join.call_args.kwargs['timeout'],1)
                self.assertEqual(pixel_supervisor.signal.pthread_sigmask(pixel_supervisor.signal.SIG_BLOCK,set()),before)

    def test_start_refusal_without_owned_pid_never_signals_or_joins(self):
        child=mock.Mock(pid=None)
        child.start.side_effect=RuntimeError('No child created')
        with mock.patch.object(pixel_supervisor.os,'kill') as kill, mock.patch.object(pixel_supervisor.os,'killpg') as group:
            with self.assertRaisesRegex(RuntimeError,'No child created'):
                pixel_supervisor.bounded_capture('/synthetic/adb',binding(),'/synthetic/out',
                    now=lambda:binding()['originNs']+809_000_000_000,process_factory=lambda **kw:child)
        kill.assert_not_called();group.assert_not_called();child.join.assert_not_called();child.is_alive.assert_not_called()

    def test_interrupted_join_still_kills_original_owned_child_and_reaps_then_propagates(self):
        child=mock.Mock(pid=543210,exitcode=-9)
        child.is_alive.return_value=True
        child.join.side_effect=[KeyboardInterrupt('actual join interruption'),None]
        with mock.patch.object(pixel_supervisor.base,'process_identity',return_value=(1,123)), \
             mock.patch.object(pixel_supervisor.os,'getpgid',return_value=543210), \
             mock.patch.object(pixel_supervisor.os,'killpg') as kill:
            with self.assertRaisesRegex(KeyboardInterrupt,'actual join interruption'):
                pixel_supervisor.bounded_capture('/synthetic/adb',binding(),'/synthetic/out',
                    now=lambda:binding()['originNs']+809_000_000_000,process_factory=lambda **kw:child)
        kill.assert_called_once_with(543210,pixel_supervisor.signal.SIGKILL)
        self.assertEqual(child.join.call_count,2)
        self.assertLessEqual(child.join.call_args.kwargs['timeout'],1)

    def test_expired_original810_never_starts_capture_child(self):
        factory = mock.Mock()
        outcome = pixel_supervisor.bounded_capture("/synthetic/adb", binding(), "/synthetic/out",
            now=lambda: binding()["originNs"] + 811_000_000_000, process_factory=factory)
        factory.assert_not_called()
        self.assertFalse(outcome["captureStarted"])

    def test_stalled_owned_child_disposed_with_reserved_original_gate(self):
        child = mock.Mock(pid=543210, exitcode=-9)
        child.is_alive.side_effect = [True, False]
        clock = [binding()["originNs"] + 805_000_000_000]
        child.join.side_effect = lambda timeout: clock.__setitem__(0, clock[0] + int(timeout * 1e9))
        with mock.patch.object(pixel_supervisor.base, "process_identity", return_value=(1, 123)), \
             mock.patch.object(pixel_supervisor.os, "getpgid", return_value=543210), \
             mock.patch.object(pixel_supervisor.os, "killpg") as kill:
            result = pixel_supervisor.bounded_capture("/synthetic/adb", binding(), "/synthetic/out",
                now=lambda: clock[0], process_factory=lambda **kwargs: child)
        kill.assert_called_once_with(543210, pixel_supervisor.signal.SIGKILL)
        self.assertEqual(result["limitNs"], binding()["originNs"] + 810_000_000_000)
        self.assertLessEqual(child.join.call_args_list[0].kwargs["timeout"], 4.8)
        self.assertLessEqual(child.join.call_args_list[1].kwargs["timeout"], .2)
        self.assertLessEqual(result["endedNs"], result["limitNs"])
        self.assertFalse(result["captureChildStillAlive"])

    def test_reused_pid_never_signaled(self):
        child = mock.Mock(pid=543210, exitcode=None)
        child.is_alive.return_value = True
        with mock.patch.object(pixel_supervisor.base, "process_identity", side_effect=[(1, 123), (1, 999)]), \
             mock.patch.object(pixel_supervisor.os, "killpg") as killpg, \
             mock.patch.object(pixel_supervisor.os, "kill") as kill:
            result = pixel_supervisor.bounded_capture("/synthetic/adb", binding(), "/synthetic/out",
                now=lambda: binding()["originNs"] + 500_000_000_000, process_factory=lambda **kwargs: child)
        killpg.assert_not_called(); kill.assert_not_called()
        self.assertTrue(result["captureChildStillAlive"])


class OriginalClock(unittest.TestCase):
    def test_original_ticks_capture_once_cannot_overwrite_origin(self):
        read_bytes = Path.read_bytes
        read_text = Path.read_text
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            wrapper = SOURCE / "pixel_supervisor.py"
            argv = b"\0".join([b"python3", str(wrapper).encode(), b"--output", str(out).encode(),
                b"--avd", b"synthetic-pixel", b"--expected-page-bytes", b"16384"])
            def binary(path):
                return argv if str(path) == "/proc/424242/cmdline" else read_bytes(path)
            def textual(path, *args, **kwargs):
                return stat(100000).decode() if str(path) == "/proc/424242/stat" else read_text(path, *args, **kwargs)
            with mock.patch.object(Path, "read_bytes", binary), mock.patch.object(Path, "read_text", textual), \
                 mock.patch.object(capture_origin.os, "sysconf", return_value=100):
                context = capture_origin.capture(out, 424242, 1, "synthetic-pixel")
                first = (out / "clock-context.json").read_bytes()
                self.assertEqual(context["originNs"], 1_000_000_000_000)
                with self.assertRaises(FileExistsError):
                    capture_origin.capture(out, 424242, 1, "synthetic-pixel")
                self.assertEqual((out / "clock-context.json").read_bytes(), first)


if __name__ == "__main__":
    unittest.main()
