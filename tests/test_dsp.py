import base64
import math
from dataclasses import replace
from pathlib import Path
import sys
import unittest

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from studio.dsp import AnalysisConfig, Analyzer, VfoDemodulator, fft_size_for_rbw, demod_audio_scale


class AnalysisTests(unittest.TestCase):
    def tone(self, n, fs=64000, f=4000, amplitude=.25):
        return (amplitude*np.exp(2j*np.pi*f*np.arange(n)/fs)).astype(np.complex64)

    def test_psd_integral_marker_and_independent_measurements(self):
        a = Analyzer(AnalysisConfig(sample_rate=64000, fft_size=1024))
        a.process(self.tone(16384), 0)
        self.assertAlmostEqual(float(a.last_psd.sum()*a.bin_hz), .25**2, places=7)
        m = a.measurements(-20000, 20000, 0, 99, 6)
        self.assertAlmostEqual(m['cp_dbfs'], 20*np.log10(.25), places=5)
        self.assertAlmostEqual(m['markers'][0]['frequency_hz'], 4000, places=3)
        self.assertEqual(len(m['markers']), 1)
        self.assertGreater(m['obw_hz'], 0)
        for detector in ('sample', 'positive_peak', 'negative_peak', 'rms'):
            a.update_config(replace(a.config, detector=detector, ref_level=30))
            a.snapshot(50000, 0, 32)
            self.assertEqual(a.measurements(-20000, 20000, 0)['cp_dbfs'], m['cp_dbfs'])

    def test_chunking_and_rendering_do_not_change_frame_denominator(self):
        cfg = AnalysisConfig(sample_rate=64000, fft_size=1024, stats_mode='cumulative')
        x = self.tone(17001)
        whole, split = Analyzer(cfg), Analyzer(cfg)
        rw = whole.process(x, 0)
        cursor = 0
        ends = []
        for n in (17, 301, 99, 2048, 7, 6000, 8529):
            part = x[cursor:cursor+n]
            r = split.process(part, cursor)
            ends.extend(r['fft_frame_end_samples'].tolist())
            split.snapshot(64000, 0, 48)
            cursor += len(part)
        self.assertEqual(whole.total_valid_frames, 1+(len(x)-1024)//512)
        self.assertEqual(whole.total_valid_frames, split.total_valid_frames)
        self.assertEqual(ends, rw['fft_frame_end_samples'].tolist())
        h, denominator = split.density_statistics()
        np.testing.assert_array_equal(h.sum(axis=1), np.full(1024, denominator))
        np.testing.assert_allclose(whole.last_psd, split.last_psd, atol=1e-14)

    def test_window_frames_off_and_gap(self):
        cfg = AnalysisConfig(sample_rate=64000, fft_size=1024,
                             persistence_mode='window', persistence_unit='frames', persistence_frames=3)
        a = Analyzer(cfg)
        a.process(self.tone(20000), 0)
        hist, denominator = a.density_statistics()
        self.assertEqual(denominator, 3)
        np.testing.assert_array_equal(hist.sum(axis=1), 3)
        a.update_config(replace(a.config, persistence_mode='off'))
        a.process(self.tone(2048), 20000)
        hist, denominator = a.density_statistics()
        self.assertEqual(denominator, 1)
        np.testing.assert_array_equal(hist.sum(axis=1), 1)
        a.process(self.tone(2048), 25000)
        self.assertEqual(a.gap_samples, 2952)
        a.note_gap(100)
        a.process(self.tone(2048), 27148)
        self.assertEqual(a.gap_samples, 3052)
        snap = a.snapshot(64000, 0, 32)
        self.assertTrue(snap['waterfall']['gap_rows'])

    def test_exponential_weights_and_cumulative_stats_are_separate(self):
        cfg = AnalysisConfig(sample_rate=64000, fft_size=1024, persistence_mode='exponential',
                             persistence_seconds=.02, persistence_unit='time_constant')
        a = Analyzer(cfg)
        r = a.process(self.tone(10000), 0)
        n = r['frames_processed']
        expected = sum(np.exp(-np.arange(n)*512/64000/.02))
        hist, denominator = a.density_statistics()
        self.assertAlmostEqual(denominator, expected, places=10)
        np.testing.assert_allclose(hist.sum(axis=1), expected)
        a.update_config(replace(a.config, stats_mode='cumulative'))
        hist, denominator = a.density_statistics()
        self.assertEqual(denominator, n)
        np.testing.assert_array_equal(hist.sum(axis=1), n)

    def test_conversion_preserves_time_retain_clears_density(self):
        a = Analyzer(AnalysisConfig(sample_rate=64000, fft_size=1024, persistence_seconds=.2))
        a.process(self.tone(4096), 0)
        n = a.total_valid_frames
        result = a.update_config(replace(a.config, persistence_unit='time_constant', switch_policy='convert'))
        self.assertFalse(result['history_cleared'])
        self.assertAlmostEqual(a.config.persistence_seconds, .2/np.log(2))
        self.assertEqual(a.total_valid_frames, n)
        result = a.update_config(replace(a.config, persistence_unit='frames', switch_policy='retain'))
        self.assertTrue(result['history_cleared'])
        self.assertEqual(a.total_valid_frames, 0)
        result = a.update_config(replace(a.config, fft_size=2048))
        self.assertEqual(result['reason'], 'analysis_conditions_changed')

    def test_bounded_window_and_image_layout(self):
        a = Analyzer(AnalysisConfig(sample_rate=64000, fft_size=1024, persistence_mode='window',
                                    persistence_seconds=100, max_density_history_mb=.01))
        a.process(self.tone(64000), 0)
        snap = a.snapshot(50000, 0, 80)
        self.assertTrue(snap['density']['window_truncated'])
        self.assertLessEqual(snap['density']['window_history_bytes'], .01*1024*1024)
        for key in ('density', 'waterfall'):
            image = snap[key]
            self.assertEqual(len(base64.b64decode(image['data_b64'])), image['width']*image['height'])
        self.assertEqual(a.measurements(-33000, 30000, 0)['status'], 'unavailable')
        with self.assertRaises(ValueError): a.measurements(-30000, 30000, 0, 100)

    def test_rbw_and_invalid_frames(self):
        self.assertEqual(fft_size_for_rbw(2048000, 750, 'hann'), 4096)
        a = Analyzer(AnalysisConfig(sample_rate=64000, fft_size=1024))
        x = self.tone(2048)
        x[0] = np.nan
        r = a.process(x, 0)
        self.assertEqual(r['frames_processed'], 2)
        self.assertEqual(a.invalid_frames, 1)

    def test_rbw_selection_limits_and_invalid_sample_rates(self):
        self.assertEqual(fft_size_for_rbw(2048000, 1e9, 'hann'), 128)
        self.assertEqual(fft_size_for_rbw(2048000, 1e-9, 'hann'), 32768)
        self.assertEqual(fft_size_for_rbw(2048000, 1e9, 'rectangular'), 128)
        for invalid in [0, -1, math.nan, math.inf, -math.inf]:
            with self.assertRaises(ValueError): fft_size_for_rbw(invalid, 600)
            with self.assertRaises(ValueError): fft_size_for_rbw(2048000, invalid)

    def test_window_limit_reports_effective_seconds_and_is_not_sticky(self):
        cfg = AnalysisConfig(sample_rate=64000, fft_size=1024,
                             persistence_mode='window', persistence_seconds=100,
                             max_density_history_mb=.01)
        a = Analyzer(cfg)
        a.process(self.tone(64000), 0)
        density = a.snapshot(64000, 0, 32)['density']
        self.assertEqual(density['window_capacity_frames'], 10)
        self.assertEqual(density['window_frames_retained'], 10)
        self.assertAlmostEqual(density['window_seconds_retained'], .08)
        self.assertEqual(density['window_history_limit_bytes'], int(.01*1024*1024))
        self.assertEqual(density['window_requested_seconds'], 100)
        self.assertTrue(density['window_truncated'])
        self.assertTrue(density['window_memory_limit_expected'])
        a.update_config(replace(a.config, ref_level=-30))
        self.assertTrue(a.snapshot(64000, 0, 32)['density']['window_truncated'])
        # A shorter requested window is applied immediately, including when
        # the source has stopped and no new USB block will arrive.
        a.update_config(replace(a.config, persistence_seconds=.016))
        density = a.snapshot(64000, 0, 32)['density']
        self.assertEqual(density['window_frames_retained'], 2)
        self.assertAlmostEqual(density['window_seconds_retained'], .016)
        self.assertFalse(density['window_truncated'])
        self.assertFalse(density['window_memory_limit_expected'])
        # Appending a batch can temporarily exceed capacity, but if all
        # removed entries are ordinary time expiry no truncation occurred.
        a = Analyzer(replace(cfg, persistence_seconds=.08))
        a.process(self.tone(64000), 0)
        self.assertFalse(a.snapshot(64000, 0, 32)['density']['window_truncated'])
        a = Analyzer(replace(cfg, persistence_unit='frames', persistence_frames=20))
        a.process(self.tone(64000), 0)
        a.update_config(replace(a.config, ref_level=-30))
        self.assertTrue(a.snapshot(64000, 0, 32)['density']['window_truncated'])
        a.update_config(replace(a.config, persistence_frames=5))
        self.assertFalse(a.snapshot(64000, 0, 32)['density']['window_truncated'])


class DemodulatorTests(unittest.TestCase):
    def synth(self, mode, fs=512000, duration=.3, offset=74000):
        t = np.arange(int(fs*duration))/fs
        if mode == 'AM':
            return (.15*(1+.5*np.cos(2*np.pi*1000*t))*np.exp(2j*np.pi*offset*t)).astype(np.complex64)
        deviation = 75000 if mode == 'WFM' else 5000
        return (.15*np.exp(2j*np.pi*offset*t+1j*deviation/1000*np.sin(2*np.pi*1000*t))).astype(np.complex64)

    def test_all_modes_recover_tone_and_streaming_boundaries(self):
        for mode, bw in [('AM',12000), ('NFM',25000), ('WFM',200000)]:
            with self.subTest(mode=mode):
                x = self.synth(mode)
                continuous = VfoDemodulator(512000, 74000, bw, mode).process(x)
                d = VfoDemodulator(512000, 74000, bw, mode)
                cuts = [0, 11, 5000, 5003, 17000, 65000, 110001, len(x)]
                chunked = np.concatenate([d.process(x[a:b]) for a,b in zip(cuts[:-1],cuts[1:])])
                self.assertEqual(len(chunked), len(continuous))
                np.testing.assert_allclose(chunked[3200:], continuous[3200:], atol=2e-4, rtol=2e-3)
                audio = chunked[3200:]
                freq = np.fft.rfftfreq(len(audio), 1/32000)
                peak = freq[np.argmax(np.abs(np.fft.rfft(audio*np.hanning(len(audio)))))]
                self.assertAlmostEqual(peak, 1000, delta=6)
                self.assertTrue(np.isfinite(audio).all())
                # Float demodulation is deliberately not a PCM limiter; a
                # nominal NFM sine can include small filter-response overshoot.
                self.assertLessEqual(np.max(np.abs(audio)), 1.05)

    def test_rejects_filter_outside_band(self):
        with self.assertRaises(ValueError): VfoDemodulator(512000, 250000, 50000, 'AM')
        with self.assertRaises(ValueError): VfoDemodulator(512000, 0, 10000, 'SSB')

    def test_fixed_wfm_headroom_and_pcm_sink_owns_clipping(self):
        self.assertEqual(demod_audio_scale('WFM'), .5)
        self.assertEqual(demod_audio_scale('AM'), 1)
        self.assertEqual(demod_audio_scale('NFM'), 1)
        x = self.synth('WFM')
        audio = VfoDemodulator(512000, 74000, 200000, 'WFM').process(x)[3200:]
        self.assertGreater(float(np.sqrt(np.mean(audio**2))), .32)
        self.assertLess(float(np.sqrt(np.mean(audio**2))), .36)
        self.assertLess(float(np.max(np.abs(audio))), .55)
        t = np.arange(153600)/512000
        x = (.15*np.exp(2j*np.pi*74000*t+10j*np.sin(2*np.pi*1000*t))).astype(np.complex64)
        audio = VfoDemodulator(512000, 74000, 50000, 'NFM').process(x)[3200:]
        self.assertGreater(float(np.max(np.abs(audio))), 1.2)


if __name__ == '__main__':
    unittest.main()
