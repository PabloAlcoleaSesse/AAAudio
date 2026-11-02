import './style.css';
import { invoke } from '@tauri-apps/api/core';
import { EqBandNode, FilterType, ParametricEqCanvas } from './dsp/ParametricEqCanvas';

type ChannelKey = 'master' | 'game' | 'chat' | 'media' | 'aux' | 'mic';

interface ChannelState {
  id: ChannelKey;
  label: string;
  faderDb: number;
  muted: boolean;
  solo: boolean;
  pan: number;
  meterDb: number;
  bands: EqBandNode[];
}

interface MeterFrame {
  peak_l: number;
  peak_r: number;
  rms_l: number;
  rms_r: number;
  gain_reduction_db: number;
  gate_open: number;
}

interface MixerTelemetryFrame {
  timestamp_ms: number;
  meters: Record<ChannelKey, MeterFrame>;
}

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

async function safeInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T | null> {
  if (!isTauri) return null;
  try {
    return await invoke<T>(cmd, args);
  } catch {
    return null;
  }
}

const BAND_COLORS = [
  '#ef4444',
  '#f97316',
  '#f59e0b',
  '#eab308',
  '#84cc16',
  '#22c55e',
  '#14b8a6',
  '#06b6d4',
  '#6366f1',
  '#ec4899',
];

const DEFAULT_FREQS = [31.5, 63, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];

function createDefaultBands(presetBoostIdx?: number, presetGain = 0): EqBandNode[] {
  return DEFAULT_FREQS.map((freqHz, i) => ({
    id: i,
    enabled: true,
    type: i === 0 ? 'lowshelf' : i === 9 ? 'highshelf' : 'peaking',
    freqHz,
    gainDb: i === presetBoostIdx ? presetGain : 0,
    q: 0.707,
    color: BAND_COLORS[i],
  }));
}

const channels: ChannelState[] = [
  {
    id: 'master',
    label: 'Master',
    faderDb: 0.0,
    muted: false,
    solo: false,
    pan: 0,
    meterDb: -14,
    bands: createDefaultBands(),
  },
  {
    id: 'game',
    label: 'Game',
    faderDb: 0.0,
    muted: false,
    solo: false,
    pan: 0,
    meterDb: -18,
    bands: createDefaultBands(1, 3.5),
  },
  {
    id: 'chat',
    label: 'Chat',
    faderDb: 0.0,
    muted: false,
    solo: false,
    pan: 0,
    meterDb: -20,
    bands: createDefaultBands(6, 2.5),
  },
  {
    id: 'media',
    label: 'Media',
    faderDb: -1.5,
    muted: false,
    solo: false,
    pan: 0,
    meterDb: -16,
    bands: createDefaultBands(0, 2.0),
  },
  {
    id: 'aux',
    label: 'Aux',
    faderDb: -6.0,
    muted: false,
    solo: false,
    pan: 0,
    meterDb: -36,
    bands: createDefaultBands(),
  },
  {
    id: 'mic',
    label: 'Mic DSP',
    faderDb: 0.0,
    muted: false,
    solo: false,
    pan: 0,
    meterDb: -22,
    bands: createDefaultBands(5, 2.0),
  },
];

let selectedChannelIdx = 1; // Default to Game channel strip
let chatMixBalance = 0.0;
let gateThresholdDb = -42.0;
let compThresholdDb = -18.0;
const compRatio = 4.0;
let sidetoneDb = -6.0;
let sidetoneEnabled = true;

const app = document.querySelector<HTMLDivElement>('#app')!;

app.innerHTML = `
  <div class="console-shell">
    <header class="topbar">
      <div class="brand">
        <span class="brand-badge">AAAudio</span>
        <h1>Ultra-Lean Real-Time Software Audio Mixer & Router</h1>
      </div>
      <div class="telemetry-pills">
        <span class="pill" id="device-pill">Quantum: <strong>128 frames @ 48 kHz (5.33 ms)</strong></span>
        <span class="pill">Sync: <strong>rtrb Lock-Free SPSC</strong></span>
        <span class="pill">Audio Thread: <strong>0 Allocs / FTZ+DAZ</strong></span>
      </div>
    </header>

    <main class="workspace-grid">
      <section class="strips-rack" id="strips-rack"></section>

      <section class="dsp-panel">
        <div class="dsp-header">
          <h2 id="eq-panel-title">10-Band Parametric EQ — Game</h2>
          <div class="control-row">
            <label for="band-type-select">Selected Node Filter:</label>
            <select id="band-type-select">
              <option value="peaking">Peaking Bell</option>
              <option value="lowshelf">Low Shelf</option>
              <option value="highshelf">High Shelf</option>
              <option value="highpass">High-Pass (12 dB/oct)</option>
              <option value="lowpass">Low-Pass (12 dB/oct)</option>
              <option value="bypass">Bypass</option>
            </select>
            <span id="active-node-readout" class="strip-db-readout">Band 1: 63 Hz | +3.5 dB | Q 0.71</span>
          </div>
        </div>

        <div class="eq-canvas-wrap">
          <canvas id="eq-canvas"></canvas>
        </div>

        <div class="sub-panels">
          <div class="card">
            <h3>ChatMix & Routing Balance</h3>
            <div class="control-row">
              <span>Game</span>
              <input id="chatmix-slider" type="range" min="-1" max="1" step="0.01" value="0" />
              <span>Chat</span>
              <span id="chatmix-val" class="strip-db-readout">Center (0 dB)</span>
            </div>
            <div class="control-row">
              <span>Zero-Feedback Sidetone</span>
              <input id="sidetone-slider" type="range" min="-36" max="6" step="0.5" value="-6" />
              <span id="sidetone-val" class="strip-db-readout">-6.0 dB</span>
            </div>
          </div>

          <div class="card">
            <h3>Microphone Voice Chain (Gate + Compressor)</h3>
            <div class="control-row">
              <span>Noise Gate Thresh</span>
              <input id="gate-slider" type="range" min="-60" max="-15" step="0.5" value="-42" />
              <span id="gate-val" class="strip-db-readout">-42.0 dBFS</span>
            </div>
            <div class="control-row">
              <span>Compressor Thresh</span>
              <input id="comp-slider" type="range" min="-40" max="0" step="0.5" value="-18" />
              <span id="comp-val" class="strip-db-readout">-18.0 dB (4:1)</span>
            </div>
          </div>
        </div>
      </section>
    </main>
  </div>
`;

const stripsRack = document.getElementById('strips-rack')!;
const eqTitleEl = document.getElementById('eq-panel-title')!;
const activeNodeReadout = document.getElementById('active-node-readout')!;
const bandTypeSelect = document.getElementById('band-type-select') as HTMLSelectElement;
const eqCanvasEl = document.getElementById('eq-canvas') as HTMLCanvasElement;

function syncBandToBackend(channelId: ChannelKey, band: EqBandNode): void {
  void safeInvoke('update_eq_band', {
    channel: channelId,
    band: {
      band_idx: band.id,
      enabled: band.enabled,
      filter_type: band.type,
      freq_hz: band.freqHz,
      q: band.q,
      gain_db: band.gainDb,
    },
  });
}

function updateNodeReadout(band: EqBandNode): void {
  const fStr =
    band.freqHz >= 1000 ? `${(band.freqHz / 1000).toFixed(2)} kHz` : `${band.freqHz.toFixed(0)} Hz`;
  const gStr = `${band.gainDb >= 0 ? '+' : ''}${band.gainDb.toFixed(1)} dB`;
  activeNodeReadout.textContent = `Band ${band.id + 1}: ${fStr} | ${gStr} | Q ${band.q.toFixed(2)}`;
  bandTypeSelect.value = band.type;
}

const eqGraph = new ParametricEqCanvas(
  eqCanvasEl,
  channels[selectedChannelIdx].bands,
  48000,
  (updatedBand) => {
    updateNodeReadout(updatedBand);
    syncBandToBackend(channels[selectedChannelIdx].id, updatedBand);
  }
);

function renderStrips(): void {
  stripsRack.innerHTML = '';

  channels.forEach((ch, idx) => {
    const stripEl = document.createElement('div');
    stripEl.className = `channel-strip ${idx === selectedChannelIdx ? 'selected' : ''}`;

    const dbText =
      ch.faderDb <= -59.5 ? '-∞ dB' : `${ch.faderDb >= 0 ? '+' : ''}${ch.faderDb.toFixed(1)} dB`;

    stripEl.innerHTML = `
      <div class="strip-title">${ch.label}</div>
      <div class="strip-db-readout" id="fader-readout-${ch.id}">${dbText}</div>
      <div class="fader-meter-row">
        <input
          type="range"
          class="fader-vertical"
          min="-60"
          max="12"
          step="0.2"
          value="${ch.faderDb}"
          data-idx="${idx}"
        />
        <div class="vu-meter">
          <div class="vu-bar" id="vu-bar-${ch.id}"></div>
        </div>
      </div>
      <div class="strip-buttons">
        <button class="btn-mini ${ch.muted ? 'mute-active' : ''}" data-action="mute" data-idx="${idx}">M</button>
        <button class="btn-mini ${ch.solo ? 'solo-active' : ''}" data-action="solo" data-idx="${idx}">S</button>
      </div>
    `;

    stripEl.addEventListener('click', (e) => {
      const target = e.target as HTMLElement;
      if (target.tagName === 'INPUT' || target.tagName === 'BUTTON') return;
      selectedChannelIdx = idx;
      eqTitleEl.textContent = `10-Band Parametric EQ — ${ch.label}`;
      eqGraph.setBands(ch.bands);
      updateNodeReadout(ch.bands[eqGraph.getActiveBandIndex()]);
      renderStrips();
    });

    const faderInput = stripEl.querySelector<HTMLInputElement>('.fader-vertical')!;
    faderInput.addEventListener('input', () => {
      ch.faderDb = parseFloat(faderInput.value);
      const readout = document.getElementById(`fader-readout-${ch.id}`)!;
      readout.textContent =
        ch.faderDb <= -59.5 ? '-∞ dB' : `${ch.faderDb >= 0 ? '+' : ''}${ch.faderDb.toFixed(1)} dB`;
      void safeInvoke('set_channel_fader', { channel: ch.id, faderDb: ch.faderDb });
    });

    stripEl.querySelectorAll<HTMLButtonElement>('button.btn-mini').forEach((btn) => {
      btn.addEventListener('click', (ev) => {
        ev.stopPropagation();
        const action = btn.dataset.action;
        if (action === 'mute') {
          ch.muted = !ch.muted;
          void safeInvoke('set_channel_mute', { channel: ch.id, muted: ch.muted });
        }
        if (action === 'solo') {
          ch.solo = !ch.solo;
          void safeInvoke('set_channel_solo', { channel: ch.id, solo: ch.solo });
        }
        renderStrips();
      });
    });

    stripsRack.appendChild(stripEl);
  });
}

bandTypeSelect.addEventListener('change', () => {
  const ch = channels[selectedChannelIdx];
  const bIdx = eqGraph.getActiveBandIndex();
  ch.bands[bIdx].type = bandTypeSelect.value as FilterType;
  eqGraph.render();
  updateNodeReadout(ch.bands[bIdx]);
  syncBandToBackend(ch.id, ch.bands[bIdx]);
});

const chatMixSlider = document.getElementById('chatmix-slider') as HTMLInputElement;
const chatMixVal = document.getElementById('chatmix-val')!;
chatMixSlider.addEventListener('input', () => {
  chatMixBalance = parseFloat(chatMixSlider.value);
  if (Math.abs(chatMixBalance) < 0.03) {
    chatMixVal.textContent = 'Center (0 dB)';
  } else if (chatMixBalance < 0) {
    chatMixVal.textContent = `Game +${Math.round(-chatMixBalance * 100)}%`;
  } else {
    chatMixVal.textContent = `Chat +${Math.round(chatMixBalance * 100)}%`;
  }
  void safeInvoke('set_chatmix_balance', { balance: chatMixBalance });
});

const sidetoneSlider = document.getElementById('sidetone-slider') as HTMLInputElement;
const sidetoneVal = document.getElementById('sidetone-val')!;
sidetoneSlider.addEventListener('input', () => {
  sidetoneDb = parseFloat(sidetoneSlider.value);
  sidetoneEnabled = sidetoneDb > -35.5;
  sidetoneVal.textContent = sidetoneEnabled ? `${sidetoneDb.toFixed(1)} dB` : 'Muted';
  void safeInvoke('set_sidetone', { enabled: sidetoneEnabled, gainDb: sidetoneDb });
});

const gateSlider = document.getElementById('gate-slider') as HTMLInputElement;
const gateVal = document.getElementById('gate-val')!;
gateSlider.addEventListener('input', () => {
  gateThresholdDb = parseFloat(gateSlider.value);
  gateVal.textContent = `${gateThresholdDb.toFixed(1)} dBFS`;
  void safeInvoke('set_gate_params', {
    params: {
      enabled: true,
      threshold_db: gateThresholdDb,
      hysteresis_db: 4.0,
      attack_ms: 2.0,
      hold_ms: 80.0,
      release_ms: 150.0,
    },
  });
});

const compSlider = document.getElementById('comp-slider') as HTMLInputElement;
const compVal = document.getElementById('comp-val')!;
compSlider.addEventListener('input', () => {
  compThresholdDb = parseFloat(compSlider.value);
  compVal.textContent = `${compThresholdDb.toFixed(1)} dB (${compRatio}:1)`;
  void safeInvoke('set_compressor_params', {
    params: {
      enabled: true,
      threshold_db: compThresholdDb,
      ratio: compRatio,
      attack_ms: 5.0,
      release_ms: 80.0,
      makeup_db: 3.0,
      knee_width_db: 6.0,
    },
  });
});

window.addEventListener('resize', () => eqGraph.resize());

renderStrips();
updateNodeReadout(channels[selectedChannelIdx].bands[0]);

function linearToDbfs(lin: number): number {
  return 20 * Math.log10(Math.max(1e-6, lin));
}

// 60 FPS Ballistic VU Meter Loop (live Rust SPSC telemetry in Tauri, or local fallback in browser)
let t = 0;
async function animateMeters(): Promise<void> {
  t += 0.045;

  if (isTauri) {
    const telemetry = await safeInvoke<MixerTelemetryFrame>('poll_mixer_telemetry');
    if (telemetry && telemetry.meters) {
      channels.forEach((ch) => {
        const frame = telemetry.meters[ch.id];
        if (frame) {
          const targetDb = linearToDbfs(Math.max(frame.rms_l, frame.rms_r));
          ch.meterDb = ch.meterDb * 0.65 + targetDb * 0.35;
        }
        const bar = document.getElementById(`vu-bar-${ch.id}`);
        if (bar) {
          const pct = Math.max(0, Math.min(100, ((ch.meterDb + 60) / 66) * 100));
          bar.style.height = `${pct}%`;
        }
      });
      requestAnimationFrame(() => void animateMeters());
      return;
    }
  }

  const anySolo = channels.some((c) => c.id !== 'master' && c.id !== 'mic' && c.solo);
  let masterEnergy = 0;

  channels.forEach((ch, idx) => {
    if (ch.id === 'master') return;

    const effectiveMute = ch.muted || (anySolo && !ch.solo && ch.id !== 'mic');
    if (effectiveMute || ch.faderDb <= -59.5) {
      ch.meterDb = Math.max(-60, ch.meterDb - 2.8);
    } else {
      const wave =
        Math.sin(t * (1.7 + idx * 0.6)) * 4.5 + Math.cos(t * (3.1 + idx * 0.9)) * 3.0;
      const chatMixOffset =
        ch.id === 'game' && chatMixBalance > 0
          ? -chatMixBalance * 24
          : ch.id === 'chat' && chatMixBalance < 0
          ? chatMixBalance * 24
          : 0;
      const targetDb = Math.min(3, -16 + ch.faderDb + wave + chatMixOffset);
      ch.meterDb = ch.meterDb * 0.75 + targetDb * 0.25;

      if (ch.id !== 'mic') {
        masterEnergy += Math.pow(10, ch.meterDb / 10);
      }
    }

    const bar = document.getElementById(`vu-bar-${ch.id}`);
    if (bar) {
      const pct = Math.max(0, Math.min(100, ((ch.meterDb + 60) / 66) * 100));
      bar.style.height = `${pct}%`;
    }
  });

  const master = channels[0];
  if (master.muted || master.faderDb <= -59.5 || masterEnergy <= 1e-9) {
    master.meterDb = Math.max(-60, master.meterDb - 2.8);
  } else {
    const sumDb = 10 * Math.log10(masterEnergy) + master.faderDb;
    master.meterDb = Math.min(3, master.meterDb * 0.75 + sumDb * 0.25);
  }

  const masterBar = document.getElementById('vu-bar-master');
  if (masterBar) {
    const pct = Math.max(0, Math.min(100, ((master.meterDb + 60) / 66) * 100));
    masterBar.style.height = `${pct}%`;
  }

  requestAnimationFrame(() => void animateMeters());
}

requestAnimationFrame(() => void animateMeters());
