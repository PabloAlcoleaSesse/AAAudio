// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use aaaudio_dsp::{ChannelId, CompressorParams, EqBandParams, NoiseGateParams};
use aaaudio_router::{MixerController, MixerSnapshot, MixerTelemetryFrame};
use std::sync::Mutex;
use std::time::Instant;
use tauri::State;

struct AppState {
    controller: Mutex<MixerController>,
    start_instant: Instant,
}

#[tauri::command]
fn get_mixer_snapshot(state: State<'_, AppState>) -> MixerSnapshot {
    let ctrl = state.controller.lock().unwrap();
    ctrl.snapshot().clone()
}

#[tauri::command]
fn poll_mixer_telemetry(state: State<'_, AppState>) -> MixerTelemetryFrame {
    let elapsed_ms = state.start_instant.elapsed().as_millis() as u64;
    let mut ctrl = state.controller.lock().unwrap();
    ctrl.poll_telemetry(elapsed_ms)
}

#[tauri::command]
fn set_channel_fader(state: State<'_, AppState>, channel: ChannelId, fader_db: f32) {
    let mut ctrl = state.controller.lock().unwrap();
    ctrl.set_fader_db(channel, fader_db);
}

#[tauri::command]
fn set_channel_mute(state: State<'_, AppState>, channel: ChannelId, muted: bool) {
    let mut ctrl = state.controller.lock().unwrap();
    ctrl.set_mute(channel, muted);
}

#[tauri::command]
fn set_channel_solo(state: State<'_, AppState>, channel: ChannelId, solo: bool) {
    let mut ctrl = state.controller.lock().unwrap();
    ctrl.set_solo(channel, solo);
}

#[tauri::command]
fn update_eq_band(state: State<'_, AppState>, channel: ChannelId, band: EqBandParams) {
    let mut ctrl = state.controller.lock().unwrap();
    ctrl.update_eq_band(channel, band);
}

#[tauri::command]
fn set_chatmix_balance(state: State<'_, AppState>, balance: f32) {
    let mut ctrl = state.controller.lock().unwrap();
    ctrl.set_chatmix_balance(balance);
}

#[tauri::command]
fn set_sidetone(state: State<'_, AppState>, enabled: bool, gain_db: f32) {
    let mut ctrl = state.controller.lock().unwrap();
    ctrl.set_sidetone(enabled, gain_db);
}

#[tauri::command]
fn set_gate_params(state: State<'_, AppState>, params: NoiseGateParams) {
    let mut ctrl = state.controller.lock().unwrap();
    ctrl.set_gate_params(params);
}

#[tauri::command]
fn set_compressor_params(state: State<'_, AppState>, params: CompressorParams) {
    let mut ctrl = state.controller.lock().unwrap();
    ctrl.set_compressor_params(params);
}

fn main() {
    let controller = MixerController::new(48_000, 128);
    let app_state = AppState {
        controller: Mutex::new(controller),
        start_instant: Instant::now(),
    };

    tauri::Builder::default()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            get_mixer_snapshot,
            poll_mixer_telemetry,
            set_channel_fader,
            set_channel_mute,
            set_channel_solo,
            update_eq_band,
            set_chatmix_balance,
            set_sidetone,
            set_gate_params,
            set_compressor_params,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AAAudio desktop application");
}
