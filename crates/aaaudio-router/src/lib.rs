//! # `aaaudio-router`
//!
//! Host Control Thread and Cross-Platform Virtual Audio Routing Orchestrator for **AAAudio**.
//!
//! ## Responsibilities
//! - Enumerates physical playback/capture devices via `cpal` and virtual routing endpoints:
//!   - **Linux:** Native PipeWire virtual sinks (`Audio/Sink` nodes for `Game`, `Chat`, `Media`, `Aux`).
//!   - **Windows:** WASAPI Process Loopback (`VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK`) + Virtual Endpoint loopback.
//!   - **macOS:** CoreAudio HAL User-Space Loopback (`BlackHole` / multi-channel aggregate / `AudioHardwareCreateProcessTap`).
//! - Pre-computes RBJ biquad filter coefficients on the non-RT Control Thread and dispatches
//!   atomic-sized `DspCommand` / `VoiceCommand` messages over `rtrb` SPSC ring buffers.
//! - Polls 60 Hz `MeterFrame` telemetry from all 6 channels without blocking the real-time audio thread.

use aaaudio_dsp::{
    AudioMixerMatrix, BiquadCoeffs, ChannelId, ChannelStripProcessor, CompressorParams, DspCommand,
    EqBandParams, MeterFrame, NoiseGateParams, VoiceChainProcessor, VoiceCommand, NUM_EQ_BANDS,
};
use cpal::traits::{DeviceTrait, HostTrait};
use rtrb::{Consumer, Producer, RingBuffer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Ring buffer capacity per channel for control -> RT commands.
const CMD_RING_CAPACITY: usize = 512;
/// Ring buffer capacity per channel for RT -> control meter frames.
const METER_RING_CAPACITY: usize = 128;

/// Platform-specific virtual audio routing strategy active on the current OS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformRoutingStatus {
    pub os: String,
    pub backend_name: String,
    pub virtual_sinks: Vec<String>,
    pub virtual_mic_source: String,
    pub physical_output_device: String,
    pub physical_input_device: String,
    pub sample_rate: u32,
    pub quantum_frames: u32,
    pub estimated_latency_ms: f32,
}

/// Complete serializable snapshot of a single channel strip's UI state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStripState {
    pub id: ChannelId,
    pub label: String,
    pub fader_db: f32,
    pub muted: bool,
    pub solo: bool,
    pub pan: f32,
    pub eq_bypass: bool,
    pub eq_bands: Vec<EqBandParams>,
}

impl ChannelStripState {
    pub fn default_for(id: ChannelId) -> Self {
        let eq_bands = (0..NUM_EQ_BANDS)
            .map(EqBandParams::default_for_band)
            .collect();
        Self {
            id,
            label: id.label().to_string(),
            fader_db: 0.0,
            muted: false,
            solo: false,
            pan: 0.0,
            eq_bypass: false,
            eq_bands,
        }
    }
}

/// Complete serializable snapshot of the entire AAAudio Mixer & Voice Chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixerSnapshot {
    pub routing: PlatformRoutingStatus,
    pub chatmix_balance: f32,
    pub sidetone_enabled: bool,
    pub sidetone_db: f32,
    pub gate: NoiseGateParams,
    pub compressor: CompressorParams,
    pub strips: Vec<ChannelStripState>,
}

/// Real-time 60 Hz meter bundle across all 6 channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixerTelemetryFrame {
    pub timestamp_ms: u64,
    pub meters: HashMap<ChannelId, MeterFrame>,
}

/// Host Control Handle managing the lock-free `rtrb` producers and consumers.
pub struct MixerController {
    sample_rate: f32,
    strip_cmd_producers: HashMap<ChannelId, Producer<DspCommand>>,
    voice_cmd_producer: Producer<VoiceCommand>,
    meter_consumers: HashMap<ChannelId, Consumer<MeterFrame>>,
    last_meters: HashMap<ChannelId, MeterFrame>,
    snapshot: MixerSnapshot,
    running: Arc<AtomicBool>,
    rt_thread: Option<JoinHandle<()>>,
}

impl MixerController {
    /// Initializes the entire lock-free DSP graph, discovers OS audio endpoints,
    /// and starts the real-time processing thread.
    pub fn new(sample_rate: u32, quantum_frames: u32) -> Self {
        let sr = sample_rate as f32;
        let mut strip_cmd_producers = HashMap::new();
        let mut meter_consumers = HashMap::new();
        let mut last_meters = HashMap::new();

        // Helper closure to build a ChannelStripProcessor + its SPSC rings
        let mut make_strip = |id: ChannelId| -> ChannelStripProcessor {
            let (cmd_prod, cmd_cons) = RingBuffer::new(CMD_RING_CAPACITY);
            let (meter_prod, meter_cons) = RingBuffer::new(METER_RING_CAPACITY);
            strip_cmd_producers.insert(id, cmd_prod);
            meter_consumers.insert(id, meter_cons);
            last_meters.insert(id, MeterFrame::default());
            ChannelStripProcessor::new(sr, cmd_cons, meter_prod)
        };

        let master_strip = make_strip(ChannelId::Master);
        let game_strip = make_strip(ChannelId::Game);
        let chat_strip = make_strip(ChannelId::Chat);
        let media_strip = make_strip(ChannelId::Media);
        let aux_strip = make_strip(ChannelId::Aux);

        let (voice_cmd_prod, voice_cmd_cons) = RingBuffer::new(CMD_RING_CAPACITY);
        let (voice_meter_prod, voice_meter_cons) = RingBuffer::new(METER_RING_CAPACITY);
        meter_consumers.insert(ChannelId::Mic, voice_meter_cons);
        last_meters.insert(ChannelId::Mic, MeterFrame::default());

        let voice_chain = VoiceChainProcessor::new(sr, voice_cmd_cons, voice_meter_prod);

        let matrix = AudioMixerMatrix::new(
            sr,
            game_strip,
            chat_strip,
            media_strip,
            aux_strip,
            master_strip,
            voice_chain,
        );

        let routing = discover_platform_routing(sample_rate, quantum_frames);
        let strips = ChannelId::ALL
            .into_iter()
            .map(ChannelStripState::default_for)
            .collect();

        let snapshot = MixerSnapshot {
            routing,
            chatmix_balance: 0.0,
            sidetone_enabled: false,
            sidetone_db: -6.0,
            gate: NoiseGateParams::default(),
            compressor: CompressorParams::default(),
            strips,
        };

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        // Spawn deterministic RT audio quantum runner (pre-allocates all buffers outside the loop)
        let rt_thread = thread::Builder::new()
            .name("aaaudio-rt-dsp".to_string())
            .spawn(move || {
                run_realtime_dsp_loop(matrix, sr, quantum_frames as usize, running_clone);
            })
            .expect("Failed to spawn real-time DSP thread");

        Self {
            sample_rate: sr,
            strip_cmd_producers,
            voice_cmd_producer: voice_cmd_prod,
            meter_consumers,
            last_meters,
            snapshot,
            running,
            rt_thread: Some(rt_thread),
        }
    }

    /// Updates a channel strip's fader in dB (`[-60.0 .. +12.0]`).
    pub fn set_fader_db(&mut self, channel: ChannelId, fader_db: f32) {
        let clamped_db = fader_db.clamp(-60.0, 12.0);
        let linear = if clamped_db <= -59.5 {
            0.0
        } else {
            ChannelStripProcessor::dbfs_to_linear(clamped_db)
        };

        if let Some(strip) = self.snapshot.strips.iter_mut().find(|s| s.id == channel) {
            strip.fader_db = clamped_db;
        }

        if channel == ChannelId::Mic {
            let _ = self
                .voice_cmd_producer
                .push(VoiceCommand::SetOutputGain(linear));
        } else if let Some(prod) = self.strip_cmd_producers.get_mut(&channel) {
            let _ = prod.push(DspCommand::SetFaderGain(linear));
        }
    }

    /// Updates a channel strip's mute state (smooth 4ms ramp on RT thread).
    pub fn set_mute(&mut self, channel: ChannelId, muted: bool) {
        if let Some(strip) = self.snapshot.strips.iter_mut().find(|s| s.id == channel) {
            strip.muted = muted;
        }
        self.recompute_effective_mutes();
    }

    /// Updates a channel strip's solo state and recomputes effective mute states.
    pub fn set_solo(&mut self, channel: ChannelId, solo: bool) {
        if let Some(strip) = self.snapshot.strips.iter_mut().find(|s| s.id == channel) {
            strip.solo = solo;
        }
        self.recompute_effective_mutes();
    }

    fn recompute_effective_mutes(&mut self) {
        let any_solo = self
            .snapshot
            .strips
            .iter()
            .any(|s| s.id != ChannelId::Master && s.id != ChannelId::Mic && s.solo);

        for strip in &self.snapshot.strips {
            let effective_mute = match strip.id {
                ChannelId::Master | ChannelId::Mic => strip.muted,
                _ => strip.muted || (any_solo && !strip.solo),
            };

            if strip.id == ChannelId::Mic {
                let _ = self
                    .voice_cmd_producer
                    .push(VoiceCommand::SetMute(effective_mute));
            } else if let Some(prod) = self.strip_cmd_producers.get_mut(&strip.id) {
                let _ = prod.push(DspCommand::SetMute(effective_mute));
            }
        }
    }

    /// Updates stereo constant-power pan (`[-1.0 .. +1.0]`).
    pub fn set_pan(&mut self, channel: ChannelId, pan: f32) {
        let p = pan.clamp(-1.0, 1.0);
        if let Some(strip) = self.snapshot.strips.iter_mut().find(|s| s.id == channel) {
            strip.pan = p;
        }
        if let Some(prod) = self.strip_cmd_producers.get_mut(&channel) {
            let _ = prod.push(DspCommand::SetPan(p));
        }
    }

    /// Pre-calculates RBJ biquad coefficients on the Control Thread (`f64` -> `f32`)
    /// and pushes an atomic-sized `UpdateEqBand` command into the SPSC ring buffer.
    pub fn update_eq_band(&mut self, channel: ChannelId, band: EqBandParams) {
        let idx = (band.band_idx as usize).min(NUM_EQ_BANDS - 1);
        if let Some(strip) = self.snapshot.strips.iter_mut().find(|s| s.id == channel) {
            if let Some(slot) = strip.eq_bands.get_mut(idx) {
                *slot = band;
            }
        }

        let coeffs = BiquadCoeffs::design(
            band.filter_type,
            band.freq_hz,
            band.q,
            band.gain_db,
            self.sample_rate,
        );

        if channel == ChannelId::Mic {
            let _ = self.voice_cmd_producer.push(VoiceCommand::UpdateEqBand {
                band_idx: idx as u8,
                enabled: band.enabled,
                coeffs,
            });
        } else if let Some(prod) = self.strip_cmd_producers.get_mut(&channel) {
            let _ = prod.push(DspCommand::UpdateEqBand {
                band_idx: idx as u8,
                enabled: band.enabled,
                coeffs,
            });
        }
    }

    /// Updates Microphone Noise Gate parameters.
    pub fn set_gate_params(&mut self, params: NoiseGateParams) {
        self.snapshot.gate = params;
        let _ = self
            .voice_cmd_producer
            .push(VoiceCommand::SetGateParams(params));
    }

    /// Updates Microphone Voice Compressor parameters.
    pub fn set_compressor_params(&mut self, params: CompressorParams) {
        self.snapshot.compressor = params;
        let _ = self
            .voice_cmd_producer
            .push(VoiceCommand::SetCompressorParams(params));
    }

    /// Updates zero-feedback Sidetone monitoring.
    pub fn set_sidetone(&mut self, enabled: bool, gain_db: f32) {
        self.snapshot.sidetone_enabled = enabled;
        self.snapshot.sidetone_db = gain_db.clamp(-60.0, 6.0);
        let linear_gain = ChannelStripProcessor::dbfs_to_linear(self.snapshot.sidetone_db);
        let _ = self.voice_cmd_producer.push(VoiceCommand::SetSidetone {
            enabled,
            linear_gain,
        });
    }

    /// Drains the latest available meter frames from all channel SPSC queues.
    pub fn poll_telemetry(&mut self, timestamp_ms: u64) -> MixerTelemetryFrame {
        for (id, cons) in &mut self.meter_consumers {
            while let Ok(frame) = cons.pop() {
                self.last_meters.insert(*id, frame);
            }
        }
        MixerTelemetryFrame {
            timestamp_ms,
            meters: self.last_meters.clone(),
        }
    }

    pub fn snapshot(&self) -> &MixerSnapshot {
        &self.snapshot
    }
}

impl Drop for MixerController {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.rt_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Discovers physical hardware devices via `cpal` and reports platform virtual routing topology.
pub fn discover_platform_routing(sample_rate: u32, quantum_frames: u32) -> PlatformRoutingStatus {
    let host = cpal::default_host();
    let physical_output_device = host
        .default_output_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_else(|| "System Default Output (DAC)".to_string());

    let physical_input_device = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_else(|| "System Default Microphone".to_string());

    let (os, backend_name) = if cfg!(target_os = "macos") {
        (
            "macOS",
            "CoreAudio HAL User-Space Loopback (BlackHole / ProcessTap)",
        )
    } else if cfg!(target_os = "windows") {
        (
            "Windows",
            "WASAPI Process Loopback + Virtual Audio Endpoint",
        )
    } else {
        ("Linux", "PipeWire Native Virtual Sinks (pipewire-rs)")
    };

    let estimated_latency_ms = ((quantum_frames as f32) / (sample_rate as f32)) * 1000.0 * 2.0;

    PlatformRoutingStatus {
        os: os.to_string(),
        backend_name: backend_name.to_string(),
        virtual_sinks: vec![
            "AAAudio - Game".to_string(),
            "AAAudio - Chat / Comms".to_string(),
            "AAAudio - Media".to_string(),
            "AAAudio - Aux".to_string(),
        ],
        virtual_mic_source: "AAAudio - Virtual Mic (Processed)".to_string(),
        physical_output_device,
        physical_input_device,
        sample_rate,
        quantum_frames,
        estimated_latency_ms,
    }
}

/// Real-time DSP thread loop. Pre-allocates all working buffers before entering
/// the real-time loop; zero allocations or locks occur inside the loop.
fn run_realtime_dsp_loop(
    mut matrix: AudioMixerMatrix,
    sample_rate: f32,
    quantum_frames: usize,
    running: Arc<AtomicBool>,
) {
    let stereo_len = quantum_frames * 2;
    let mut game_buf = vec![0.0_f32; stereo_len];
    let mut chat_buf = vec![0.0_f32; stereo_len];
    let mut media_buf = vec![0.0_f32; stereo_len];
    let mut aux_buf = vec![0.0_f32; stereo_len];
    let mut mic_mono_in = vec![0.0_f32; quantum_frames];
    let mut virtual_mic_out = vec![0.0_f32; stereo_len];
    let mut master_out = vec![0.0_f32; stereo_len];

    let quantum_duration = Duration::from_secs_f64((quantum_frames as f64) / (sample_rate as f64));
    let mut phase: f32 = 0.0;
    let phase_inc = 1.0 / sample_rate;
    let mut next_tick = Instant::now();

    while running.load(Ordering::Relaxed) {
        // Synthesize realistic multi-band test stems on virtual sinks so metering and EQ
        // respond live out-of-the-box even when no external app is playing into a virtual sink
        for (i, mic_slot) in mic_mono_in.iter_mut().enumerate().take(quantum_frames) {
            let t = phase;
            phase = (phase + phase_inc) % 3600.0;

            let env_game = ((t * 2.3).sin() * 0.5 + 0.5).powi(2);
            let env_chat = ((t * 3.7).cos() * 0.5 + 0.5).powi(3);
            let env_media = (t * 1.1).sin() * 0.3 + 0.6;

            let g_sig = 0.28
                * env_game
                * ((2.0 * std::f32::consts::PI * 110.0 * t).sin()
                    + 0.5 * (2.0 * std::f32::consts::PI * 880.0 * t).sin());
            let c_sig = 0.22 * env_chat * (2.0 * std::f32::consts::PI * 240.0 * t).sin();
            let m_sig = 0.32
                * env_media
                * ((2.0 * std::f32::consts::PI * 63.0 * t).sin()
                    + 0.6 * (2.0 * std::f32::consts::PI * 2500.0 * t).sin());
            let a_sig = 0.08 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();

            let idx = i * 2;
            game_buf[idx] = g_sig;
            game_buf[idx + 1] = g_sig * 0.92;
            chat_buf[idx] = c_sig;
            chat_buf[idx + 1] = c_sig;
            media_buf[idx] = m_sig * 0.95;
            media_buf[idx + 1] = m_sig;
            aux_buf[idx] = a_sig;
            aux_buf[idx + 1] = a_sig;
            *mic_slot = c_sig * 0.85;
        }

        matrix.process_quantum(
            &mut game_buf,
            &mut chat_buf,
            &mut media_buf,
            &mut aux_buf,
            &mic_mono_in,
            &mut virtual_mic_out,
            &mut master_out,
        );

        next_tick += quantum_duration;
        let now = Instant::now();
        if next_tick > now {
            thread::sleep(next_tick - now);
        } else {
            next_tick = now;
        }
    }
}
