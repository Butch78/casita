import importlib.util
import pathlib
import tempfile
import unittest
from unittest import mock


PATH = pathlib.Path(__file__).resolve().parents[1] / 'reports/2026-09-12-tar-reserved/run.py'
spec = importlib.util.spec_from_file_location('tar_reservation', PATH)
reservation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reservation)


class TarReservationTests(unittest.TestCase):
    def test_resume_skips_exited_and_reused_pids(self):
        entries = [dict(pid=10, started=1), dict(pid=11, started=2), dict(pid=12, started=3)]
        with mock.patch.object(reservation, 'processes', return_value={10: dict(started=1), 11: dict(started=99)}), \
             mock.patch.object(reservation.os, 'kill') as kill:
            result = reservation.resume(entries)
        kill.assert_called_once_with(10, reservation.signal.SIGCONT)
        self.assertEqual([r['status'] for r in result], ['exited', 'exited', 'resumed'])

    def test_watchdog_resumes_on_deadline_and_interrupts_controller(self):
        with tempfile.TemporaryDirectory() as temporary:
            ledger = pathlib.Path(temporary) / 'paused.json'
            state = dict(controller_pid=20, controller_started=7, resume_deadline=0,
                         paused=[dict(pid=10, started=1)], finished=False)
            ledger.write_text(reservation.json.dumps(state))
            with mock.patch.object(reservation, 'processes', return_value={20: dict(started=7), 10: dict(started=1)}), \
                 mock.patch.object(reservation.os, 'kill') as kill:
                reservation.watchdog(ledger)
            self.assertEqual(kill.call_args_list, [mock.call(10, reservation.signal.SIGCONT),
                                                   mock.call(20, reservation.signal.SIGTERM)])
            self.assertTrue(ledger.with_suffix('.watchdog.json').exists())

    def test_watchdog_does_not_signal_reused_controller(self):
        with tempfile.TemporaryDirectory() as temporary:
            ledger = pathlib.Path(temporary) / 'paused.json'
            ledger.write_text(reservation.json.dumps(dict(controller_pid=20, controller_started=7,
                resume_deadline=0, paused=[], finished=False)))
            with mock.patch.object(reservation, 'processes', return_value={20: dict(started=8)}), \
                 mock.patch.object(reservation.os, 'kill') as kill:
                reservation.watchdog(ledger)
            kill.assert_not_called()
