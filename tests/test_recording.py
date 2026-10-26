from __future__ import annotations

import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import wave

import numpy as np

from studio.recording import IQRecorder, PreTriggerBuffer, ReplayReader, WavRecorder
from studio.trigger import TriggerController


def raw(samples, value=128):
    return bytes([value, value])*samples


class PreTriggerTests(unittest.TestCase):
    def test_capacity_trim_and_exact_end(self):
        ring = PreTriggerBuffer(10, max_bytes=20)
        ring.append(0, raw(8, 1))
        ring.append(8, raw(8, 2))
        self.assertEqual(ring.available_seconds, 1)
        chunks, seconds = ring.get(.6, end_sample=12)
        self.assertAlmostEqual(seconds, .6)
        self.assertEqual(chunks, [(6, raw(2, 1)), (8, raw(4, 2))])
        self.assertLessEqual(ring.bytes_used, 20)

    def test_gap_discards_unavailable_pre_time(self):
        ring = PreTriggerBuffer(10, 100)
        ring.append(0, raw(10))
        ring.append(20, raw(3))
        chunks, seconds = ring.get(2)
        self.assertEqual(chunks, [(20, raw(3))])
        self.assertEqual(seconds, .3)
        self.assertEqual(ring.gap_count, 1)

    def test_duplicate_overlap_and_invalid_pairs(self):
        ring = PreTriggerBuffer(10, 100)
        ring.append(0, raw(5))
        ring.append(3, raw(5))
        self.assertEqual(sum(len(data) for _, data in ring.get(2)[0]), 16)
        with self.assertRaises(ValueError):
            ring.append(8, b'\x00')


class IQRecordingTests(unittest.TestCase):
    def test_sigmf_replay_pre_roll_overlap_and_gap(self):
        with tempfile.TemporaryDirectory() as temporary:
            writer = IQRecorder(temporary, 10, 32e6,
                                {"gain_db": 20.7, "stream_start_utc": "2026-01-01T00:00:00Z"})
            writer.start([(100, raw(5, 1))])
            self.assertEqual(writer.write(raw(8, 2), 102), 5)
            self.assertEqual(writer.write(raw(3, 3), 120), 3)
            writer.mark_trigger(104, {"metric": "peak"})
            result = writer.stop()
            self.assertEqual(result['samples'], 13)
            data = Path(result['data_path']).read_bytes()
            self.assertEqual(data, raw(5, 1)+raw(5, 2)+raw(3, 3))
            meta = json.loads(Path(result['meta_path']).read_text(encoding='utf-8'))
            self.assertEqual(meta['global']['core:datatype'], 'cu8')
            self.assertEqual(meta['global']['core:sha512'], hashlib.sha512(data).hexdigest())
            self.assertEqual(meta['captures'][0]['core:global_index'], 100)
            self.assertEqual(meta['captures'][1]['core:sample_start'], 10)
            self.assertEqual(meta['captures'][1]['core:global_index'], 120)
            self.assertEqual(meta['captures'][0]['core:datetime'], '2026-01-01T00:00:10.000000Z')
            with ReplayReader(result['meta_path']) as replay:
                self.assertEqual(replay.total_samples, 13)
                self.assertEqual(replay.global_sample_for(11), 121)
                self.assertEqual(replay.seek_seconds(.5), 5)
                self.assertEqual(replay.read_chunk(4), (5, raw(4, 2)))
                self.assertEqual(replay.seek_seconds(100), 13)
                self.assertEqual(replay.read_chunk(1), (13, b''))

    def test_size_cap_finalizes_only_this_recorder(self):
        with tempfile.TemporaryDirectory() as temporary:
            first = IQRecorder(temporary, 10, 1e6, {}, max_bytes=10)
            other = IQRecorder(temporary, 10, 1e6, {})
            first.start()
            other.start()
            self.assertEqual(first.write(raw(10), 0), 5)
            self.assertFalse(first.active)
            self.assertTrue(first.limit_reached)
            self.assertEqual(first.stop_reason, 'size_limit')
            self.assertTrue(other.active)
            other.write(raw(2), 0)
            other.stop()
            meta = json.loads(first.meta_path.read_text(encoding='utf-8'))
            self.assertEqual(meta['global']['spectrum_studio:recording']['state'], 'incomplete')

    def test_source_duration_cap_counts_gaps(self):
        with tempfile.TemporaryDirectory() as temporary:
            writer = IQRecorder(temporary, 10, 1e6, {}, max_seconds=1)
            writer.start()
            writer.write(raw(2), 0)
            self.assertEqual(writer.write(raw(10), 8), 2)
            self.assertEqual(writer.samples_written, 4)
            self.assertEqual(writer.stop_reason, 'duration_limit')
            self.assertFalse(writer.active)

    def test_write_failure_preserves_prior_samples(self):
        class FailedWrite:
            def __init__(self, underlying):
                self.underlying = underlying
            def write(self, _):
                raise OSError('test disk full')
            def __getattr__(self, name):
                return getattr(self.underlying, name)
        with tempfile.TemporaryDirectory() as temporary:
            writer = IQRecorder(temporary, 10, 1e6, {})
            writer.start()
            writer.write(raw(2, 3), 0)
            writer._file = FailedWrite(writer._file)
            self.assertEqual(writer.write(raw(3), 2), 0)
            self.assertFalse(writer.active)
            self.assertIn('disk full', writer.last_error)
            self.assertEqual(writer.data_path.read_bytes(), raw(2, 3))
            self.assertEqual(writer.stop_reason, 'io_error')

    def test_start_permission_error_is_reported(self):
        with tempfile.TemporaryDirectory() as temporary:
            writer = IQRecorder(temporary, 10, 1e6, {})
            with mock.patch.object(Path, 'open', side_effect=PermissionError('denied')):
                with self.assertRaises(PermissionError):
                    writer.start()
            self.assertFalse(writer.active)

    def test_replay_rejects_partial_sample_pair(self):
        with tempfile.TemporaryDirectory() as temporary:
            writer = IQRecorder(temporary, 10, 1e6, {})
            writer.start()
            writer.write(raw(2), 0)
            writer.stop()
            with writer.data_path.open('ab') as handle:
                handle.write(b'\x00')
            with self.assertRaises(ValueError):
                ReplayReader(writer.meta_path)

    def test_replay_rejects_unsupported_required_extension_or_channel_layout(self):
        with tempfile.TemporaryDirectory() as temporary:
            writer = IQRecorder(temporary, 10, 1e6, {})
            writer.start()
            writer.write(raw(2), 0)
            writer.stop()
            metadata = json.loads(writer.meta_path.read_text(encoding='utf-8'))
            metadata['global']['core:extensions'] = [{'name': 'unknown', 'version': '1.0.0', 'optional': False}]
            writer.meta_path.write_text(json.dumps(metadata), encoding='utf-8')
            with self.assertRaisesRegex(ValueError, 'required SigMF extension'):
                ReplayReader(writer.meta_path)
            metadata['global']['core:extensions'][0]['optional'] = True
            metadata['global']['core:num_channels'] = 2
            writer.meta_path.write_text(json.dumps(metadata), encoding='utf-8')
            with self.assertRaisesRegex(ValueError, 'one complex IQ channel'):
                ReplayReader(writer.meta_path)


class WavTests(unittest.TestCase):
    def test_pcm16_pcm24_and_independent_vfos(self):
        with tempfile.TemporaryDirectory() as temporary:
            for fmt, width in [('PCM16', 2), ('PCM24', 3)]:
                writer = WavRecorder(temporary, fmt, audio_rate=32000, format=fmt)
                writer.start({'frequency_hz': 32e6})
                writer.write(np.array([-1, -.5, 0, .5, 1], dtype=np.float32))
                result = writer.stop()
                with wave.open(result['path'], 'rb') as reader:
                    self.assertEqual(reader.getsampwidth(), width)
                    self.assertEqual(reader.getnchannels(), 1)
                    self.assertEqual(reader.getframerate(), 32000)
                    self.assertEqual(reader.getnframes(), 5)
                    data = reader.readframes(5)
                if width == 2:
                    values = np.frombuffer(data, dtype='<i2').astype(np.int32)
                    expected = [-32768, -16384, 0, 16384, 32767]
                else:
                    triples = np.frombuffer(data, dtype=np.uint8).reshape(-1, 3).astype(np.int32)
                    values = triples[:, 0] | (triples[:, 1] << 8) | (triples[:, 2] << 16)
                    values = (values ^ 0x800000)-0x800000
                    expected = [-8388608, -4194304, 0, 4194304, 8388607]
                np.testing.assert_array_equal(values, expected)

    def test_condition_change_splits_without_stopping_other_vfo(self):
        with tempfile.TemporaryDirectory() as temporary:
            first = WavRecorder(temporary, 'one')
            second = WavRecorder(temporary, 'two')
            first.start({'mode': 'AM'})
            second.start({'mode': 'FM'})
            first.write(np.ones(4)*.25)
            old_path = first.path
            split = first.split({'mode': 'NFM'})
            self.assertNotEqual(first.path, old_path)
            self.assertTrue(first.active)
            self.assertTrue(second.active)
            self.assertEqual(split['previous']['stop_reason'], 'conditions_changed')
            first.write(np.zeros(3))
            first.stop()
            second.stop()
            with wave.open(str(old_path), 'rb') as reader:
                self.assertEqual(reader.getnframes(), 4)

    def test_small_riff_cap_and_nonfinite_stop(self):
        with tempfile.TemporaryDirectory() as temporary:
            limited = WavRecorder(temporary, 'limited', max_bytes=6)
            limited.start()
            self.assertEqual(limited.write(np.zeros(10)), 3)
            self.assertTrue(limited.limit_reached)
            self.assertFalse(limited.active)
            invalid = WavRecorder(temporary, 'invalid')
            invalid.start()
            invalid.write(np.array([np.nan]))
            self.assertEqual(invalid.stop_reason, 'invalid_audio')
            self.assertFalse(invalid.active)


class TriggerTests(unittest.TestCase):
    def test_dwell_hysteresis_and_same_record_retrigger_extension(self):
        trigger = TriggerController(100, pre_seconds=0, post_seconds=.5,
                                    threshold_db=-50, hysteresis_db=3, min_duration_s=.1)
        trigger.arm()
        self.assertEqual(trigger.evaluate({'peak': -40}, 0, 5), [])
        events = trigger.evaluate({'peak': -40}, 5, 5)
        self.assertEqual(events[0]['type'], 'trigger')
        self.assertEqual(events[0]['sample_index'], 10)
        self.assertEqual(events[0]['end_sample'], 60)
        self.assertEqual(trigger.evaluate({'peak': -51}, 10, 10), [])
        trigger.evaluate({'peak': -54}, 20, 10)
        events = trigger.evaluate({'peak': -40}, 30, 10)
        self.assertEqual(events[0]['type'], 'extend')
        self.assertEqual(events[0]['end_sample'], 90)
        self.assertEqual(events[0]['record_sequence'], 1)
        self.assertEqual(trigger.evaluate({'peak': -60}, 40, 50)[0]['sample_index'], 90)
        self.assertFalse(trigger.armed)

    def test_retrigger_at_deadline_wins_before_completion(self):
        trigger = TriggerController(100, pre_seconds=0, post_seconds=.2)
        trigger.arm()
        trigger.evaluate({'peak': -40}, 0, 10)
        trigger.evaluate({'peak': -100}, 10, 10)
        # The preceding block ended exactly at the post deadline, so it finished.
        self.assertFalse(trigger.armed)
        trigger = TriggerController(100, pre_seconds=0, post_seconds=.25)
        trigger.arm()
        trigger.evaluate({'peak': -40}, 0, 10)
        trigger.evaluate({'peak': -100}, 10, 15)
        self.assertFalse(trigger.armed)

    def test_exact_boundary_with_long_dwell_extension(self):
        trigger = TriggerController(100, pre_seconds=0, post_seconds=.3,
                                    min_duration_s=.05)
        trigger.arm()
        trigger.evaluate({'peak': -40}, 0, 5)  # trigger at 5, end 35
        trigger.evaluate({'peak': -100}, 5, 25)
        events = trigger.evaluate({'peak': -40}, 30, 10)  # qualifies at 35
        self.assertEqual(events[0]['type'], 'extend')
        self.assertEqual(events[0]['sample_index'], 35)
        self.assertEqual(trigger.end_sample, 65)
        self.assertTrue(trigger.armed)

    def test_auto_waits_for_save_deadtime_release_and_fresh_crossing(self):
        trigger = TriggerController(100, pre_seconds=0, post_seconds=.1, mode='auto', rearm_s=.1)
        trigger.arm()
        events = trigger.evaluate({'peak': -40}, 0, 10)
        self.assertEqual([e['type'] for e in events], ['trigger', 'complete'])
        self.assertEqual(trigger.state, 'saving')
        self.assertTrue(trigger.armed)
        self.assertEqual(trigger.evaluate({'peak': -100}, 10, 10), [])
        trigger.recording_finished(20)
        self.assertEqual(trigger.evaluate({'peak': -40}, 20, 10), [])
        events = trigger.evaluate({'peak': -40}, 30, 10)
        self.assertEqual([e['type'] for e in events], ['rearmed'])
        self.assertFalse(trigger.recording)
        trigger.evaluate({'peak': -100}, 40, 10)
        events = trigger.evaluate({'peak': -40}, 50, 10)
        self.assertEqual(events[0]['type'], 'trigger')
        self.assertEqual(events[0]['record_sequence'], 2)

    def test_prebuffer_filling_can_use_existing_buffer(self):
        trigger = TriggerController(10, pre_seconds=1, post_seconds=1)
        trigger.arm()
        self.assertEqual(trigger.evaluate({'peak': -40}, 0, 5), [])
        self.assertEqual(trigger.evaluate({'peak': -40}, 5, 5), [])
        events = trigger.evaluate({'peak': -40}, 10, 5)
        self.assertEqual(events[0]['start_sample'], 0)
        trigger.disarm()
        trigger.arm(prebuffer_seconds=1)
        events = trigger.evaluate({'peak': -40}, 100, 5)
        self.assertEqual(events[0]['sample_index'], 100)
        self.assertEqual(events[0]['start_sample'], 90)

    def test_and_or_missing_values_and_gap_dwell(self):
        conditions = [{'metric': 'peak', 'threshold_db': -50},
                      {'metric': 'power', 'threshold_db': -60}]
        trigger = TriggerController(100, pre_seconds=0, conditions=conditions, logic='AND')
        trigger.arm()
        self.assertEqual(trigger.evaluate({'peak': -40, 'power': -70}, 0, 10), [])
        events = trigger.evaluate({'peak': -40, 'power': -50}, 10, 10)
        self.assertEqual(events[0]['type'], 'trigger')
        trigger = TriggerController(100, pre_seconds=0, min_duration_s=.1)
        trigger.arm()
        self.assertEqual(trigger.evaluate({'peak': -40}, 0, 5), [])
        self.assertEqual(trigger.evaluate({'peak': -40}, 10, 5), [])
        events = trigger.evaluate({'peak': -40}, 15, 5)
        self.assertEqual(events[0]['sample_index'], 20)
        trigger.disarm()
        trigger.arm()
        self.assertEqual(trigger.evaluate({'peak': float('nan')}, 20, 10), [])
        self.assertIn('peak', trigger.last_invalid_metrics)

    def test_recording_failure_disarms_auto(self):
        trigger = TriggerController(100, pre_seconds=0, post_seconds=0, mode='auto')
        trigger.arm()
        trigger.evaluate({'peak': -40}, 0, 10)
        trigger.recording_finished(success=False)
        self.assertFalse(trigger.armed)
        self.assertEqual(trigger.state, 'disarmed')


if __name__ == '__main__':
    unittest.main()
