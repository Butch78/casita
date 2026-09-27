import json
import pathlib
import tempfile
import unittest
from benchmarks import all as all_suites, dashboard
from benchmarks.suites import durable_ledger as ledger, ledger_boundaries as boundaries
from benchmarks.suites import repository as common


class DurableLedgerTests(unittest.TestCase):
    def test_phase_parser_rejects_missing_durability_and_wrong_work(self):
        metrics = dict.fromkeys(ledger.METRICS, 0)
        metrics.update(groups=1, operations=8, max_group=8, journal_syncs=1, journal_frames=1)
        case = dict(records=64, writers=8, iterations=1, mode='journal', correctness=ledger.CORRECTNESS,
                    samples=[dict(phase=phase, iteration=0, nanos=100, operations=1 if phase.startswith('deletion-') else 8,
                                  metrics={**metrics, 'operations': 1 if phase.startswith('deletion-') else 8}) for phase in ledger.PHASES])
        def parse(value, trailer='test result: ok. 1 passed; 0 failed;'):
            return ledger.parse_sample('durable_ledger_sample ' + json.dumps(value) + '\n' + trailer, 64, 8, 1, 'journal')
        self.assertEqual(parse(case), case)
        for field, value in [('records', 1), ('writers', 1), ('iterations', 2), ('correctness', ''), ('samples', case['samples'][:-1])]:
            with self.subTest(field=field), self.assertRaises(common.BenchmarkError):
                parse({**case, field: value})
        for key, value in [('journal_syncs', 0), ('replacement_updates', 1), ('max_group', 65), ('groups', 0), ('journal_bytes', -1), ('operations', 7)]:
            invalid = json.loads(json.dumps(case))
            invalid['samples'][0]['metrics'][key] = value
            with self.subTest(metric=key), self.assertRaises(common.BenchmarkError):
                parse(invalid)
        with self.assertRaises(common.BenchmarkError):
            parse(case, 'test result: ok. 0 passed; 0 failed;')

    def test_incremental_gate_and_reference_compatibility(self):
        metrics = dict.fromkeys((*ledger.METRICS, *ledger.INCREMENTAL_METRICS), 0)
        metrics.update(groups=1, operations=1, max_group=1, journal_syncs=1, journal_frames=1, cached_edits=1)
        case = dict(records=4096, writers=1, iterations=1, mode='journal', context='readers', correctness=ledger.CORRECTNESS,
                    samples=[dict(phase=phase, iteration=0, nanos=100, operations=1, metrics=dict(metrics)) for phase in ledger.PHASES])
        def parse(value, context='readers', incremental=True):
            return ledger.parse_sample('durable_ledger_sample ' + json.dumps(value) + '\ntest result: ok. 1 passed; 0 failed;', 4096, 1, 1, 'journal', context, incremental)
        self.assertEqual(parse(case), case)
        for key,value in [('inventory_copies',1), ('inventory_diffs',1), ('cached_edits',0)]:
            bad = json.loads(json.dumps(case)); bad['samples'][0]['metrics'][key]=value
            with self.subTest(key=key), self.assertRaises(common.BenchmarkError): parse(bad)
        with self.assertRaises(common.BenchmarkError): parse(case,context='quiet')
        del case['context']
        for sample in case['samples']:
            for key in ledger.INCREMENTAL_METRICS: del sample['metrics'][key]
        self.assertEqual(parse(case,context='quiet',incremental=False),case)
        with self.assertRaises(common.BenchmarkError): parse(case,context='quiet',incremental=True)

    def test_every_boundary_has_exact_counter_gates(self):
        for boundary, positions in boundaries.CASES.items():
            for position in positions:
                metrics = dict.fromkeys(ledger.METRICS, 0)
                if boundary == 'checkpoint-record-bytes':
                    oversized = position >= 1048576
                    metrics.update(checkpoints=3 if oversized else 1, journal_frames=0 if oversized else 2,
                                   journal_syncs=6 if oversized else 4)
                    checked = 'checkpoints'
                elif boundary.startswith('checkpoint-'):
                    checkpoint = position == (257 if boundary == 'checkpoint-operations' else 4)
                    metrics.update(checkpoints=int(checkpoint), journal_frames=int(not checkpoint), journal_syncs=2 if checkpoint else 1)
                    checked = 'checkpoints'
                elif boundary == 'group-size':
                    groups = (position + 63) // 64
                    metrics.update(groups=groups, operations=position, journal_frames=groups, journal_syncs=groups, max_group=min(position, 64))
                    checked = 'groups'
                else:
                    metrics.update(checkpoints=position, replacement_updates=int(position == 0))
                    checked = 'replacement_updates'
                case = dict(boundary=boundary, position=position, nanos=100, metrics=metrics, correctness=boundaries.CORRECTNESS)
                def parse(value):
                    return boundaries.parse_sample('ledger_boundary_sample ' + json.dumps(value) + '\ntest result: ok. 1 passed; 0 failed;', boundary, position)
                self.assertEqual(parse(case), case)
                metrics[checked] += 1
                with self.subTest(boundary=boundary, position=position), self.assertRaises(common.BenchmarkError):
                    parse(case)

    def test_resource_additions_remove_record_size_cliff(self):
        for position in boundaries.CASES['checkpoint-record-bytes']:
            metrics = dict.fromkeys(ledger.METRICS, 0)
            metrics.update(journal_frames=3, journal_syncs=3, journal_bytes=12696)
            case = dict(boundary='checkpoint-record-bytes', position=position, nanos=100,
                        journal_encoding='resource-additions-v2', metrics=metrics, correctness=boundaries.CORRECTNESS)
            def parse(value):
                return boundaries.parse_sample('ledger_boundary_sample '+json.dumps(value)+'\ntest result: ok. 1 passed; 0 failed;', 'checkpoint-record-bytes', position)
            self.assertEqual(parse(case), case)
            with self.assertRaises(common.BenchmarkError):
                parse({**case, 'journal_encoding':'unknown'})
            for field in ('checkpoints', 'journal_bytes', 'journal_syncs', 'journal_frames'):
                wrong = {**case, 'metrics': {**metrics, field: metrics[field]+1}}
                with self.subTest(position=position, field=field), self.assertRaises(common.BenchmarkError):
                    parse(wrong)

    def test_all_and_dashboard_registration(self):
        for suite in ('durable-ledger', 'ledger-boundaries'):
            args = all_suites.suite_arguments(suite, pathlib.Path('/binaries'), 'smoke', 1)
            self.assertIn('/binaries/casita-lib-test', args)
            self.assertIn('--no-build', args)
            with tempfile.TemporaryDirectory() as temporary:
                path = pathlib.Path(temporary) / 'result.json'
                path.write_text(json.dumps(dict(result_schema=f'casita.{suite}.v1', complete=True, suite_id='state-and-publication',
                    environment={}, configuration={}, samples=[dict(status='ok', operation='register', entries=64, wall_seconds=0.1)])))
                self.assertEqual(len(dashboard.normalize_result(path)['observations']), 1)
                value = json.loads(path.read_text())
                value['complete'] = False
                path.write_text(json.dumps(value))
                with self.assertRaises(ValueError):
                    dashboard.normalize_result(path)
