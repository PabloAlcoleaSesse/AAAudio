//! Microphone Voice Conditioning DSP Chain:
//! - 80 Hz Butterworth High-Pass Rumble Filter
//! - Hysteresis Noise Gate (Threshold, Hysteresis, Attack, Hold, Release)
//! - Soft-Knee Feed-Forward Compressor (Threshold, Ratio, Attack, Release, Makeup Gain)
//! - Zero-Feedback Sidetone Loopback Monitor

use crate::biquad::{BiquadCoeffs, FilterType, ParametricEq10, StereoBiquad};
use crate::denormals::ScopedNoDenormals;
use crate::smoother::SmoothedParam;
use crate::strip::MeterFrame;
use rtrb::{Consumer, Producer};
use serde::{Deserialize, Serialize};

const DB_EPSILON: f32 = 1.0e-10;

#[inline(always)]
fn lin_to_db(lin: f32) -> f32 {
    20.0 * lin.max(DB_EPSILON).log10()
}

#[inline(always)]
fn db_to_lin(db: f32) -> f32 {
    10.0_f32.powf(db * 0.05)
}

#[inline(always)]
fn time_ms_to_coeff(time_ms: f32, sample_rate: f32) -> f32 {
    if time_ms <= 0.0 || sample_rate <= 0.0 {
        0.0
    } else {
        (-1.0 / ((time_ms * 0.001) * sample_rate)).exp()
    }
}

/// Serializable Noise Gate configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NoiseGateParams {
    pub enabled: bool,
    pub threshold_db: f32,
    pub hysteresis_db: f32,
    pub attack_ms: f32,
    pub hold_ms: f32,
    pub release_ms: f32,
}

impl Default for NoiseGateParams {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_db: -42.0,
            hysteresis_db: 4.0,
            attack_ms: 2.0,
            hold_ms: 80.0,
            release_ms: 150.0,
        }
    }
}

/// Serializable Voice Compressor configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CompressorParams {
    pub enabled: bool,
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
    pub knee_width_db: f32,
}

impl Default for CompressorParams {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_db: -18.0,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 80.0,
            makeup_db: 3.0,
            knee_width_db: 6.0,
        }
    }
}

/// Lock-free commands for the Microphone Voice Chain.
#[derive(Debug, Clone, Copy)]
pub enum VoiceCommand {
    SetOutputGain(f32),
    SetMute(bool),
    SetSidetone {
        enabled: bool,
        linear_gain: f32,
    },
    SetGateParams(NoiseGateParams),
    SetCompressorParams(CompressorParams),
    UpdateEqBand {
        band_idx: u8,
        enabled: bool,
        coeffs: BiquadCoeffs,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateState {
    Closed,
    Attack,
    Hold,
    Release,
}

/// Hysteresis Noise Gate with Attack, Hold, and Release ballistics.
#[derive(Debug, Clone)]
pub struct NoiseGate {
    open_threshold_lin: f32,
    close_threshold_lin: f32,
    floor_gain_lin: f32,
    attack_coeff: f32,
    release_coeff: f32,
    hold_samples: u32,
    hold_counter: u32,
    det_env: f32,
    gain_env: f32,
    state: GateState,
    sample_rate: f32,
    pub enabled: bool,
}

impl NoiseGate {
    pub fn new(sample_rate: f32) -> Self {
        let mut gate = Self {
            open_threshold_lin: 0.0,
            close_threshold_lin: 0.0,
            floor_gain_lin: db_to_lin(-70.0),
            attack_coeff: 0.0,
            release_coeff: 0.0,
            hold_samples: 0,
            hold_counter: 0,
            det_env: 0.0,
            gain_env: 0.0,
            state: GateState::Closed,
            sample_rate,
            enabled: true,
        };
        gate.apply_params(NoiseGateParams::default());
        gate
    }

    pub fn apply_params(&mut self, p: NoiseGateParams) {
        self.enabled = p.enabled;
        self.open_threshold_lin = db_to_lin(p.threshold_db);
        self.close_threshold_lin = db_to_lin(p.threshold_db - p.hysteresis_db.max(1.0));
        self.attack_coeff = time_ms_to_coeff(p.attack_ms, self.sample_rate);
        self.release_coeff = time_ms_to_coeff(p.release_ms, self.sample_rate);
        self.hold_samples = ((p.hold_ms * 0.001) * self.sample_rate) as u32;
    }

    #[inline(always)]
    pub fn process_sample(&mut self, input: f32) -> (f32, f32) {
        if !self.enabled {
            return (input, 1.0);
        }

        let abs_in = input.abs();
        let det_c = if abs_in > self.det_env { 0.979 } else { 0.999 };
        self.det_env = det_c * self.det_env + (1.0 - det_c) * abs_in;

        match self.state {
            GateState::Closed | GateState::Release => {
                if self.det_env >= self.open_threshold_lin {
                    self.state = GateState::Attack;
                    self.hold_counter = self.hold_samples;
                }
            }
            GateState::Attack | GateState::Hold => {
                if self.det_env >= self.close_threshold_lin {
                    self.hold_counter = self.hold_samples;
                } else if self.hold_counter > 0 {
                    self.state = GateState::Hold;
                    self.hold_counter -= 1;
                } else {
                    self.state = GateState::Release;
                }
            }
        }

        match self.state {
            GateState::Attack | GateState::Hold => {
                self.gain_env = self.attack_coeff * self.gain_env + (1.0 - self.attack_coeff) * 1.0;
                if self.gain_env > 0.999 {
                    self.gain_env = 1.0;
                    if self.state == GateState::Attack {
                        self.state = GateState::Hold;
                    }
                }
            }
            GateState::Release | GateState::Closed => {
                self.gain_env = self.release_coeff * self.gain_env
                    + (1.0 - self.release_coeff) * self.floor_gain_lin;
                if self.gain_env <= self.floor_gain_lin + 1.0e-5 {
                    self.gain_env = self.floor_gain_lin;
                    self.state = GateState::Closed;
                }
            }
        }

        (input * self.gain_env, self.gain_env)
    }
}

/// Feed-Forward Soft-Knee Voice Compressor.
#[derive(Debug, Clone)]
pub struct Compressor {
    threshold_db: f32,
    ratio: f32,
    knee_width_db: f32,
    attack_coeff: f32,
    release_coeff: f32,
    makeup_gain_lin: f32,
    env_db: f32,
    sample_rate: f32,
    pub enabled: bool,
}

impl Compressor {
    pub fn new(sample_rate: f32) -> Self {
        let mut comp = Self {
            threshold_db: -18.0,
            ratio: 4.0,
            knee_width_db: 6.0,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            makeup_gain_lin: 1.0,
            env_db: 0.0,
            sample_rate,
            enabled: true,
        };
        comp.apply_params(CompressorParams::default());
        comp
    }

    pub fn apply_params(&mut self, p: CompressorParams) {
        self.enabled = p.enabled;
        self.threshold_db = p.threshold_db.clamp(-60.0, 0.0);
        self.ratio = p.ratio.clamp(1.0, 20.0);
        self.knee_width_db = p.knee_width_db.clamp(0.0, 24.0);
        self.attack_coeff = time_ms_to_coeff(p.attack_ms, self.sample_rate);
        self.release_coeff = time_ms_to_coeff(p.release_ms, self.sample_rate);
        self.makeup_gain_lin = db_to_lin(p.makeup_db.clamp(-12.0, 24.0));
    }

    #[inline(always)]
    fn compute_gain_reduction_db(&self, input_db: f32) -> f32 {
        let overshoot = input_db - self.threshold_db;
        let half_knee = self.knee_width_db * 0.5;
        let slope = 1.0 / self.ratio - 1.0;

        if overshoot <= -half_knee {
            0.0
        } else if overshoot >= half_knee && self.knee_width_db > 0.0 {
            overshoot * slope
        } else if self.knee_width_db > 0.0 {
            let x = overshoot + half_knee;
            slope * (x * x) / (2.0 * self.knee_width_db)
        } else {
            overshoot * slope
        }
    }

    #[inline(always)]
    pub fn process_sample(&mut self, input: f32) -> (f32, f32) {
        if !self.enabled {
            return (input, 0.0);
        }

        let in_db = lin_to_db(input.abs());
        let target_gr_db = self.compute_gain_reduction_db(in_db);

        let coeff = if target_gr_db < self.env_db {
            self.attack_coeff
        } else {
            self.release_coeff
        };

        self.env_db = coeff * self.env_db + (1.0 - coeff) * target_gr_db;
        let total_gain_lin = db_to_lin(self.env_db) * self.makeup_gain_lin;

        (input * total_gain_lin, self.env_db)
    }
}

/// Complete Microphone Voice Chain Processor with zero-feedback Sidetone monitoring.
pub struct VoiceChainProcessor {
    cmd_consumer: Consumer<VoiceCommand>,
    meter_producer: Producer<MeterFrame>,

    hpf: StereoBiquad,
    pub gate: NoiseGate,
    pub compressor: Compressor,
    pub eq: ParametricEq10,

    mic_output_gain: SmoothedParam,
    mute_gain: SmoothedParam,
    sidetone_gain: SmoothedParam,

    accum_sq: f32,
    accum_peak: f32,
    min_gr_db: f32,
    last_gate_env: f32,
    accum_frames: u32,
    meter_emit_threshold_frames: u32,
    last_meter: MeterFrame,
}

impl VoiceChainProcessor {
    pub fn new(
        sample_rate: f32,
        cmd_consumer: Consumer<VoiceCommand>,
        meter_producer: Producer<MeterFrame>,
    ) -> Self {
        let mut hpf = StereoBiquad::new();
        let hpf_coeffs = BiquadCoeffs::design(
            FilterType::HighPass,
            80.0,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            sample_rate,
        );
        hpf.set_target_coeffs(hpf_coeffs, true, 0);

        let meter_emit_threshold_frames = ((sample_rate / 60.0) as u32).max(64);

        Self {
            cmd_consumer,
            meter_producer,
            hpf,
            gate: NoiseGate::new(sample_rate),
            compressor: Compressor::new(sample_rate),
            eq: ParametricEq10::new(sample_rate),
            mic_output_gain: SmoothedParam::new(1.0, 10.0, sample_rate),
            mute_gain: SmoothedParam::new(1.0, 4.0, sample_rate),
            sidetone_gain: SmoothedParam::new(0.0, 5.0, sample_rate),
            accum_sq: 0.0,
            accum_peak: 0.0,
            min_gr_db: 0.0,
            last_gate_env: 0.0,
            accum_frames: 0,
            meter_emit_threshold_frames,
            last_meter: MeterFrame::default(),
        }
    }

    #[inline]
    pub fn drain_commands(&mut self) {
        while let Ok(cmd) = self.cmd_consumer.pop() {
            match cmd {
                VoiceCommand::SetOutputGain(gain) => {
                    self.mic_output_gain.set_target(gain.clamp(0.0, 4.0));
                }
                VoiceCommand::SetMute(muted) => {
                    self.mute_gain.set_target(if muted { 0.0 } else { 1.0 });
                }
                VoiceCommand::SetSidetone {
                    enabled,
                    linear_gain,
                } => {
                    self.sidetone_gain.set_target(if enabled {
                        linear_gain.clamp(0.0, 2.0)
                    } else {
                        0.0
                    });
                }
                VoiceCommand::SetGateParams(p) => {
                    self.gate.apply_params(p);
                }
                VoiceCommand::SetCompressorParams(p) => {
                    self.compressor.apply_params(p);
                }
                VoiceCommand::UpdateEqBand {
                    band_idx,
                    enabled,
                    coeffs,
                } => {
                    self.eq.update_band(band_idx as usize, coeffs, enabled);
                }
            }
        }
    }

    /// Processes mono microphone capture in-place, writes conditioned stereo voice to
    /// `virtual_mic_out_interleaved`, and injects zero-feedback sidetone into
    /// `hardware_master_out_interleaved`.
    pub fn process_block(
        &mut self,
        mic_mono_in: &[f32],
        virtual_mic_out_interleaved: &mut [f32],
        hardware_master_out_interleaved: &mut [f32],
    ) {
        let _no_denormals = ScopedNoDenormals::new();
        self.drain_commands();

        let frames = mic_mono_in
            .len()
            .min(virtual_mic_out_interleaved.len() / 2)
            .min(hardware_master_out_interleaved.len() / 2);

        if frames == 0 {
            return;
        }

        for (i, &sample_in) in mic_mono_in.iter().enumerate().take(frames) {
            let mut s = sample_in;
            let mut dummy_r = 0.0_f32;

            // 1. 80 Hz Butterworth High-Pass Filter
            self.hpf.process_stereo_frame(&mut s, &mut dummy_r);

            // 2. Hysteresis Noise Gate
            let (gated, gate_env) = self.gate.process_sample(s);
            self.last_gate_env = gate_env;

            // 3. Soft-Knee Voice Compressor
            let (mut compressed, gr_db) = self.compressor.process_sample(gated);
            if gr_db < self.min_gr_db {
                self.min_gr_db = gr_db;
            }

            // 4. 10-Band Voice Parametric EQ
            let mut right_eq = compressed;
            self.eq.process_stereo_frame(&mut compressed, &mut right_eq);

            // 5. Output Fader + Smooth Mute
            let out_voice =
                compressed * self.mic_output_gain.next_sample() * self.mute_gain.next_sample();
            let idx = i * 2;
            virtual_mic_out_interleaved[idx] = out_voice;
            virtual_mic_out_interleaved[idx + 1] = out_voice;

            // 6. Zero-Feedback Sidetone injection ONLY into physical Hardware Master Output
            let st_gain = self.sidetone_gain.next_sample();
            if st_gain > 1.0e-6 {
                let sidetone_sample = out_voice * st_gain;
                hardware_master_out_interleaved[idx] += sidetone_sample;
                hardware_master_out_interleaved[idx + 1] += sidetone_sample;
            }

            let abs_v = out_voice.abs();
            if abs_v > self.accum_peak {
                self.accum_peak = abs_v;
            }
            self.accum_sq += out_voice * out_voice;
        }

        self.accum_frames += frames as u32;
        if self.accum_frames >= self.meter_emit_threshold_frames {
            let rms = (self.accum_sq / (self.accum_frames as f32)).sqrt();
            let meter = MeterFrame {
                peak_l: self.accum_peak,
                peak_r: self.accum_peak,
                rms_l: rms,
                rms_r: rms,
                gain_reduction_db: self.min_gr_db,
                gate_open: self.last_gate_env,
            };
            self.last_meter = meter;
            let _ = self.meter_producer.push(meter);

            self.accum_sq = 0.0;
            self.accum_peak = 0.0;
            self.min_gr_db = 0.0;
            self.accum_frames = 0;
        }
    }

    #[inline]
    pub fn last_meter(&self) -> MeterFrame {
        self.last_meter
    }
}
