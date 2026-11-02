// ParametricEqCanvas.ts
// Zero-dependency, 60/120 FPS interactive 10-band Parametric EQ renderer & controller

export type FilterType = 'bypass' | 'peaking' | 'lowshelf' | 'highshelf' | 'highpass' | 'lowpass';

export interface EqBandNode {
  id: number;
  enabled: boolean;
  type: FilterType;
  freqHz: number;  // [20 .. 20000]
  gainDb: number;  // [-24 .. +24]
  q: number;       // [0.1 .. 24.0]
  color: string;
}

interface BiquadCoeffs {
  b0: number;
  b1: number;
  b2: number;
  a1: number;
  a2: number;
}

export type BandUpdateCallback = (band: EqBandNode) => void;

const MIN_FREQ = 20;
const MAX_FREQ = 20000;
const LOG_FREQ_SPAN = Math.log10(MAX_FREQ / MIN_FREQ); // log10(1000) = 3.0
const MAX_DB = 24;
const NUM_EVAL_POINTS = 512;

export class ParametricEqCanvas {
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private sampleRate: number;
  private bands: EqBandNode[];
  private onBandChange: BandUpdateCallback;

  // Precomputed evaluation grid (eliminates per-frame trig overhead)
  private evalCosW: Float64Array = new Float64Array(NUM_EVAL_POINTS);
  private evalSinW: Float64Array = new Float64Array(NUM_EVAL_POINTS);
  private evalCos2W: Float64Array = new Float64Array(NUM_EVAL_POINTS);
  private evalSin2W: Float64Array = new Float64Array(NUM_EVAL_POINTS);
  private totalDbCurve: Float32Array = new Float32Array(NUM_EVAL_POINTS);

  private activeBandIdx: number = 0;
  private hoveredBandIdx: number = -1;
  private isDragging: boolean = false;
  private dpr: number = 1;
  private width: number = 0;
  private height: number = 0;

  constructor(
    canvas: HTMLCanvasElement,
    initialBands: EqBandNode[],
    sampleRate = 48000,
    onBandChange: BandUpdateCallback
  ) {
    this.canvas = canvas;
    const ctx = canvas.getContext('2d', { alpha: false });
    if (!ctx) throw new Error('Failed to acquire 2D canvas context');
    this.ctx = ctx;
    this.bands = initialBands;
    this.sampleRate = sampleRate;
    this.onBandChange = onBandChange;

    this.precomputeTrigTables();
    this.resize();
    this.bindEvents();
    this.render();
  }

  public setBands(bands: EqBandNode[]): void {
    this.bands = bands;
    this.render();
  }

  public getActiveBandIndex(): number {
    return this.activeBandIdx;
  }

  private precomputeTrigTables(): void {
    for (let i = 0; i < NUM_EVAL_POINTS; i++) {
      const normX = i / (NUM_EVAL_POINTS - 1);
      const freq = MIN_FREQ * Math.pow(10, normX * LOG_FREQ_SPAN);
      const w = (2 * Math.PI * freq) / this.sampleRate;
      this.evalCosW[i] = Math.cos(w);
      this.evalSinW[i] = Math.sin(w);
      this.evalCos2W[i] = Math.cos(2 * w);
      this.evalSin2W[i] = Math.sin(2 * w);
    }
  }

  public resize(): void {
    const rect = this.canvas.getBoundingClientRect();
    this.dpr = window.devicePixelRatio || 1;
    this.width = Math.max(320, rect.width);
    this.height = Math.max(180, rect.height);
    this.canvas.width = Math.round(this.width * this.dpr);
    this.canvas.height = Math.round(this.height * this.dpr);
    this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    this.render();
  }

  public freqToX(freqHz: number): number {
    const clamped = Math.max(MIN_FREQ, Math.min(MAX_FREQ, freqHz));
    return (Math.log10(clamped / MIN_FREQ) / LOG_FREQ_SPAN) * this.width;
  }

  public xToFreq(x: number): number {
    const norm = Math.max(0, Math.min(1, x / this.width));
    return MIN_FREQ * Math.pow(10, norm * LOG_FREQ_SPAN);
  }

  public dbToY(db: number): number {
    const clamped = Math.max(-MAX_DB, Math.min(MAX_DB, db));
    return (this.height * 0.5) * (1 - clamped / MAX_DB);
  }

  public yToDb(y: number): number {
    const norm = Math.max(0, Math.min(1, y / this.height));
    return MAX_DB * (1 - 2 * norm);
  }

  private computeCoeffs(band: EqBandNode): BiquadCoeffs {
    if (!band.enabled || band.type === 'bypass') {
      return { b0: 1, b1: 0, b2: 0, a1: 0, a2: 0 };
    }

    const f0 = Math.max(10, Math.min(this.sampleRate * 0.495, band.freqHz));
    const q = Math.max(0.1, Math.min(24.0, band.q));
    const db = Math.max(-MAX_DB, Math.min(MAX_DB, band.gainDb));

    const w0 = (2 * Math.PI * f0) / this.sampleRate;
    const cosW0 = Math.cos(w0);
    const sinW0 = Math.sin(w0);
    const alpha = sinW0 / (2 * q);
    const A = Math.pow(10, db / 40);

    let b0 = 1, b1 = 0, b2 = 0, a0 = 1, a1 = 0, a2 = 0;

    switch (band.type) {
      case 'peaking':
        b0 = 1 + alpha * A;
        b1 = -2 * cosW0;
        b2 = 1 - alpha * A;
        a0 = 1 + alpha / A;
        a1 = -2 * cosW0;
        a2 = 1 - alpha / A;
        break;
      case 'lowshelf': {
        const sqA = 2 * Math.sqrt(A) * alpha;
        b0 = A * ((A + 1) - (A - 1) * cosW0 + sqA);
        b1 = 2 * A * ((A - 1) - (A + 1) * cosW0);
        b2 = A * ((A + 1) - (A - 1) * cosW0 - sqA);
        a0 = (A + 1) + (A - 1) * cosW0 + sqA;
        a1 = -2 * ((A - 1) + (A + 1) * cosW0);
        a2 = (A + 1) + (A - 1) * cosW0 - sqA;
        break;
      }
      case 'highshelf': {
        const sqA = 2 * Math.sqrt(A) * alpha;
        b0 = A * ((A + 1) + (A - 1) * cosW0 + sqA);
        b1 = -2 * A * ((A - 1) + (A + 1) * cosW0);
        b2 = A * ((A + 1) + (A - 1) * cosW0 - sqA);
        a0 = (A + 1) - (A - 1) * cosW0 + sqA;
        a1 = 2 * ((A - 1) - (A + 1) * cosW0);
        a2 = (A + 1) - (A - 1) * cosW0 - sqA;
        break;
      }
      case 'highpass':
        b0 = (1 + cosW0) * 0.5;
        b1 = -(1 + cosW0);
        b2 = (1 + cosW0) * 0.5;
        a0 = 1 + alpha;
        a1 = -2 * cosW0;
        a2 = 1 - alpha;
        break;
      case 'lowpass':
        b0 = (1 - cosW0) * 0.5;
        b1 = 1 - cosW0;
        b2 = (1 - cosW0) * 0.5;
        a0 = 1 + alpha;
        a1 = -2 * cosW0;
        a2 = 1 - alpha;
        break;
    }

    const invA0 = 1 / a0;
    return {
      b0: b0 * invA0,
      b1: b1 * invA0,
      b2: b2 * invA0,
      a1: a1 * invA0,
      a2: a2 * invA0,
    };
  }

  private evalBandDbAt(c: BiquadCoeffs, i: number): number {
    const cw = this.evalCosW[i];
    const sw = this.evalSinW[i];
    const c2w = this.evalCos2W[i];
    const s2w = this.evalSin2W[i];

    const numRe = c.b0 + c.b1 * cw + c.b2 * c2w;
    const numIm = -(c.b1 * sw + c.b2 * s2w);
    const denRe = 1.0 + c.a1 * cw + c.a2 * c2w;
    const denIm = -(c.a1 * sw + c.a2 * s2w);

    const magSq = (numRe * numRe + numIm * numIm) / Math.max(1e-18, denRe * denRe + denIm * denIm);
    return 10 * Math.log10(Math.max(1e-12, magSq));
  }

  private traceSmoothSpline(dbValues: Float32Array): void {
    const ctx = this.ctx;
    const n = dbValues.length;
    const stepX = this.width / (n - 1);

    ctx.beginPath();
    ctx.moveTo(0, this.dbToY(dbValues[0]));

    for (let i = 0; i < n - 1; i++) {
      const x0 = Math.max(0, (i - 1) * stepX);
      const y0 = this.dbToY(dbValues[Math.max(0, i - 1)]);
      const x1 = i * stepX;
      const y1 = this.dbToY(dbValues[i]);
      const x2 = (i + 1) * stepX;
      const y2 = this.dbToY(dbValues[i + 1]);
      const x3 = Math.min(this.width, (i + 2) * stepX);
      const y3 = this.dbToY(dbValues[Math.min(n - 1, i + 2)]);

      const cp1x = x1 + (x2 - x0) / 6;
      const cp1y = y1 + (y2 - y0) / 6;
      const cp2x = x2 - (x3 - x1) / 6;
      const cp2y = y2 - (y3 - y1) / 6;

      ctx.bezierCurveTo(cp1x, cp1y, cp2x, cp2y, x2, y2);
    }
  }

  public render(): void {
    const ctx = this.ctx;
    const w = this.width;
    const h = this.height;
    const zeroY = this.dbToY(0);

    ctx.fillStyle = '#0b0f17';
    ctx.fillRect(0, 0, w, h);

    this.renderGrid();

    this.totalDbCurve.fill(0);
    const activeCoeffsList = this.bands.map((b) => this.computeCoeffs(b));

    for (let bIdx = 0; bIdx < this.bands.length; bIdx++) {
      const band = this.bands[bIdx];
      if (!band.enabled) continue;
      const coeffs = activeCoeffsList[bIdx];

      const isSelected = bIdx === this.activeBandIdx || bIdx === this.hoveredBandIdx;
      const bandCurve = isSelected ? new Float32Array(NUM_EVAL_POINTS) : null;

      for (let i = 0; i < NUM_EVAL_POINTS; i++) {
        const db = this.evalBandDbAt(coeffs, i);
        this.totalDbCurve[i] += db;
        if (bandCurve) bandCurve[i] = db;
      }

      if (bandCurve) {
        this.traceSmoothSpline(bandCurve);
        ctx.strokeStyle = band.color;
        ctx.lineWidth = 1.25;
        ctx.stroke();

        ctx.lineTo(w, zeroY);
        ctx.lineTo(0, zeroY);
        ctx.closePath();
        ctx.fillStyle = `${band.color}22`;
        ctx.fill();
      }
    }

    this.traceSmoothSpline(this.totalDbCurve);
    ctx.strokeStyle = '#38bdf8';
    ctx.lineWidth = 2.5;
    ctx.stroke();

    ctx.lineTo(w, zeroY);
    ctx.lineTo(0, zeroY);
    ctx.closePath();
    const grad = ctx.createLinearGradient(0, 0, 0, h);
    grad.addColorStop(0, 'rgba(56, 189, 248, 0.28)');
    grad.addColorStop(0.5, 'rgba(56, 189, 248, 0.03)');
    grad.addColorStop(1, 'rgba(56, 189, 248, 0.28)');
    ctx.fillStyle = grad;
    ctx.fill();

    for (let i = 0; i < this.bands.length; i++) {
      const b = this.bands[i];
      const cx = this.freqToX(b.freqHz);
      const cy = this.dbToY(b.type === 'highpass' || b.type === 'lowpass' ? 0 : b.gainDb);
      const isFocused = i === this.activeBandIdx || i === this.hoveredBandIdx;

      if (isFocused) {
        ctx.beginPath();
        ctx.arc(cx, cy, 13, 0, Math.PI * 2);
        ctx.fillStyle = `${b.color}33`;
        ctx.fill();
      }

      ctx.beginPath();
      ctx.arc(cx, cy, isFocused ? 7.5 : 6, 0, Math.PI * 2);
      ctx.fillStyle = b.enabled ? b.color : '#30363d';
      ctx.fill();
      ctx.lineWidth = 2;
      ctx.strokeStyle = isFocused ? '#ffffff' : '#0b0f17';
      ctx.stroke();
    }
  }

  private renderGrid(): void {
    const ctx = this.ctx;
    const freqMarkers = [20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000];
    const dbMarkers = [-18, -12, -6, 0, 6, 12, 18];

    ctx.font = '10px Inter, system-ui, sans-serif';
    ctx.textAlign = 'center';

    for (const f of freqMarkers) {
      const x = this.freqToX(f);
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, this.height);
      ctx.strokeStyle = '#1f2937';
      ctx.lineWidth = 1;
      ctx.stroke();

      const label = f >= 1000 ? `${f / 1000}k` : `${f}`;
      ctx.fillStyle = '#6b7280';
      ctx.fillText(label, x, this.height - 6);
    }

    ctx.textAlign = 'left';
    for (const db of dbMarkers) {
      const y = this.dbToY(db);
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(this.width, y);
      ctx.strokeStyle = db === 0 ? '#374151' : '#1f2937';
      ctx.lineWidth = db === 0 ? 1.5 : 1;
      ctx.stroke();

      ctx.fillStyle = '#6b7280';
      ctx.fillText(`${db > 0 ? '+' : ''}${db} dB`, 6, y - 4);
    }
  }

  private hitTestNode(x: number, y: number): number {
    const hitRadiusSq = 14 * 14;
    for (let i = this.bands.length - 1; i >= 0; i--) {
      const b = this.bands[i];
      const nx = this.freqToX(b.freqHz);
      const ny = this.dbToY(b.type === 'highpass' || b.type === 'lowpass' ? 0 : b.gainDb);
      const dx = x - nx;
      const dy = y - ny;
      if (dx * dx + dy * dy <= hitRadiusSq) return i;
    }
    return -1;
  }

  private bindEvents(): void {
    this.canvas.addEventListener('pointerdown', (e: PointerEvent) => {
      const rect = this.canvas.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const y = e.clientY - rect.top;
      const idx = this.hitTestNode(x, y);

      if (idx !== -1) {
        this.activeBandIdx = idx;
        this.isDragging = true;
        this.canvas.setPointerCapture(e.pointerId);
        this.onBandChange(this.bands[idx]);
        this.render();
      }
    });

    this.canvas.addEventListener('pointermove', (e: PointerEvent) => {
      const rect = this.canvas.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const y = e.clientY - rect.top;

      if (this.isDragging && this.activeBandIdx !== -1) {
        const band = this.bands[this.activeBandIdx];
        band.freqHz = Math.round(this.xToFreq(x) * 10) / 10;
        if (band.type !== 'highpass' && band.type !== 'lowpass') {
          band.gainDb = Math.round(this.yToDb(y) * 100) / 100;
        }
        this.onBandChange(band);
        this.render();
      } else {
        const hovered = this.hitTestNode(x, y);
        if (hovered !== this.hoveredBandIdx) {
          this.hoveredBandIdx = hovered;
          this.canvas.style.cursor = hovered !== -1 ? 'grab' : 'default';
          this.render();
        }
      }
    });

    this.canvas.addEventListener('pointerup', (e: PointerEvent) => {
      if (this.isDragging) {
        this.isDragging = false;
        this.canvas.releasePointerCapture(e.pointerId);
        this.render();
      }
    });

    this.canvas.addEventListener(
      'wheel',
      (e: WheelEvent) => {
        const targetIdx = this.activeBandIdx !== -1 ? this.activeBandIdx : this.hoveredBandIdx;
        if (targetIdx === -1) return;
        e.preventDefault();

        const band = this.bands[targetIdx];
        const factor = e.deltaY < 0 ? 1.08 : 1 / 1.08;
        band.q = Math.max(0.1, Math.min(24.0, Math.round(band.q * factor * 100) / 100));
        this.onBandChange(band);
        this.render();
      },
      { passive: false }
    );

    this.canvas.addEventListener('dblclick', (e: MouseEvent) => {
      const rect = this.canvas.getBoundingClientRect();
      const idx = this.hitTestNode(e.clientX - rect.left, e.clientY - rect.top);
      if (idx !== -1) {
        this.bands[idx].gainDb = 0.0;
        this.onBandChange(this.bands[idx]);
        this.render();
      }
    });
  }
}
