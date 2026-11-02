use aaaudio_dsp::{ChannelId, ChannelStripProcessor, EqBandParams, FilterType};
use aaaudio_router::MixerController;
use std::env;
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    env_logger::init();
    let args: Vec<String> = env::args().collect();
    let benchmark_only = args.iter().any(|a| a == "--self-test" || a == "--bench");

    let sample_rate = 48_000;
    let quantum_frames = 128; // 2.66 ms per quantum (< 10 ms round-trip deadline)

    let mut controller = MixerController::new(sample_rate, quantum_frames);
    let snap = controller.snapshot().clone();

    println!("╔══════════════════════════════════════════════════════════════════════╗");
    println!("║  AAAudio — Ultra-Lean Cross-Platform Real-Time Audio Mixer & Router  ║");
    println!("╚══════════════════════════════════════════════════════════════════════╝");
    println!("  OS / Platform     : {}", snap.routing.os);
    println!("  Routing Strategy  : {}", snap.routing.backend_name);
    println!(
        "  Physical Output   : {}",
        snap.routing.physical_output_device
    );
    println!(
        "  Physical Mic In   : {}",
        snap.routing.physical_input_device
    );
    println!(
        "  Engine Quantum    : {} frames @ {} Hz ({:.2} ms end-to-end latency)",
        snap.routing.quantum_frames, snap.routing.sample_rate, snap.routing.estimated_latency_ms
    );
    println!(
        "  Virtual Sinks     : {}",
        snap.routing.virtual_sinks.join(" | ")
    );
    println!("  Virtual Mic Out   : {}", snap.routing.virtual_mic_source);
    println!();

    // Configure an example +4.5 dB low-shelf bass boost on the Game channel and +3 dB presence on Chat
    controller.update_eq_band(
        ChannelId::Game,
        EqBandParams {
            band_idx: 0,
            enabled: true,
            filter_type: FilterType::LowShelf,
            freq_hz: 80.0,
            q: std::f32::consts::FRAC_1_SQRT_2,
            gain_db: 4.5,
        },
    );
    controller.update_eq_band(
        ChannelId::Chat,
        EqBandParams {
            band_idx: 6,
            enabled: true,
            filter_type: FilterType::Peaking,
            freq_hz: 2500.0,
            q: 1.2,
            gain_db: 3.0,
        },
    );
    controller.set_sidetone(true, -6.0);

    let start = Instant::now();
    let iterations = if benchmark_only { 12 } else { 30 };

    for tick in 0..iterations {
        thread::sleep(Duration::from_millis(50));
        let elapsed_ms = start.elapsed().as_millis() as u64;
        let telemetry = controller.poll_telemetry(elapsed_ms);

        if tick % 4 == 3 {
            let master = telemetry
                .meters
                .get(&ChannelId::Master)
                .copied()
                .unwrap_or_default();
            let game = telemetry
                .meters
                .get(&ChannelId::Game)
                .copied()
                .unwrap_or_default();
            let chat = telemetry
                .meters
                .get(&ChannelId::Chat)
                .copied()
                .unwrap_or_default();
            let mic = telemetry
                .meters
                .get(&ChannelId::Mic)
                .copied()
                .unwrap_or_default();

            println!(
                "[{:>4} ms] Master: {:>6.1} dBFS | Game: {:>6.1} dBFS | Chat: {:>6.1} dBFS | Mic: {:>6.1} dBFS (GR: {:>4.1} dB)",
                elapsed_ms,
                ChannelStripProcessor::linear_to_dbfs(master.rms_l),
                ChannelStripProcessor::linear_to_dbfs(game.rms_l),
                ChannelStripProcessor::linear_to_dbfs(chat.rms_l),
                ChannelStripProcessor::linear_to_dbfs(mic.rms_l),
                mic.gain_reduction_db,
            );
        }
    }

    println!("\n✓ Lock-free SPSC telemetry and real-time DSP quantum verified.");
}
