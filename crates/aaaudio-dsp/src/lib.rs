//! # `aaaudio-dsp`
//!
//! Real-time, zero-allocation audio DSP engine for **AAAudio**.
//!
//! ## Core Guarantees
//! - **Zero Heap Allocations** on the audio callback thread.
//! - **Zero Locks** (`Mutex`, `RwLock`, or syscalls) via `rtrb` lock-free SPSC ring buffers.
//! - **Hardware Denormal Suppression** (`FTZ` / `DAZ` via [`denormals::ScopedNoDenormals`]).
//! - **Click-Free Parameter Transitions** via [`smoother::SmoothedParam`] and per-sample TDF-II
//!   biquad coefficient interpolation.

pub mod biquad;
pub mod denormals;
pub mod mixer;
pub mod smoother;
pub mod strip;
pub mod voice;

pub use biquad::{
    BiquadCoeffs, EqBandParams, FilterType, ParametricEq10, StereoBiquad, DEFAULT_BAND_FREQS,
    NUM_EQ_BANDS,
};
pub use denormals::ScopedNoDenormals;
pub use mixer::{compute_chatmix_gains, AudioMixerMatrix};
pub use smoother::SmoothedParam;
pub use strip::{ChannelId, ChannelStripProcessor, DspCommand, MeterFrame};
pub use voice::{
    Compressor, CompressorParams, NoiseGate, NoiseGateParams, VoiceChainProcessor, VoiceCommand,
};

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;
    use std::f32::consts::PI;

    const SAMPLE_RATE: f32 = 48_000.0;

    #[test]
    fn test_smoothed_param_convergence() {
        let mut smoother = SmoothedParam::new(0.0, 5.0, SAMPLE_RATE);
        smoother.set_target(1.0);
        assert!(smoother.is_smoothing());

        // Advance 50ms (10 time constants -> well within 1e-6 snap threshold)
        let mut last = 0.0;
        for _ in 0..(SAMPLE_RATE as usize / 20) {
            let v = smoother.next_sample();
            assert!(v >= last);
            last = v;
        }
        assert!((smoother.current() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn test_biquad_peaking_and_filtering_response() {
        // +12 dB Peaking filter at 1 kHz
        let peak_coeffs = BiquadCoeffs::design(FilterType::Peaking, 1000.0, 1.0, 12.0, SAMPLE_RATE);
        let db_at_1k = peak_coeffs.magnitude_db_at(1000.0, SAMPLE_RATE);
        let db_at_50hz = peak_coeffs.magnitude_db_at(50.0, SAMPLE_RATE);
        assert!(
            (db_at_1k - 12.0).abs() < 0.05,
            "Expected +12 dB at center freq, got {db_at_1k}"
        );
        assert!(
            db_at_50hz.abs() < 0.1,
            "Expected ~0 dB far from center freq, got {db_at_50hz}"
        );

        // High-pass filter at 100 Hz should strongly attenuate 20 Hz while passing 5 kHz
        let hpf = BiquadCoeffs::design(
            FilterType::HighPass,
            100.0,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            SAMPLE_RATE,
        );
        assert!(hpf.magnitude_db_at(20.0, SAMPLE_RATE) < -24.0);
        assert!(hpf.magnitude_db_at(5000.0, SAMPLE_RATE).abs() < 0.05);
    }

    #[test]
    fn test_channel_strip_lock_free_commands_and_metering() {
        let (mut cmd_prod, cmd_cons) = RingBuffer::new(64);
        let (meter_prod, mut meter_cons) = RingBuffer::new(64);

        let mut strip = ChannelStripProcessor::new(SAMPLE_RATE, cmd_cons, meter_prod);

        // Push unity gain and unmute
        cmd_prod.push(DspCommand::SetFaderGain(1.0)).unwrap();
        cmd_prod.push(DspCommand::SetMute(false)).unwrap();

        // Process 1024 stereo frames of a 1 kHz full-scale sine wave
        let frames = 1024;
        let mut buf = vec![0.0_f32; frames * 2];
        for i in 0..frames {
            let s = (2.0 * PI * 1000.0 * (i as f32) / SAMPLE_RATE).sin();
            buf[i * 2] = s;
            buf[i * 2 + 1] = s;
        }

        strip.process_interleaved(&mut buf);

        let meter = meter_cons
            .pop()
            .expect("Expected meter frame after 1024 frames");
        assert!(
            (meter.peak_l - 1.0).abs() < 0.02,
            "Peak L should be ~1.0, got {}",
            meter.peak_l
        );
        assert!(
            (meter.rms_l - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02,
            "RMS L of sine wave should be ~0.7071, got {}",
            meter.rms_l
        );
    }

    #[test]
    fn test_voice_chain_and_sidetone_isolation() {
        let (mut cmd_prod, cmd_cons) = RingBuffer::new(64);
        let (meter_prod, _meter_cons) = RingBuffer::new(64);

        let mut voice = VoiceChainProcessor::new(SAMPLE_RATE, cmd_cons, meter_prod);
        cmd_prod
            .push(VoiceCommand::SetSidetone {
                enabled: true,
                linear_gain: 0.5,
            })
            .unwrap();

        let frames = 1024;
        let mic_in: Vec<f32> = (0..frames)
            .map(|i| 0.5 * (2.0 * PI * 400.0 * (i as f32) / SAMPLE_RATE).sin())
            .collect();
        let mut virtual_mic_out = vec![0.0_f32; frames * 2];
        let mut hw_master_out = vec![0.0_f32; frames * 2];

        voice.process_block(&mic_in, &mut virtual_mic_out, &mut hw_master_out);

        // Verify virtual mic received conditioned voice AND hardware master received sidetone
        let vmic_energy: f32 = virtual_mic_out.iter().map(|x| x * x).sum();
        let hw_energy: f32 = hw_master_out.iter().map(|x| x * x).sum();
        assert!(vmic_energy > 1.0);
        assert!(hw_energy > 0.1);
    }

    #[test]
    fn test_chatmix_law() {
        let (g0, c0) = compute_chatmix_gains(0.0);
        assert_eq!((g0, c0), (1.0, 1.0));

        let (g_game, c_game) = compute_chatmix_gains(-1.0);
        assert_eq!(g_game, 1.0);
        assert_eq!(c_game, 0.0);

        let (g_chat, c_chat) = compute_chatmix_gains(1.0);
        assert_eq!(g_chat, 0.0);
        assert_eq!(c_chat, 1.0);
    }
}
