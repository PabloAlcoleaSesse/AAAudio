# AAAudio Technical Specification & Architecture Design

This document details the real-time systems architecture, lock-free thread model, cross-platform virtual audio routing strategy, and DSP mathematical foundations of **AAAudio**.

---

## 1. System Architecture & Threading Model

To guarantee hard real-time audio deadlines (< 10 ms round-trip latency) while keeping idle memory below 50 MB, the application is partitioned into three strict execution domains with zero shared mutable locks across the real-time boundary:

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ 1. UI & IPC THREAD (Tauri WebView / TypeScript + Canvas 2D)                  │
│  - Renders 60/120 FPS Parametric EQ graphs, faders, and ballistic VU meters  │
│  - Dispatches discrete user events via Tauri Commands                        │
│  - Consumes binary telemetry stream via IPC Channel                          │
└───────────────────┬──────────────────────────────────────▲───────────────────┘
                    │ Async Command                        │ 60 Hz Telemetry Frame
                    ▼                                      │
┌──────────────────────────────────────────────────────────────────────────────┐
│ 2. HOST CONTROL & GRAPH ORCHESTRATOR THREAD (aaaudio-router)                 │
│  - Manages OS Audio Routing (PipeWire nodes, WASAPI Process Loopback, HAL)   │
│  - Pre-computes biquad coefficients (f64 -> f32) and envelope time constants │
│  - Owns rtrb::Producer<DspCommand> and rtrb::Consumer<MeterFrame>            │
└───────────────────┬──────────────────────────────────────▲───────────────────┘
                    │ Lock-Free SPSC (rtrb)                │ Lock-Free SPSC (rtrb)
                    │ Capacity: 512 cmds                   │ Capacity: 128 frames
                    ▼                                      │
┌──────────────────────────────────────────────────────────────────────────────┐
│ 3. REAL-TIME AUDIO CALLBACK THREAD (aaaudio-dsp)                             │
│  - Strictly NO heap allocations (Box, Vec, String), NO Mutex/RwLock, NO I/O  │
│  - Hardware Denormal Suppression (ScopedNoDenormals: FTZ / DAZ)              │
│  - Drains rtrb::Consumer<DspCommand> at start of audio quantum               │
│  - Steps One-Pole exponential smoothers per sample                           │
│  - Executes 10-band TDF-II Biquad cascades, Voice Gate/Comp, & Mixing Matrix │
│  - Pushes decimated Peak/RMS & Gain Reduction via rtrb::Producer::push()     │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Lock-Free Parameter Update Pipeline
1. **UI -> Control:** Pointer drags on faders or EQ nodes invoke the Host Control Thread.
2. **Control -> RT Ring Buffer:** The Host Control Thread computes normalized `f64` RBJ biquad filter coefficients, downcasts to `f32`, and pushes a `Copy` `DspCommand` into the channel's `rtrb::Producer<DspCommand>`.
3. **RT Callback Ingestion:** At the start of each audio quantum (`128` frames @ `48 kHz` = `2.66 ms`), the real-time thread drains pending commands from `rtrb::Consumer<DspCommand>`.
4. **Glitch-Free Application:**
   - **Fader Gains, Mutes & Pan:** Update the target of a one-pole exponential smoother (`SmoothedParam`). Mute toggles ramp smoothly over `4 ms` rather than hard-stepping, eliminating clicks.
   - **EQ Coefficients:** Linearly interpolate `(b0, b1, b2, a1, a2)` per sample over `3 ms` inside a Transposed Direct Form II (`StereoBiquad`) structure.

---

## 2. Cross-Platform Virtual Routing Strategy (Zero Kernel Signing Certs)

### Linux: Native PipeWire (`pipewire-rs`)
- Instantiates 4 virtual stereo `Audio/Sink` nodes (`AAAudio - Game`, `Chat / Comms`, `Media`, `Aux`) and 1 virtual `Audio/Source` node (`AAAudio - Virtual Mic`).
- Dynamically links application `Stream/Output/Audio` ports to virtual sinks using PipeWire's registry and metadata API.

### Windows: Dual-Mode WASAPI Process Loopback + User-Space Virtual Endpoints
- **Per-Application Capture:** Uses native Windows 10/11 `ActivateAudioInterfaceAsync` with `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK` (`PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`) to isolate specific application PIDs (`Game`, `Media`, `Aux`) without any kernel driver.
- **Voice/Comms Endpoint:** Pairs with signed open-source virtual endpoints (VDD / VB-CABLE) captured via low-latency WASAPI Loopback (`AUDCLNT_STREAMFLAGS_LOOPBACK`).

### macOS: CoreAudio User-Space HAL Plug-In (`AudioServerPlugIn`) & Process Tap
- Uses user-space `/Library/Audio/Plug-Ins/HAL/` drivers (compatible with `BlackHole` 2ch/16ch or custom shared-memory HAL bundles) and macOS 14.2+ `AudioHardwareCreateProcessTap` for per-process audio isolation—running 100% in user space inside `coreaudiod` without deprecated `kext` entitlements.

---

## 3. Mathematical Formulation for the 10-Band Parametric EQ

For each active biquad stage with normalized coefficients $(b_0, b_1, b_2, 1, a_1, a_2)$ evaluated on the unit circle $z = e^{j\omega}$ where $\omega = 2\pi f / f_s$:

$$H_k(e^{j\omega}) = \frac{b_0 + b_1 e^{-j\omega} + b_2 e^{-j2\omega}}{1 + a_1 e^{-j\omega} + a_2 e^{-j2\omega}}$$

Using precomputed trigonometric tables for $\cos\omega, \sin\omega, \cos 2\omega, \sin 2\omega$ across 512 logarithmically spaced points from $20\text{ Hz}$ to $20\text{ kHz}$:

$$|H_k(e^{j\omega})|^2 = \frac{(b_0 + b_1 \cos\omega + b_2 \cos 2\omega)^2 + (b_1 \sin\omega + b_2 \sin 2\omega)^2}{(1 + a_1 \cos\omega + a_2 \cos 2\omega)^2 + (a_1 \sin\omega + a_2 \sin 2\omega)^2}$$

$$G_{\text{total}}(f)\text{ [dB]} = \sum_{k=1}^{10} 10 \log_{10}\left(|H_k(e^{j\omega})|^2\right)$$
