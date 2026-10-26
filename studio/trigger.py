"""Sample-clock level trigger, with retriggered post-roll and save-aware Auto.

Input levels represent one analysis block. Crossings therefore have block-level
resolution; dwell and post-roll deadlines are expressed in source samples and
do not depend on drawing rate or the system wall clock. AND/OR combines latched
condition states. A new aggregate false-to-true transition is a detection.
"""
from __future__ import annotations

from dataclasses import dataclass
import math


def _nonnegative(value, name):
    number = float(value)
    if not math.isfinite(number) or number < 0:
        raise ValueError(f"{name} must be nonnegative and finite")
    return number


def _metric(value):
    text = str(value).lower()
    if text in ("peak", "peak_dbfs", "peak_level"):
        return "peak"
    if text in ("power", "integrated", "integrated_power", "channel_power", "cp_dbfs", "power_dbfs"):
        return "power"
    return text


@dataclass
class _Condition:
    metric: str
    threshold: float
    hysteresis: float
    minimum_samples: int
    direction: str = "above"
    high: bool = False
    candidate_start: int | None = None
    qualified_at: int | None = None

    def reset(self):
        self.high = False
        self.candidate_start = None
        self.qualified_at = None

    def observe(self, value, start, end):
        if value is None or not math.isfinite(value):
            self.reset()
            return False
        below = self.direction == "below"
        tripped = value <= self.threshold if below else value >= self.threshold
        released = value > self.threshold+self.hysteresis if below else value < self.threshold-self.hysteresis
        if self.high:
            if released:
                self.reset()
            return self.high
        if not tripped:
            self.candidate_start = None
            return False
        if self.candidate_start is None:
            self.candidate_start = start
        ready = self.candidate_start+self.minimum_samples
        if ready <= end:
            self.high = True
            self.qualified_at = ready
        return self.high


class TriggerController:
    """No I/O: evaluate levels and return source-indexed recording events.

    ``trigger`` starts a record, ``extend`` lengthens that same record, and
    ``complete`` supplies its exclusive source end sample. Auto stays armed in
    ``saving`` until the engine calls ``recording_finished`` after finalization.
    The engine must Disarm after recording caps/errors and stop recording on a
    manual Disarm. Missing/invalid level values never count as a valid crossing.
    """

    def __init__(self, sample_rate, pre_seconds=1, post_seconds=2,
                 mode="one_shot", threshold_db=-55, metric="peak",
                 hysteresis_db=3, min_duration_s=0, rearm_s=.2,
                 logic="OR", conditions=None):
        self.sample_rate = float(sample_rate)
        if not math.isfinite(self.sample_rate) or self.sample_rate <= 0:
            raise ValueError("sample_rate must be positive and finite")
        self.pre_seconds = _nonnegative(pre_seconds, "pre_seconds")
        self.post_seconds = _nonnegative(post_seconds, "post_seconds")
        self.rearm_s = _nonnegative(rearm_s, "rearm_s")
        self.hysteresis_db = _nonnegative(hysteresis_db, "hysteresis_db")
        self.min_duration_s = _nonnegative(min_duration_s, "min_duration_s")
        self.threshold_db = float(threshold_db)
        if not math.isfinite(self.threshold_db):
            raise ValueError("threshold_db must be finite")
        self.metric = _metric(metric)
        self.mode = str(mode).lower().replace("-", "_")
        if self.mode in ("oneshot", "one shot"):
            self.mode = "one_shot"
        if self.mode not in ("one_shot", "auto"):
            raise ValueError("trigger mode must be one_shot or auto")
        self.logic = str(logic).upper()
        if self.logic not in ("AND", "OR"):
            raise ValueError("trigger logic must be AND or OR")
        definitions = list(conditions or [{"metric": self.metric, "threshold_db": self.threshold_db}])
        if not definitions:
            raise ValueError("at least one trigger condition is required")
        self.conditions = []
        for item in definitions:
            threshold = float(item.get("threshold_db", item.get("threshold", self.threshold_db)))
            if not math.isfinite(threshold):
                raise ValueError("condition threshold must be finite")
            direction = str(item.get("direction", "above")).lower()
            direction = {"rising": "above", "falling": "below"}.get(direction, direction)
            if direction not in ("above", "below"):
                raise ValueError("condition direction must be above or below")
            dwell = _nonnegative(item.get("min_duration_s", self.min_duration_s), "min_duration_s")
            hysteresis = _nonnegative(item.get("hysteresis_db", self.hysteresis_db), "hysteresis_db")
            self.conditions.append(_Condition(_metric(item.get("metric", self.metric)), threshold,
                                               hysteresis, int(math.ceil(dwell*self.sample_rate)), direction))
        self.pre_samples = int(round(self.pre_seconds*self.sample_rate))
        self.post_samples = int(round(self.post_seconds*self.sample_rate))
        self.rearm_samples = int(round(self.rearm_s*self.sample_rate))
        self.armed = False
        self.state = "disarmed"
        self.trigger_sample = self.start_sample = self.end_sample = None
        self.record_sequence = 0
        self._combined = False
        self._last_end = None
        self._arm_sample = None
        self._ready_sample = None
        self._rearm_until = None
        self._initial_pre_seconds = 0.0
        self._needs_release = False
        self.last_invalid_metrics = []

    @property
    def recording(self):
        return self.state == "recording"

    @property
    def active(self):
        return self.armed

    def _reset_conditions(self):
        self._combined = False
        for condition in self.conditions:
            condition.reset()

    def arm(self, sample_index=None, prebuffer_seconds=None):
        if self.recording or self.state == "saving":
            raise RuntimeError("finish the current record before arming again")
        self.armed = True
        self.state = "filling" if self.pre_samples else "waiting"
        self._arm_sample = None if sample_index is None else self._index(sample_index)
        self._initial_pre_seconds = _nonnegative(prebuffer_seconds or 0, "prebuffer_seconds")
        remaining = max(0, self.pre_samples-int(self._initial_pre_seconds*self.sample_rate))
        self._ready_sample = None if self._arm_sample is None else self._arm_sample+remaining
        if not remaining:
            self.state = "waiting"
        self._rearm_until = None
        self._last_end = None
        self.trigger_sample = self.start_sample = self.end_sample = None
        self.last_invalid_metrics = []
        self._needs_release = False
        self._reset_conditions()
        return self.snapshot()

    def disarm(self):
        self.armed = False
        self.state = "disarmed"
        self._reset_conditions()
        return self.snapshot()

    @staticmethod
    def _index(value):
        number = int(value)
        if number < 0 or number != value:
            raise ValueError("sample index must be a nonnegative integer")
        return number

    def _edge(self, values, start, end):
        states = []
        self.last_invalid_metrics = []
        aliases = {"peak": ("peak", "peak_dbfs", "peak_level"),
                   "power": ("power", "integrated_power", "channel_power", "cp_dbfs", "power_dbfs")}
        for condition in self.conditions:
            value = None
            for key in aliases.get(condition.metric, (condition.metric,)):
                if key in values:
                    try:
                        value = float(values[key])
                    except (ValueError, TypeError):
                        value = None
                    break
            if value is None or not math.isfinite(value):
                self.last_invalid_metrics.append(condition.metric)
            states.append(condition.observe(value, start, end))
        combined = all(states) if self.logic == "AND" else any(states)
        crossing = combined and not self._combined
        self._combined = combined
        if not crossing:
            return None
        times = [condition.qualified_at for condition in self.conditions if condition.high]
        event_at = max(times) if self.logic == "AND" else min(times)
        return max(start, event_at)

    def _finish(self):
        event = {"type": "complete", "sample_index": self.end_sample,
                 "end_sample": self.end_sample, "record_sequence": self.record_sequence,
                 "reason": "post_trigger_complete", "rearm_mode": self.mode}
        self._reset_conditions()
        if self.mode == "auto":
            self.state = "saving"
            self._needs_release = True
        else:
            self.state = "disarmed"
            self.armed = False
        return event

    def recording_finished(self, sample_index=None, success=True):
        if not success:
            return self.disarm()
        if self.mode != "auto" or self.state != "saving":
            return self.snapshot()
        finished = self._last_end if sample_index is None else self._index(sample_index)
        if finished is None:
            finished = self.end_sample or 0
        finished = max(finished, self.end_sample or 0)
        self._rearm_until = finished+self.rearm_samples
        self._arm_sample = finished
        self._ready_sample = finished+self.pre_samples
        self.state = "rearming"
        self._reset_conditions()
        return self.snapshot()

    notify_saved = recording_finished

    def evaluate(self, level_values, block_start_sample, block_samples):
        start = self._index(block_start_sample)
        count = self._index(block_samples)
        if count == 0 or not self.armed:
            return []
        end = start+count
        if self._last_end is not None and start < self._last_end:
            raise ValueError("trigger blocks must not overlap or go backwards")
        gap = self._last_end is not None and start > self._last_end
        if gap:
            self._reset_conditions()
            if not self.recording and self.state != "saving":
                self._arm_sample = start
                self._ready_sample = start+self.pre_samples
        self._last_end = end
        if self.state == "saving":
            return []
        values = dict(level_values or {})
        if self._arm_sample is None:
            self._arm_sample = start
            self._ready_sample = start+max(0, self.pre_samples-int(self._initial_pre_seconds*self.sample_rate))
        events = []
        effective_start = start
        if not self.recording:
            if self._rearm_until is not None:
                effective_start = max(effective_start, self._rearm_until)
            available = values.get("available_pre_seconds")
            if available is not None:
                available = _nonnegative(available, "available_pre_seconds")
                if available*self.sample_rate >= self.pre_samples:
                    ready = effective_start
                else:
                    ready = max(effective_start, start+self.pre_samples-int(available*self.sample_rate))
            else:
                ready = max(effective_start, self._ready_sample or start)
            if ready >= end:
                self.state = "rearming" if self._rearm_until is not None else "filling"
                self._reset_conditions()
                return []
            effective_start = ready
            if self._rearm_until is not None:
                events.append({"type": "rearmed", "sample_index": effective_start})
                self._rearm_until = None
            self.state = "waiting"
        crossing = self._edge(values, effective_start, end)
        if not self.recording and self._needs_release:
            # Auto requires a fresh aggregate edge after hysteresis release;
            # a continuously high carrier must not create endless new files.
            if not self._combined:
                self._needs_release = False
            return events
        if self.recording:
            if crossing is not None and crossing <= self.end_sample:
                self.end_sample = max(self.end_sample, crossing+self.post_samples)
                events.append({"type": "extend", "sample_index": crossing,
                               "end_sample": self.end_sample, "record_sequence": self.record_sequence})
            if self.end_sample <= end:
                events.append(self._finish())
            return events
        if crossing is not None:
            self.record_sequence += 1
            self.trigger_sample = crossing
            self.start_sample = max(0, crossing-self.pre_samples)
            self.end_sample = crossing+self.post_samples
            self.state = "recording"
            events.append({"type": "trigger", "sample_index": crossing,
                           "trigger_sample": crossing, "start_sample": self.start_sample,
                           "end_sample": self.end_sample, "record_sequence": self.record_sequence,
                           "pre_seconds": self.pre_seconds, "post_seconds": self.post_seconds})
            if self.end_sample <= end:
                events.append(self._finish())
        return events

    def snapshot(self):
        return {"armed": self.armed, "state": self.state, "recording": self.recording,
                "mode": self.mode, "logic": self.logic, "pre_seconds": self.pre_seconds,
                "post_seconds": self.post_seconds, "rearm_s": self.rearm_s,
                "trigger_sample": self.trigger_sample, "start_sample": self.start_sample,
                "end_sample": self.end_sample, "record_sequence": self.record_sequence,
                "last_invalid_metrics": list(self.last_invalid_metrics),
                "waiting_for_release": self._needs_release,
                "conditions": [{"metric": c.metric, "threshold_db": c.threshold,
                                "hysteresis_db": c.hysteresis,
                                "min_duration_s": c.minimum_samples/self.sample_rate,
                                "direction": c.direction, "active": c.high}
                               for c in self.conditions]}
