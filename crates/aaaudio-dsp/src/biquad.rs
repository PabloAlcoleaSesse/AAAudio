//! 10-Band Stereo Parametric Equalizer using Robert Bristow-Johnson (RBJ)
//! Audio EQ Cookbook Biquad Filters and Transposed Direct Form II (TDF-II).

use serde::{Deserialize, Serialize};

/// Number of parametric EQ bands per channel strip.
pub const NUM_EQ_BANDS: usize = 10;

/// Default center frequencies (Hz) for a 10-band studio parametric EQ.
pub const DEFAULT_BAND_FREQS: [f32; NUM_EQ_BANDS] = [
    31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// Supported second-order IIR biquad filter topologies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum FilterType {
    Bypass = 0,
    Peaking = 1,
    LowShelf = 2,
    HighShelf = 3,
    HighPass = 4,
    LowPass = 5,
}

/// High-level user-facing parameters for a single Parametric EQ band.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EqBandParams {
    pub band_idx: u8,
    pub enabled: bool,
    pub filter_type: FilterType,
    pub freq_hz: f32,
    pub q: f32,
    pub gain_db: f32,
}

impl EqBandParams {
    pub fn default_for_band(band_idx: usize) -> Self {
        let idx = band_idx.min(NUM_EQ_BANDS - 1);
        let filter_type = match idx {
            0 => FilterType::LowShelf,
            9 => FilterType::HighShelf,
            _ => FilterType::Peaking,
        };
        Self {
            band_idx: idx as u8,
            enabled: true,
            filter_type,
            freq_hz: DEFAULT_BAND_FREQS[idx],
            q: std::f32::consts::FRAC_1_SQRT_2,
            gain_db: 0.0,
        }
    }
}

/// Normalized biquad transfer function coefficients:
///
/// $$H(z) = \frac{b_0 + b_1 z^{-1} + b_2 z^{-2}}{1 + a_1 z^{-1} + a_2 z^{-2}}$$
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[repr(C)]
pub struct BiquadCoeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl Default for BiquadCoeffs {
    fn default() -> Self {
        Self::identity()
    }
}

impl BiquadCoeffs {
    /// Unity pass-through filter (0 dB flat magnitude response across all frequencies).
    #[inline(always)]
    pub const fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }

    /// Computes normalized RBJ Audio EQ Cookbook coefficients using `f64` intermediate
    /// precision to avoid pole/zero quantization noise at low frequencies (< 60 Hz),
    /// then downcasts to `f32` for real-time execution.
    pub fn design(
        filter_type: FilterType,
        freq_hz: f32,
        q: f32,
        gain_db: f32,
        sample_rate: f32,
    ) -> Self {
        if filter_type == FilterType::Bypass || sample_rate <= 0.0 {
            return Self::identity();
        }

        let nyquist = (sample_rate as f64) * 0.495;
        let f0 = (freq_hz as f64).clamp(10.0, nyquist);
        let fs = sample_rate as f64;
        let q_val = (q as f64).clamp(0.1, 24.0);
        let db = (gain_db as f64).clamp(-24.0, 24.0);

        let w0 = 2.0 * std::f64::consts::PI * (f0 / fs);
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q_val);
        let a = 10.0_f64.powf(db / 40.0);

        let (b0, b1, b2, a0, a1, a2) = match filter_type {
            FilterType::Bypass => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),

            FilterType::Peaking => {
                let b0 = 1.0 + alpha * a;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0 - alpha * a;
                let a0 = 1.0 + alpha / a;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha / a;
                (b0, b1, b2, a0, a1, a2)
            }

            FilterType::LowShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
                let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
                let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
                let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
                let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }

            FilterType::HighShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
                let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
                let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
                let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
                let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }

            FilterType::HighPass => {
                let b0 = (1.0 + cos_w0) * 0.5;
                let b1 = -(1.0 + cos_w0);
                let b2 = (1.0 + cos_w0) * 0.5;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }

            FilterType::LowPass => {
                let b0 = (1.0 - cos_w0) * 0.5;
                let b1 = 1.0 - cos_w0;
                let b2 = (1.0 - cos_w0) * 0.5;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
        };

        let inv_a0 = 1.0 / a0;
        Self {
            b0: (b0 * inv_a0) as f32,
            b1: (b1 * inv_a0) as f32,
            b2: (b2 * inv_a0) as f32,
            a1: (a1 * inv_a0) as f32,
            a2: (a2 * inv_a0) as f32,
        }
    }

    /// Evaluates the magnitude response in dB at `freq_hz` for a given `sample_rate`.
    pub fn magnitude_db_at(&self, freq_hz: f32, sample_rate: f32) -> f32 {
        let w = 2.0 * std::f64::consts::PI * (freq_hz as f64) / (sample_rate as f64);
        let cw = w.cos();
        let sw = w.sin();
        let c2w = (2.0 * w).cos();
        let s2w = (2.0 * w).sin();

        let b0 = self.b0 as f64;
        let b1 = self.b1 as f64;
        let b2 = self.b2 as f64;
        let a1 = self.a1 as f64;
        let a2 = self.a2 as f64;

        let num_re = b0 + b1 * cw + b2 * c2w;
        let num_im = -(b1 * sw + b2 * s2w);
        let den_re = 1.0 + a1 * cw + a2 * c2w;
        let den_im = -(a1 * sw + a2 * s2w);

        let mag_sq =
            (num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im).max(1.0e-18);
        (10.0 * mag_sq.max(1.0e-12).log10()) as f32
    }
}

/// Stereo Transposed Direct Form II (TDF-II) Biquad Filter with per-sample
/// linear coefficient ramping for click-free real-time EQ sweeps.
#[derive(Debug, Clone, Copy)]
pub struct StereoBiquad {
    active: BiquadCoeffs,
    target: BiquadCoeffs,
    z1: [f32; 2],
    z2: [f32; 2],
    interp_steps_remaining: u32,
    step_b0: f32,
    step_b1: f32,
    step_b2: f32,
    step_a1: f32,
    step_a2: f32,
    enabled: bool,
}

impl Default for StereoBiquad {
    fn default() -> Self {
        Self::new()
    }
}

impl StereoBiquad {
    pub const fn new() -> Self {
        Self {
            active: BiquadCoeffs::identity(),
            target: BiquadCoeffs::identity(),
            z1: [0.0; 2],
            z2: [0.0; 2],
            interp_steps_remaining: 0,
            step_b0: 0.0,
            step_b1: 0.0,
            step_b2: 0.0,
            step_a1: 0.0,
            step_a2: 0.0,
            enabled: false,
        }
    }

    /// Sets new target coefficients, ramping smoothly over `ramp_samples`.
    #[inline]
    pub fn set_target_coeffs(
        &mut self,
        new_coeffs: BiquadCoeffs,
        enabled: bool,
        ramp_samples: u32,
    ) {
        self.enabled = enabled;
        let target = if enabled {
            new_coeffs
        } else {
            BiquadCoeffs::identity()
        };
        self.target = target;

        if ramp_samples == 0 {
            self.active = target;
            self.interp_steps_remaining = 0;
        } else {
            let inv_steps = 1.0 / (ramp_samples as f32);
            self.step_b0 = (target.b0 - self.active.b0) * inv_steps;
            self.step_b1 = (target.b1 - self.active.b1) * inv_steps;
            self.step_b2 = (target.b2 - self.active.b2) * inv_steps;
            self.step_a1 = (target.a1 - self.active.a1) * inv_steps;
            self.step_a2 = (target.a2 - self.active.a2) * inv_steps;
            self.interp_steps_remaining = ramp_samples;
        }
    }

    /// Resets internal filter delay lines to zero.
    #[inline]
    pub fn reset(&mut self) {
        self.z1 = [0.0; 2];
        self.z2 = [0.0; 2];
    }

    /// Processes a single stereo frame `[left, right]` in-place using TDF-II.
    #[inline(always)]
    pub fn process_stereo_frame(&mut self, left: &mut f32, right: &mut f32) {
        if !self.enabled && self.interp_steps_remaining == 0 {
            return;
        }

        if self.interp_steps_remaining > 0 {
            self.active.b0 += self.step_b0;
            self.active.b1 += self.step_b1;
            self.active.b2 += self.step_b2;
            self.active.a1 += self.step_a1;
            self.active.a2 += self.step_a2;
            self.interp_steps_remaining -= 1;
            if self.interp_steps_remaining == 0 {
                self.active = self.target;
            }
        }

        let c = self.active;

        // Left channel (TDF-II)
        let xl = *left;
        let yl = c.b0 * xl + self.z1[0];
        self.z1[0] = c.b1 * xl - c.a1 * yl + self.z2[0];
        self.z2[0] = c.b2 * xl - c.a2 * yl;
        *left = yl;

        // Right channel (TDF-II)
        let xr = *right;
        let yr = c.b0 * xr + self.z1[1];
        self.z1[1] = c.b1 * xr - c.a1 * yr + self.z2[1];
        self.z2[1] = c.b2 * xr - c.a2 * yr;
        *right = yr;
    }

    #[inline(always)]
    pub fn active_coeffs(&self) -> BiquadCoeffs {
        self.active
    }
}

/// 10-Band Stereo Parametric Equalizer.
#[derive(Debug, Clone)]
pub struct ParametricEq10 {
    pub bands: [StereoBiquad; NUM_EQ_BANDS],
    coeff_ramp_samples: u32,
}

impl ParametricEq10 {
    pub fn new(sample_rate: f32) -> Self {
        let coeff_ramp_samples = ((sample_rate * 0.003) as u32).max(32);
        Self {
            bands: [StereoBiquad::new(); NUM_EQ_BANDS],
            coeff_ramp_samples,
        }
    }

    #[inline]
    pub fn update_band(&mut self, band_idx: usize, coeffs: BiquadCoeffs, enabled: bool) {
        if let Some(band) = self.bands.get_mut(band_idx) {
            band.set_target_coeffs(coeffs, enabled, self.coeff_ramp_samples);
        }
    }

    #[inline]
    pub fn update_band_immediate(&mut self, band_idx: usize, coeffs: BiquadCoeffs, enabled: bool) {
        if let Some(band) = self.bands.get_mut(band_idx) {
            band.set_target_coeffs(coeffs, enabled, 0);
        }
    }

    #[inline(always)]
    pub fn process_stereo_frame(&mut self, left: &mut f32, right: &mut f32) {
        for band in &mut self.bands {
            band.process_stereo_frame(left, right);
        }
    }

    pub fn reset(&mut self) {
        for band in &mut self.bands {
            band.reset();
        }
    }
}
