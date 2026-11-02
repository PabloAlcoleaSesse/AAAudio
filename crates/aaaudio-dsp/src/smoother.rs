//! Real-time parameter smoothing to prevent zipper noise and step discontinuities.

/// One-pole exponential smoother (`y[n] = y[n-1] + coeff * (target - y[n-1])`)
/// with a snap-to-target threshold to eliminate residual denormal drift.
#[derive(Debug, Clone, Copy)]
pub struct SmoothedParam {
    current: f32,
    target: f32,
    coeff: f32,
}

impl SmoothedParam {
    /// Creates a new smoothed parameter initialized to `initial_val`.
    /// `smoothing_time_ms` defines the 1-pole time constant $\tau$ in milliseconds
    /// (~5$\tau$ reaches 99.3% of the step transition).
    pub fn new(initial_val: f32, smoothing_time_ms: f32, sample_rate: f32) -> Self {
        let mut param = Self {
            current: initial_val,
            target: initial_val,
            coeff: 1.0,
        };
        param.set_smoothing_time(smoothing_time_ms, sample_rate);
        param
    }

    /// Recomputes the filter pole coefficient for a given time constant and sample rate.
    #[inline]
    pub fn set_smoothing_time(&mut self, smoothing_time_ms: f32, sample_rate: f32) {
        if smoothing_time_ms <= 0.0 || sample_rate <= 0.0 {
            self.coeff = 1.0;
        } else {
            let tau_samples = (smoothing_time_ms * 0.001) * sample_rate;
            self.coeff = 1.0 - (-1.0 / tau_samples).exp();
        }
    }

    /// Updates the target value to ramp toward.
    #[inline(always)]
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Immediately snaps both `current` and `target` without ramping.
    #[inline(always)]
    pub fn set_immediate(&mut self, val: f32) {
        self.current = val;
        self.target = val;
    }

    /// Returns `true` if the parameter is still actively ramping toward `target`.
    #[inline(always)]
    pub fn is_smoothing(&self) -> bool {
        (self.target - self.current).abs() > 1.0e-6
    }

    /// Advances the smoother by one sample and returns the smoothed value.
    #[inline(always)]
    pub fn next_sample(&mut self) -> f32 {
        let diff = self.target - self.current;
        if diff.abs() > 1.0e-6 {
            self.current += self.coeff * diff;
        } else {
            self.current = self.target;
        }
        self.current
    }

    /// Returns the current instantaneous value without advancing state.
    #[inline(always)]
    pub fn current(&self) -> f32 {
        self.current
    }

    /// Returns the target value.
    #[inline(always)]
    pub fn target(&self) -> f32 {
        self.target
    }
}
