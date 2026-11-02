//! Real-Time Channel Strip Processor with lock-free `rtrb` command ingestion,
//! exponential parameter smoothing, 10-band Parametric EQ, and decimated Peak/RMS metering.

use crate::biquad::{BiquadCoeffs, ParametricEq10};
use crate::denormals::ScopedNoDenormals;
use crate::smoother::SmoothedParam;
use rtrb::{Consumer, Producer};
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

const EPSILON: f32 = 1.0e-12;

/// Logical mixer channel identifiers in AAAudio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum ChannelId {
    Master = 0,
    Game = 1,
    Chat = 2,
    Media = 3,
    Aux = 4,
    Mic = 5,
}

impl ChannelId {
    pub const ALL: [ChannelId; 6] = [
        ChannelId::Master,
        ChannelId::Game,
        ChannelId::Chat,
        ChannelId::Media,
        ChannelId::Aux,
        ChannelId::Mic,
    ];

    pub const fn label(&self) -> &'static str {
        match self {
            ChannelId::Master => "Master",
            ChannelId::Game => "Game",
            ChannelId::Chat => "Chat / Comms",
            ChannelId::Media => "Media",
            ChannelId::Aux => "Aux",
            ChannelId::Mic => "Microphone",
        }
    }
}

/// Lock-free command packet sent from the Host Control Thread to the Real-Time Audio Thread.
/// Guaranteed `Copy` and free of heap allocations.
#[derive(Debug, Clone, Copy)]
pub enum DspCommand {
    /// Set channel fader linear gain (`10^(dB / 20)`).
    SetFaderGain(f32),
    /// Set mute state (`true` = smooth 4ms ramp to `0.0`).
    SetMute(bool),
    /// Set stereo constant-power pan in `[-1.0, 1.0]`.
    SetPan(f32),
    /// Update a specific EQ band (`0..10`) with pre-computed biquad coefficients.
    UpdateEqBand {
        band_idx: u8,
        enabled: bool,
        coeffs: BiquadCoeffs,
    },
    /// Toggle global EQ bypass for this channel strip.
    SetEqBypass(bool),
}

/// Real-time VU meter telemetry frame pushed from the RT thread to the Host Control Thread.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[repr(C)]
pub struct MeterFrame {
    pub peak_l: f32,
    pub peak_r: f32,
    pub rms_l: f32,
    pub rms_r: f32,
    pub gain_reduction_db: f32,
    pub gate_open: f32,
}

/// Complete real-time Channel Strip DSP processor.
pub struct ChannelStripProcessor {
    cmd_consumer: Consumer<DspCommand>,
    meter_producer: Producer<MeterFrame>,

    fader_gain: SmoothedParam,
    mute_gain: SmoothedParam,
    pan_l: SmoothedParam,
    pan_r: SmoothedParam,

    target_unmuted_fader: f32,
    is_muted: bool,
    eq_bypass: bool,
    eq: ParametricEq10,

    accum_sq_l: f32,
    accum_sq_r: f32,
    accum_peak_l: f32,
    accum_peak_r: f32,
    accum_frames: u32,
    meter_emit_threshold_frames: u32,
    last_meter: MeterFrame,
}

impl ChannelStripProcessor {
    pub fn new(
        sample_rate: f32,
        cmd_consumer: Consumer<DspCommand>,
        meter_producer: Producer<MeterFrame>,
    ) -> Self {
        let meter_emit_threshold_frames = ((sample_rate / 60.0) as u32).max(64);

        Self {
            cmd_consumer,
            meter_producer,
            fader_gain: SmoothedParam::new(1.0, 10.0, sample_rate),
            mute_gain: SmoothedParam::new(1.0, 4.0, sample_rate),
            pan_l: SmoothedParam::new(std::f32::consts::FRAC_1_SQRT_2, 10.0, sample_rate),
            pan_r: SmoothedParam::new(std::f32::consts::FRAC_1_SQRT_2, 10.0, sample_rate),
            target_unmuted_fader: 1.0,
            is_muted: false,
            eq_bypass: false,
            eq: ParametricEq10::new(sample_rate),
            accum_sq_l: 0.0,
            accum_sq_r: 0.0,
            accum_peak_l: 0.0,
            accum_peak_r: 0.0,
            accum_frames: 0,
            meter_emit_threshold_frames,
            last_meter: MeterFrame::default(),
        }
    }

    /// Drains all pending commands from the lock-free SPSC queue without blocking.
    #[inline]
    pub fn drain_commands(&mut self) {
        while let Ok(cmd) = self.cmd_consumer.pop() {
            match cmd {
                DspCommand::SetFaderGain(linear_gain) => {
                    self.target_unmuted_fader = linear_gain.clamp(0.0, 4.0);
                    self.fader_gain.set_target(self.target_unmuted_fader);
                }
                DspCommand::SetMute(muted) => {
                    self.is_muted = muted;
                    self.mute_gain.set_target(if muted { 0.0 } else { 1.0 });
                }
                DspCommand::SetPan(pan) => {
                    let p = pan.clamp(-1.0, 1.0);
                    let angle = (p + 1.0) * (PI * 0.25);
                    self.pan_l.set_target(angle.cos());
                    self.pan_r.set_target(angle.sin());
                }
                DspCommand::UpdateEqBand {
                    band_idx,
                    enabled,
                    coeffs,
                } => {
                    self.eq.update_band(band_idx as usize, coeffs, enabled);
                }
                DspCommand::SetEqBypass(bypass) => {
                    self.eq_bypass = bypass;
                }
            }
        }
    }

    /// Processes an interleaved stereo buffer (`[L, R, L, R, ...]`) in-place.
    /// Strictly zero heap allocations and zero locks.
    pub fn process_interleaved(&mut self, buffer: &mut [f32]) {
        let _no_denormals = ScopedNoDenormals::new();
        self.drain_commands();

        let num_frames = buffer.len() / 2;
        if num_frames == 0 {
            return;
        }

        let mut local_sq_l = 0.0_f32;
        let mut local_sq_r = 0.0_f32;
        let mut local_peak_l = 0.0_f32;
        let mut local_peak_r = 0.0_f32;

        const PAN_UNITY_COMPENSATION: f32 = std::f32::consts::SQRT_2;

        for frame in buffer.chunks_exact_mut(2) {
            let mut left = frame[0];
            let mut right = frame[1];

            if !self.eq_bypass {
                self.eq.process_stereo_frame(&mut left, &mut right);
            }

            let gain = self.fader_gain.next_sample() * self.mute_gain.next_sample();
            let gl = gain * self.pan_l.next_sample() * PAN_UNITY_COMPENSATION;
            let gr = gain * self.pan_r.next_sample() * PAN_UNITY_COMPENSATION;

            left *= gl;
            right *= gr;

            frame[0] = left;
            frame[1] = right;

            let abs_l = left.abs();
            let abs_r = right.abs();
            if abs_l > local_peak_l {
                local_peak_l = abs_l;
            }
            if abs_r > local_peak_r {
                local_peak_r = abs_r;
            }
            local_sq_l += left * left;
            local_sq_r += right * right;
        }

        self.accum_sq_l += local_sq_l;
        self.accum_sq_r += local_sq_r;
        self.accum_peak_l = self.accum_peak_l.max(local_peak_l);
        self.accum_peak_r = self.accum_peak_r.max(local_peak_r);
        self.accum_frames += num_frames as u32;

        if self.accum_frames >= self.meter_emit_threshold_frames {
            let inv_n = 1.0 / (self.accum_frames as f32);
            let rms_l = (self.accum_sq_l * inv_n).sqrt();
            let rms_r = (self.accum_sq_r * inv_n).sqrt();

            let meter = MeterFrame {
                peak_l: self.accum_peak_l,
                peak_r: self.accum_peak_r,
                rms_l,
                rms_r,
                gain_reduction_db: 0.0,
                gate_open: 1.0,
            };
            self.last_meter = meter;
            let _ = self.meter_producer.push(meter);

            self.accum_sq_l = 0.0;
            self.accum_sq_r = 0.0;
            self.accum_peak_l = 0.0;
            self.accum_peak_r = 0.0;
            self.accum_frames = 0;
        }
    }

    #[inline]
    pub fn last_meter(&self) -> MeterFrame {
        self.last_meter
    }

    #[inline]
    pub fn linear_to_dbfs(linear: f32) -> f32 {
        20.0 * (linear.max(EPSILON)).log10()
    }

    #[inline]
    pub fn dbfs_to_linear(db: f32) -> f32 {
        if db <= -96.0 {
            0.0
        } else {
            10.0_f32.powf(db * 0.05)
        }
    }
}
