# AAAudio

[![License: MIT](https://img.shields.io/badge/License-MIT-38bdf8.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange.svg)](https://www.rust-lang.org/)

**AAAudio** is an ultra-lean, cross-platform software audio mixer, virtual router, and real-time voice/parametric EQ processor inspired by SteelSeries Sonar—built in pure **Rust** and **TypeScript/Canvas**, completely free of bloat, telemetry, and background adware services.

---

## Key Features & Engineering Highlights

- **Hard Real-Time DSP Guarantees (< 10 ms End-to-End Latency):**
  - Zero heap allocations (`Box`, `Vec`, `String`) and zero blocking synchronization primitives (`Mutex`, `RwLock`) on the real-time audio callback thread.
  - Lock-free single-producer single-consumer (`rtrb`) command and metering pipelines.
  - Hardware Flush-To-Zero (`FTZ`) and Denormals-Are-Zero (`DAZ`) RAII guards (`ScopedNoDenormals`) on `x86_64` and `aarch64`.
- **6 Dedicated Channel Strips + ChatMix:**
  - `Master`, `Game`, `Chat / Comms`, `Media`, `Aux`, and `Microphone`.
  - Hardware-style faders with one-pole exponential smoothing (`SmoothedParam`) to eliminate zipper noise and mute clicks.
  - Constant-power stereo panning and a dedicated **ChatMix** (`Game <-> Chat`) balance control.
- **10-Band Stereo Parametric Equalizer (`ParametricEq10`):**
  - Full Robert Bristow-Johnson (RBJ) Audio EQ Cookbook implementation (`Peaking`, `LowShelf`, `HighShelf`, `HighPass`, `LowPass`, `Bypass`) computed in `f64` and executed in `f32` Transposed Direct Form II (`StereoBiquad`) with per-sample coefficient interpolation.
- **Dedicated Microphone Voice DSP Chain (`VoiceChainProcessor`):**
  - 80 Hz 2nd-Order Butterworth High-Pass rumble filter.
  - Hysteresis **Noise Gate** (`Threshold`, `Hysteresis`, `Attack`, `Hold`, `Release`).
  - Feed-forward Soft-Knee **Voice Compressor** (`Threshold`, `Ratio`, `Attack`, `Release`, `Makeup Gain`).
  - **Zero-Feedback Sidetone Loopback:** Injects low-latency conditioned voice directly into the hardware master bus without bleeding into the virtual microphone stream.
- **Interactive 60/120 FPS Canvas Parametric EQ (`ParametricEqCanvas.ts`):**
  - Precomputed trigonometric lookup tables evaluating $H(e^{j\omega})$ across 512 logarithmic frequency points (`20 Hz – 20 kHz`), rendered via smooth Catmull-Rom / cubic Bézier splines with draggable nodes and scroll-wheel $Q$ control.

---

## Repository Structure

```text
AAAudio/
├── Cargo.toml                  # Rust workspace manifest
├── crates/
│   ├── aaaudio-dsp/            # Real-time zero-allocation DSP crate (EQ, Smoothers, Voice Chain, Matrix)
│   ├── aaaudio-router/         # Cross-platform virtual routing & Host Control Thread orchestrator
│   └── aaaudio-daemon/         # CLI & real-time engine runner binary (`aaaudio`)
├── ui/                         # Lightweight TypeScript + Vite digital mixing console & Canvas EQ
│   ├── src/dsp/ParametricEqCanvas.ts
│   ├── src/main.ts
│   └── src/style.css
└── docs/
    └── ARCHITECTURE.md         # Full technical specification & OS routing architecture
```

---

## Quick Start

### 1. Run the Rust DSP Test Suite & Real-Time Engine Self-Test

```bash
# Run all unit tests (RBJ Biquad accuracy, Smoother convergence, Voice Chain, ChatMix, SPSC rings)
cargo test --workspace

# Run the real-time audio engine & telemetry self-test
cargo run --bin aaaudio -- --self-test
```

### 2. Launch the Interactive Mixing Console & Parametric EQ UI

```bash
cd ui
npm install
npm run dev
```

Then open `http://localhost:1420` (or wrap with Tauri v2) to interact with the 6-channel mixing console, drag 10-band Parametric EQ nodes (scroll wheel adjusts $Q$, double-click resets gain to `0 dB`), and tune the Microphone Voice Chain.

---

## Architecture & Deep Dive

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the complete technical specification covering:
1. Separation between the **UI Event Loop**, **Host Control Thread**, and **Real-Time Audio Thread**.
2. Cross-platform virtual audio routing without proprietary kernel-signing certificates (**Linux** `pipewire-rs`, **Windows** WASAPI Process Loopback + Virtual Endpoints, **macOS** CoreAudio User-Space HAL / `BlackHole` / Process Tap).
3. Transfer function $H(f)$ and coordinate mapping math.

---

## License

Licensed under the [MIT License](LICENSE).
