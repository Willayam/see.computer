import React, { useEffect, useRef } from 'react';
import { createRoot } from 'react-dom/client';
import { DialRoot, useDialKit } from 'dialkit';
import 'dialkit/styles.css';

/* ---------------- audio + spectrum ---------------- */
let micAnalyser = null, micData = null, micFreq = null, micSampleRate = 48000;
async function enableMic() {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const ac = new AudioContext(); const src = ac.createMediaStreamSource(stream);
  micSampleRate = ac.sampleRate;
  micAnalyser = ac.createAnalyser(); micAnalyser.fftSize = 1024;
  micAnalyser.smoothingTimeConstant = 0.5;
  src.connect(micAnalyser);
  micData = new Float32Array(micAnalyser.fftSize);
  micFreq = new Float32Array(micAnalyser.frequencyBinCount);
}
function micLevel() {
  micAnalyser.getFloatTimeDomainData(micData);
  let sum = 0, peak = 0;
  for (const v of micData) { sum += v * v; peak = Math.max(peak, Math.abs(v)); }
  return { avg: Math.min(1, Math.sqrt(sum / micData.length) * 6), peak: Math.min(1, peak * 3) };
}
function fakeLevel(t) {
  const phrase = (t % 7); let env = 0;
  if (phrase < 4.4) env = Math.max(0.06, Math.max(0, Math.sin(t * 7.3) * 0.6 + Math.sin(t * 13.1) * 0.4) * (0.55 + 0.45 * Math.sin(t * 1.1)));
  else if (phrase < 4.9) env = 0.03;
  else if (phrase < 6.2) env = Math.max(0.05, Math.abs(Math.sin(t * 9.7)) * 0.7);
  else env = 0.02;
  const level = Math.min(1, env * (0.85 + 0.3 * Math.sin(t * 31)));
  return { avg: level, peak: Math.min(1, level * 1.5 + 0.08 * Math.abs(Math.sin(t * 47))) };
}
const N_BANDS = 24;
const gauss = (x, c, w) => Math.exp(-((x - c) * (x - c)) / (2 * w * w));
function micBands() {
  micAnalyser.getFloatFrequencyData(micFreq);
  const nyq = micSampleRate / 2, bins = micFreq.length;
  const fLo = 85, fHi = 8000, out = new Array(N_BANDS).fill(0);
  for (let b = 0; b < N_BANDS; b++) {
    const f0 = fLo * Math.pow(fHi / fLo, b / N_BANDS);
    const f1 = fLo * Math.pow(fHi / fLo, (b + 1) / N_BANDS);
    const i0 = Math.max(0, Math.floor(f0 / nyq * bins));
    const i1 = Math.min(bins - 1, Math.max(i0 + 1, Math.ceil(f1 / nyq * bins)));
    let sum = 0;
    for (let i = i0; i < i1; i++) sum += micFreq[i];
    out[b] = Math.pow(Math.max(0, Math.min(1, (sum / (i1 - i0) + 78) / 48)), 1.4);
  }
  return out;
}
function fakeBands(t) {
  const env = fakeLevel(t).avg;
  const c1 = 0.14 + 0.06 * Math.sin(t * 1.7);
  const c2 = 0.38 + 0.13 * Math.sin(t * 0.9 + 1.2);
  const c3 = 0.62 + 0.1 * Math.sin(t * 1.3 + 2.4);
  const out = new Array(N_BANDS);
  for (let b = 0; b < N_BANDS; b++) {
    const x = b / (N_BANDS - 1);
    const shape = 0.45 * Math.exp(-x * 2.4)
      + 0.9 * gauss(x, c1, 0.07) + 0.7 * gauss(x, c2, 0.1) + 0.35 * gauss(x, c3, 0.09);
    out[b] = Math.min(1, env * shape * (0.72 + 0.28 * Math.sin(t * 12 + b * 2.3)) * 1.5);
  }
  return out;
}
/* ---------------- springs ---------------- */
function makeSpring(v0) { return { x: v0, v: 0 }; }
function stepSpring(s, target, dt, response, damping) {
  const K = Math.pow(2 * Math.PI / Math.max(0.03, response), 2);
  const C = 2 * damping * Math.sqrt(K);
  const h = 1 / 240; let r = Math.min(dt, 0.1);
  if (!isFinite(s.x) || !isFinite(s.v)) { s.x = target; s.v = 0; }
  while (r > 0) { const st = Math.min(h, r);
    s.v += (K * (target - s.x) - C * s.v) * st; s.x += s.v * st; r -= st; }
  return s.x;
}
/* ---------------- color helpers ---------------- */
function hexRgb(hx) {
  const v = parseInt(hx.slice(1), 16);
  return [(v >> 16) & 255, (v >> 8) & 255, v & 255];
}
function mixc(a, b, f) { f = Math.max(0, Math.min(1, f));
  return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]; }
const css = (c, a = 1) => `rgba(${c[0] | 0},${c[1] | 0},${c[2] | 0},${a})`;
const WHITE = [255, 255, 255], BLACKC = [0, 0, 0];
/* ---------------- shared render state ---------------- */
const S = {
  m: makeSpring(0), p: makeSpring(0), sc: makeSpring(1), w: makeSpring(56),
  sampleAt: 0, mT: 0, pT: 0, silentFor: 0, t0: performance.now(), c: {},
  bands: new Float32Array(N_BANDS),
};
/* ---------------- capsule + shell + blocks ---------------- */
function hexCapsule(ctx, cx, cy, w, h, accent, avg, peak, cfg, shineT) {
  const r = h / 2;
  const bg = mixc(mixc(accent, BLACKC, 0.5), accent, avg);
  ctx.save();
  ctx.shadowColor = css(accent, avg); ctx.shadowBlur = 4 * cfg.glow.outer;
  ctx.fillStyle = css(bg);
  ctx.beginPath(); ctx.roundRect(cx - w / 2, cy - h / 2, w, h, r); ctx.fill();
  ctx.shadowBlur = 8 * cfg.glow.outer; ctx.shadowColor = css(accent, avg * 0.5);
  ctx.beginPath(); ctx.roundRect(cx - w / 2, cy - h / 2, w, h, r); ctx.fill();
  ctx.restore();
  ctx.save(); ctx.beginPath(); ctx.roundRect(cx - w / 2, cy - h / 2, w, h, r); ctx.clip();
  ctx.globalCompositeOperation = 'screen';
  const aGlow = (avg < 0.1 ? avg / 0.1 : 1) * cfg.glow.inner;
  let g = ctx.createRadialGradient(cx, cy, 1, cx, cy, Math.max(4, w / 2 - 4));
  g.addColorStop(0, css(accent, Math.min(1, aGlow * 0.9))); g.addColorStop(1, css(accent, 0));
  ctx.fillStyle = g; ctx.fillRect(cx - w / 2, cy - h / 2, w, h);
  const aCore = (avg < 0.1 ? avg / 0.1 * 0.5 : 0.5) * cfg.glow.inner;
  g = ctx.createRadialGradient(cx, cy, 0.5, cx, cy, Math.max(3, w * 0.2));
  g.addColorStop(0, `rgba(255,255,255,${Math.min(1, aCore)})`); g.addColorStop(1, 'rgba(255,255,255,0)');
  ctx.fillStyle = g; ctx.fillRect(cx - w / 2, cy - h / 2, w, h);
  const pw = w * (peak + 0.6), aPeak = peak < 0.1 ? peak / 0.1 * 0.5 : 0.5;
  g = ctx.createRadialGradient(cx, cy, 0.5, cx, cy, pw / 2);
  g.addColorStop(0, css(accent, aPeak)); g.addColorStop(1, css(accent, 0));
  ctx.fillStyle = g; ctx.fillRect(cx - pw / 2, cy - h / 2 + 2, pw, h - 4);
  if (shineT !== undefined) {
    const sx = cx - w / 2 - 20 + ((shineT * (w + 40) * 1.4) % (w + 40));
    g = ctx.createLinearGradient(sx, 0, sx + 22, 0);
    g.addColorStop(0, 'rgba(255,255,255,0)'); g.addColorStop(0.5, 'rgba(255,255,255,0.5)'); g.addColorStop(1, 'rgba(255,255,255,0)');
    ctx.fillStyle = g; ctx.fillRect(cx - w / 2, cy - h / 2, w, h);
  }
  ctx.restore();
  ctx.strokeStyle = css(mixc(accent, WHITE, 0.1), 0.6); ctx.lineWidth = 1;
  ctx.beginPath(); ctx.roundRect(cx - w / 2, cy - h / 2, w, h, r); ctx.stroke();
}
function bodyShell(ctx, cx, cy, w, h, r) {
  ctx.save();
  ctx.shadowColor = 'rgba(0,0,0,0.5)'; ctx.shadowBlur = 10; ctx.shadowOffsetY = 3;
  ctx.fillStyle = 'rgba(13,14,18,0.97)';
  ctx.beginPath(); ctx.roundRect(cx - w / 2, cy - h / 2, w, h, r); ctx.fill();
  ctx.restore();
  ctx.strokeStyle = 'rgba(255,255,255,0.16)'; ctx.lineWidth = 1;
  ctx.beginPath(); ctx.roundRect(cx - w / 2, cy - h / 2, w, h, r); ctx.stroke();
}
function glowBlock(ctx, x, y, w, h, color, intensity, peak = 0, r = 2) {
  ctx.fillStyle = color; ctx.globalAlpha = 0.16 + 0.1 * intensity;
  ctx.beginPath(); ctx.roundRect(x, y, w, h, r); ctx.fill();
  if (intensity > 0.04) {
    ctx.globalAlpha = Math.min(1, 0.25 + intensity);
    ctx.shadowColor = color; ctx.shadowBlur = 5 + 9 * intensity;
    ctx.beginPath(); ctx.roundRect(x, y, w, h, r); ctx.fill();
    ctx.shadowBlur = 0;
    if (peak > 0.06) {
      ctx.fillStyle = `rgba(255,255,255,${0.35 * peak + 0.25 * intensity})`;
      ctx.beginPath(); ctx.roundRect(x + w * 0.24, y + h * 0.24, w * 0.52, h * 0.52, r * 0.7); ctx.fill();
    }
  }
  ctx.globalAlpha = 1;
}
/* ---------------- variants ---------------- */
function bandsFor(n, gain) {
  const out = new Array(n);
  for (let i = 0; i < n; i++) {
    const a = Math.floor(i / n * N_BANDS), z = Math.max(a + 1, Math.ceil((i + 1) / n * N_BANDS));
    let m = 0;
    for (let j = a; j < z; j++) m = Math.max(m, S.bands[j]);
    out[i] = Math.min(1, m * gain);
  }
  return out;
}
/* tilt-compensated, optionally mirrored bands: speech energy is left-heavy,
   so boost highs by `tilt`; mirror puts low frequencies at the center */
function balancedBands(n, gain, tilt, mirror) {
  const base = bandsFor(n, gain);
  const comp = base.map((v, i) => Math.min(1, v * (0.7 + tilt * (i / Math.max(1, n - 1)))));
  if (!mirror) return comp;
  const out = new Array(n);
  const mid = (n - 1) / 2;
  for (let j = 0; j < n; j++) {
    const u = Math.abs(j - mid) / mid;
    out[j] = comp[Math.min(n - 1, Math.round(u * (n - 1)))];
  }
  return out;
}
const recPulse = (t, hz) => 0.55 + 0.45 * Math.sin(t * Math.PI * 2 * hz);
const VARIANTS = {
  eyeline(ctx, cx, cy, t, d, cfg) {
    const W = cfg.chip.w, H = cfg.chip.h;
    const rec = d.status === 'record';
    bodyShell(ctx, cx, cy, W, H, Math.min(12, H / 2));
    const pad = 10, innerW = W - pad * 2;
    const n = Math.round(cfg.line.bars);
    const gap = 2, bw = Math.max(1.5, (innerW - (n - 1) * gap) / n);
    const x0 = cx - innerW / 2;
    const lineTh = cfg.line.thickness;
    const CYc = hexRgb(cfg.colors.listen), RDc = hexRgb(cfg.colors.record), AMc = hexRgb(cfg.colors.warn);
    const accent = d.warn ? AMc : rec ? RDc : CYc;
    const col = css(accent);
    ctx.save();
    ctx.shadowColor = col; ctx.shadowBlur = 6 * cfg.glow.outer;
    /* the baseline: silence is just a line */
    ctx.fillStyle = css(accent, d.warn ? 0.55 : 0.4);
    ctx.beginPath(); ctx.roundRect(x0, cy - lineTh / 2, innerW, lineTh, lineTh / 2); ctx.fill();
    if (d.status === 'armed' || d.status === 'hidden') { ctx.restore(); return; }
    if (d.warn) {
      const p = (t * 0.55) % 1;
      const hx = x0 + p * innerW;
      const g = ctx.createLinearGradient(hx - 22, 0, hx + 22, 0);
      g.addColorStop(0, css(AMc, 0)); g.addColorStop(0.5, css(AMc, 1)); g.addColorStop(1, css(AMc, 0));
      ctx.fillStyle = g;
      ctx.beginPath(); ctx.roundRect(x0, cy - lineTh / 2 - 0.5, innerW, lineTh + 1, lineTh / 2); ctx.fill();
      ctx.restore(); return;
    }
    if (d.status === 'transcribe' || d.status === 'save') {
      const speed = d.status === 'save' ? 2.6 : 1.1;
      for (let i = 0; i < n; i++) {
        const u = i / (n - 1);
        const wv = Math.max(0, Math.sin((u - t * speed) * Math.PI * 2)) * 0.5;
        if (wv < 0.05) continue;
        const bh = lineTh + wv * (H - 14 - lineTh) * 0.4;
        ctx.fillStyle = css(accent, 0.35 + wv);
        ctx.beginPath(); ctx.roundRect(x0 + i * (bw + gap), cy - bh / 2, bw, bh, bw / 2); ctx.fill();
      }
      ctx.restore(); return;
    }
    /* listening or recording: bars fill in from the line, balanced */
    const bands = balancedBands(n, cfg.bands.gain, cfg.line.tilt, cfg.line.mirror);
    const blinkP = (t + 0.9) % cfg.eye.blinkEvery;
    const blink = rec && blinkP < 0.14;
    const maxH = H - 12;
    for (let i = 0; i < n; i++) {
      const u = n === 1 ? 0.5 : i / (n - 1);
      let v = bands[i];
      let alpha = 0.9;
      if (rec) {
        let env = Math.pow(Math.sin(Math.PI * u), 0.8);
        const inPupil = Math.abs(u - 0.5) < cfg.eye.pupil / 2;
        if (inPupil) env *= 0.12;
        const shaped = (0.3 + 0.7 * v) * env;
        v = v * (1 - cfg.eye.envelope) + shaped * cfg.eye.envelope;
        if (inPupil) alpha = 1;
        if (blink) v = 0;
      }
      const bh = lineTh + v * (maxH - lineTh);
      if (bh <= lineTh + 0.4) continue;
      ctx.fillStyle = css(accent, alpha * (0.45 + 0.55 * Math.min(1, v * 1.4)));
      ctx.beginPath(); ctx.roundRect(x0 + i * (bw + gap), cy - bh / 2, bw, bh, Math.min(bw / 2, 2)); ctx.fill();
    }
    if (rec && !blink) {
      ctx.fillStyle = `rgba(255,255,255,${0.35 + 0.25 * recPulse(t, cfg.recPulseHz)})`;
      const pw = Math.max(2, cfg.eye.pupil * innerW * 0.3);
      ctx.beginPath(); ctx.roundRect(cx - pw / 2, cy - lineTh, pw, lineTh * 2, lineTh); ctx.fill();
    }
    ctx.restore();
  },
  hex(ctx, cx, cy, t, d, cfg) {
    const RED = hexRgb(cfg.colors.record), BLUE = [10, 132, 255];
    if (d.warn) { hexCapsule(ctx, cx, cy, cfg.capsule.idleW, cfg.capsule.height, RED, 0.05, 0, cfg); return; }
    if (d.status === 'transcribe' || d.status === 'save') hexCapsule(ctx, cx, cy, d.w, cfg.capsule.height, BLUE, 0.3, 0.1, cfg, t);
    else if (d.status === 'listen' || d.status === 'record') hexCapsule(ctx, cx, cy, d.w, cfg.capsule.height, RED, d.avg, d.peak, cfg);
    else hexCapsule(ctx, cx, cy, cfg.capsule.idleW, cfg.capsule.height, BLACKC, 0, 0, cfg);
  },
  palette(ctx, cx, cy, t, d, cfg) {
    const CY = hexRgb(cfg.colors.listen), RD = hexRgb(cfg.colors.record), AM = hexRgb(cfg.colors.warn);
    if (d.warn) { hexCapsule(ctx, cx, cy, cfg.capsule.idleW, cfg.capsule.height, AM, 0.25 + 0.2 * Math.sin(t * 2.5), 0, cfg); return; }
    if (d.status === 'transcribe' || d.status === 'save') hexCapsule(ctx, cx, cy, d.w, cfg.capsule.height, CY, 0.3, 0.1, cfg, t);
    else if (d.status === 'record') hexCapsule(ctx, cx, cy, d.w, cfg.capsule.height, RD, Math.max(d.avg, 0.3 * recPulse(t, cfg.recPulseHz)), d.peak, cfg);
    else if (d.status === 'listen') hexCapsule(ctx, cx, cy, d.w, cfg.capsule.height, CY, d.avg, d.peak, cfg);
    else hexCapsule(ctx, cx, cy, cfg.capsule.idleW, cfg.capsule.height, BLACKC, 0, 0, cfg);
  },
  pupil(ctx, cx, cy, t, d, cfg) {
    VARIANTS.palette(ctx, cx, cy, t, d, cfg);
    if (d.status !== 'listen' && d.status !== 'record') {
      if (d.status === 'transcribe') { ctx.fillStyle = 'rgba(10,10,12,0.85)';
        ctx.beginPath(); ctx.roundRect(cx - 4, cy - 1, 8, 2, 1); ctx.fill(); }
      return;
    }
    if (d.warn) { const lx = Math.sin(t * 2.2) * 3.5;
      ctx.fillStyle = 'rgba(10,10,12,0.85)';
      ctx.beginPath(); ctx.arc(cx + lx, cy, 2.6, 0, 7); ctx.fill(); return; }
    const blink = ((t + 0.8) % 4.2) < 0.12;
    if (blink) { ctx.fillStyle = 'rgba(10,10,12,0.85)';
      ctx.beginPath(); ctx.roundRect(cx - 4.5, cy - 1, 9, 2, 1); ctx.fill(); return; }
    const pr = cfg.pupil.min + (cfg.pupil.max - cfg.pupil.min) * Math.min(1, d.avg * 1.2);
    ctx.fillStyle = 'rgba(10,10,12,0.88)';
    ctx.beginPath(); ctx.arc(cx, cy, pr, 0, 7); ctx.fill();
    ctx.fillStyle = 'rgba(255,255,255,0.75)';
    ctx.beginPath(); ctx.arc(cx - pr * 0.35, cy - pr * 0.35, Math.max(0.5, pr * 0.22), 0, 7); ctx.fill();
  },
  jelly(ctx, cx, cy, t, d, cfg) {
    S.c.wob = (S.c.wob ?? 0) + (((d.status === 'listen' || d.status === 'record') && !d.warn ? S.mT : 0) - (S.c.wob ?? 0)) * 0.12;
    S.c.sag = (S.c.sag ?? 0) + ((d.warn ? 1 : 0) - (S.c.sag ?? 0)) * 0.06;
    const breathe = d.status === 'transcribe' ? Math.sin(t * 2.6) * 0.06 : 0;
    const jump = d.status === 'save' ? Math.abs(Math.sin(t * 3.4)) * 5 : 0;
    const sx2 = 1 + S.c.wob * 0.55 + breathe - S.c.sag * 0.12;
    const sy2 = 1 - S.c.wob * 0.3 - breathe - S.c.sag * 0.28;
    const W = 52 * sx2, H = 22 * sy2;
    ctx.save(); ctx.translate(cx, cy + S.c.sag * 4 - jump); ctx.rotate(S.c.sag * 0.16);
    ctx.shadowColor = 'rgba(0,0,0,0.35)'; ctx.shadowBlur = 8; ctx.shadowOffsetY = 4;
    ctx.fillStyle = '#efece6';
    ctx.beginPath(); ctx.roundRect(-W / 2, -H / 2, W, H, H / 2); ctx.fill();
    ctx.shadowBlur = 0; ctx.shadowOffsetY = 0;
    ctx.strokeStyle = 'rgba(0,0,0,0.22)'; ctx.lineWidth = 1;
    ctx.beginPath(); ctx.roundRect(-W / 2, -H / 2, W, H, H / 2); ctx.stroke();
    const blushA = d.status === 'record' ? 0.55 + 0.4 * recPulse(t, cfg.recPulseHz) : d.warn ? 0.55 : 0;
    if (blushA > 0.02) {
      const bc = d.warn ? hexRgb(cfg.colors.warn) : hexRgb(cfg.colors.record);
      const g = ctx.createRadialGradient(0, 0, 1, 0, 0, W * 0.42);
      g.addColorStop(0, css(bc, blushA)); g.addColorStop(1, css(bc, 0));
      ctx.fillStyle = g;
      ctx.beginPath(); ctx.roundRect(-W / 2, -H / 2, W, H, H / 2); ctx.fill();
    }
    ctx.fillStyle = '#2b2b2e';
    const blink = ((t + 0.8) % 3.9) < 0.15 && d.status === 'listen' && !d.warn;
    const eh = blink || d.status === 'transcribe' ? 1.2 : 4;
    ctx.beginPath(); ctx.roundRect(-7.5, -H * 0.08 - eh / 2, 2.3, eh, 1.1); ctx.fill();
    ctx.beginPath(); ctx.roundRect(5.2, -H * 0.08 - eh / 2, 2.3, eh, 1.1); ctx.fill();
    ctx.restore();
  },
  plasma(ctx, cx, cy, t, d, cfg) {
    const accent = d.warn ? hexRgb(cfg.colors.warn) : d.status === 'record' ? hexRgb(cfg.colors.record) : hexRgb(cfg.colors.listen);
    const col = css(accent);
    const lv = d.status === 'transcribe' ? 0.3 + 0.12 * Math.sin(t * 4) : Math.max(0.12, d.avg);
    bodyShell(ctx, cx, cy, 64, 22, 11);
    ctx.save(); ctx.beginPath(); ctx.roundRect(cx - 32, cy - 11, 64, 22, 11); ctx.clip();
    if (d.warn) {
      ctx.setLineDash([3, 3]); ctx.lineDashOffset = -t * 10; ctx.strokeStyle = col; ctx.lineWidth = 1.4;
      ctx.beginPath(); ctx.arc(cx, cy, 6, 0, 7); ctx.stroke(); ctx.setLineDash([]);
    } else {
      const heart = d.status === 'record' ? 0.7 + 0.45 * recPulse(t, cfg.recPulseHz) : 1;
      const base = (4 + 5.5 * lv) * heart;
      ctx.shadowColor = col; ctx.shadowBlur = (12 * lv * heart + 3) * cfg.glow.outer;
      ctx.beginPath();
      for (let i = 0; i <= 30; i++) {
        const th = (i / 30) * Math.PI * 2;
        const wob = (Math.sin(th * 3 + t * 8) + Math.sin(th * 5 - t * 6)) * 1.4 * lv;
        ctx.lineTo(cx + Math.cos(th) * (base + wob) * 2.1, cy + Math.sin(th) * (base + wob) * 0.8);
      }
      ctx.closePath();
      const g = ctx.createRadialGradient(cx, cy, 0.5, cx, cy, base * 2);
      g.addColorStop(0, 'rgba(255,250,240,0.9)'); g.addColorStop(0.5, col); g.addColorStop(1, 'rgba(0,0,0,0)');
      ctx.fillStyle = g; ctx.fill();
      if (d.status === 'save') { const p = (t * 1.6) % 1; ctx.fillStyle = 'rgba(255,255,255,0.9)';
        ctx.fillRect(cx - 32 + p * 64 - 2, cy - 11, 3, 22); }
    }
    ctx.restore();
  },
  vfdface(ctx, cx, cy, t, d, cfg) {
    const accent = d.warn ? hexRgb(cfg.colors.warn) : d.status === 'record' ? hexRgb(cfg.colors.record) : [158, 243, 255];
    const col = css(accent);
    bodyShell(ctx, cx, cy, 56, 22, 11);
    ctx.save(); ctx.shadowColor = col; ctx.shadowBlur = 6 * cfg.glow.outer; ctx.strokeStyle = col; ctx.fillStyle = col;
    ctx.lineWidth = 2; ctx.lineCap = 'round';
    const blink = ((t + 0.6) % 3.9) < 0.16 && d.status === 'listen' && !d.warn;
    const ey = cy - 3.5;
    for (const ex of [cx - 9, cx + 9]) {
      if (d.warn) { ctx.beginPath(); ctx.moveTo(ex - 2.6, ey - 2.6); ctx.lineTo(ex + 2.6, ey + 2.6);
        ctx.moveTo(ex + 2.6, ey - 2.6); ctx.lineTo(ex - 2.6, ey + 2.6); ctx.stroke(); }
      else if (d.status === 'transcribe' || blink) { ctx.beginPath(); ctx.moveTo(ex - 3, ey + 1); ctx.lineTo(ex + 3, ey + 1); ctx.stroke(); }
      else if (d.status === 'save') { ctx.beginPath(); ctx.moveTo(ex - 3, ey + 1.5); ctx.quadraticCurveTo(ex, ey - 2.5, ex + 3, ey + 1.5); ctx.stroke(); }
      else { ctx.globalAlpha = d.status === 'record' ? 0.55 + 0.45 * recPulse(t, cfg.recPulseHz) : 1; ctx.fillRect(ex - 1.8, ey - 3, 3.6, 6.4); ctx.globalAlpha = 1; }
    }
    const mv = d.warn ? 0 : d.status === 'listen' || d.status === 'record' ? d.avg : d.status === 'save' ? 0.5 : 0.12;
    if (d.warn) { ctx.beginPath(); ctx.moveTo(cx - 3.5, cy + 5.5); ctx.lineTo(cx + 3.5, cy + 5.5); ctx.stroke(); }
    else { const mw = 7 + mv * 14, mh = 1.6 + mv * 6;
      ctx.beginPath(); ctx.roundRect(cx - mw / 2, cy + 5 - mh / 2, mw, mh, 1.5); ctx.fill(); }
    ctx.restore();
  },
  vfdbar(ctx, cx, cy, t, d, cfg) {
    const accent = d.warn ? hexRgb(cfg.colors.warn) : d.status === 'record' ? [255, 171, 74] : [158, 243, 255];
    const col = css(accent);
    bodyShell(ctx, cx, cy, 84, 20, 10);
    const n = Math.round(cfg.bands.bars), bw = Math.max(2, (66 - (n - 1) * 2.4) / n), gap = 2.4;
    const x0 = cx - (n * (bw + gap) - gap) / 2;
    const bands = bandsFor(n, cfg.bands.gain);
    ctx.save(); ctx.shadowColor = col; ctx.shadowBlur = 6 * cfg.glow.outer;
    for (let i = 0; i < n; i++) {
      if (d.warn) { const on = Math.floor(t * 6) % n === i;
        ctx.fillStyle = on ? col : 'rgba(255,179,64,0.14)';
        ctx.fillRect(x0 + i * (bw + gap), cy - 5, bw, 10); continue; }
      if (d.status === 'transcribe') { const c2 = (t * 10) % (2 * n); const k = c2 < n ? c2 : 2 * n - c2;
        ctx.fillStyle = Math.abs(i - k) < 1.4 ? col : 'rgba(158,243,255,0.14)';
        ctx.fillRect(x0 + i * (bw + gap), cy - 5, bw, 10); continue; }
      if (d.status === 'save') { ctx.fillStyle = i < ((t * 14) % (n + 3)) ? col : 'rgba(158,243,255,0.14)';
        ctx.fillRect(x0 + i * (bw + gap), cy - 5, bw, 10); continue; }
      const v = bands[i];
      const hgt = Math.max(2.5, v * 15);
      ctx.fillStyle = v < 0.03 ? 'rgba(158,243,255,0.16)' : col;
      ctx.fillRect(x0 + i * (bw + gap), cy + 2.5 - hgt / 2, bw, Math.min(hgt, 14));
    }
    if (d.status === 'record' && !d.warn) {
      const frac = (d.elapsed % 60) / 60;
      for (let i = 0; i < n; i++) { ctx.fillStyle = i / n < frac ? css(hexRgb(cfg.colors.record)) : 'rgba(255,69,58,0.18)';
        ctx.fillRect(x0 + i * (bw + gap), cy - 7.5, bw, 1.8); }
    }
    ctx.restore();
  },
  eyemachine(ctx, cx, cy, t, d, cfg) {
    const px = 5, gap = 1.5;
    const CY = hexRgb(cfg.colors.listen), RD = hexRgb(cfg.colors.record), AM = hexRgb(cfg.colors.warn);
    if (d.status !== 'record') {
      const cols = 13, rows = 5;
      const W = cols * (px + gap) - gap + 14, H = rows * (px + gap) - gap + 12;
      bodyShell(ctx, cx, cy, W, H, 9);
      const x0 = cx - (cols * (px + gap) - gap) / 2, y0 = cy - (rows * (px + gap) - gap) / 2;
      if (d.warn) {
        const pos = Math.round((Math.sin(t * 2.6) * 0.5 + 0.5) * (cols - 1));
        for (let i = 0; i < cols; i++) {
          const dd = Math.abs(i - pos), inten = dd === 0 ? 0.9 : dd === 1 ? 0.3 : 0;
          glowBlock(ctx, x0 + i * (px + gap), y0 + 2 * (px + gap), px, px, css(AM), inten, 0);
        }
        return;
      }
      if (d.status === 'transcribe' || d.status === 'save') {
        for (let i = 0; i < cols; i++) {
          const ph = d.status === 'save' ? (i / cols < ((t * 1.6) % 1) ? 1 : 0.05)
            : 0.25 + 0.75 * Math.max(0, Math.sin(t * 5 - i * 0.55));
          glowBlock(ctx, x0 + i * (px + gap), y0 + 2 * (px + gap), px, px, css(CY), ph * 0.7, 0);
        }
        return;
      }
      const bands = bandsFor(cols, cfg.bands.gain);
      for (let i = 0; i < cols; i++) {
        const v = bands[i];
        const lit = Math.round(v * rows * 1.15);
        for (let j = 0; j < rows; j++) {
          const fromMid = Math.abs(j - 2), on = fromMid < lit / 2 + 0.26;
          const X = x0 + i * (px + gap), Y = y0 + j * (px + gap);
          if (on) glowBlock(ctx, X, Y, px, px, css(CY), Math.min(1, 0.45 + v), v > 0.85 ? v : 0);
          else { ctx.fillStyle = 'rgba(255,255,255,0.05)'; ctx.beginPath(); ctx.roundRect(X, Y, px, px, 2); ctx.fill(); }
        }
      }
      return;
    }
    const cols = 9, rows = 5;
    const W = cols * (px + gap) - gap + 14, H = rows * (px + gap) - gap + 12;
    const grow = 1 + 0.05 * recPulse(t, cfg.recPulseHz);
    ctx.save(); ctx.translate(cx, cy); ctx.scale(grow, grow); ctx.translate(-cx, -cy);
    bodyShell(ctx, cx, cy, W, H, 9);
    const x0 = cx - (cols * (px + gap) - gap) / 2, y0 = cy - (rows * (px + gap) - gap) / 2;
    if (d.warn) {
      const pos = Math.round((Math.sin(t * 2.6) * 0.5 + 0.5) * (cols - 1));
      for (let i = 0; i < cols; i++) {
        const dd = Math.abs(i - pos), inten = dd === 0 ? 0.9 : dd === 1 ? 0.3 : 0;
        glowBlock(ctx, x0 + i * (px + gap), cy - px / 2, px, px, css(AM), inten, 0);
      }
      ctx.restore(); return;
    }
    const shape = [[0,0,1,1,1,1,1,0,0],[0,1,1,1,1,1,1,1,0],[1,1,1,1,1,1,1,1,1],[0,1,1,1,1,1,1,1,0],[0,0,1,1,1,1,1,0,0]];
    const openT = ((t + 0.8) % 3.6) < 0.22 ? Math.abs(Math.sin((((t + 0.8) % 3.6) / 0.22) * Math.PI)) : 1;
    const pupil = 1 + Math.round(Math.min(1, d.avg * 1.6 + 0.15) * 1.4) * 2, half = (pupil - 1) / 2;
    for (let j = 0; j < rows; j++) {
      const rowVisible = openT > Math.abs(j - 2) / 2.6;
      for (let i = 0; i < cols; i++) {
        if (!shape[j][i]) continue;
        const X = x0 + i * (px + gap), Y = y0 + j * (px + gap);
        if (!rowVisible) { if (j === 2) glowBlock(ctx, X, Y, px, px, css(RD), 0.5, 0); continue; }
        const isPupil = Math.abs(i - 4) <= half && Math.abs(j - 2) <= half;
        const isIris = !isPupil && Math.abs(i - 4) <= half + 1 && Math.abs(j - 2) <= half + 1;
        if (isPupil) glowBlock(ctx, X, Y, px, px, css(RD), 0.95, 0.7 * recPulse(t, cfg.recPulseHz));
        else if (isIris) glowBlock(ctx, X, Y, px, px, css(RD), 0.5 * recPulse(t, cfg.recPulseHz) + 0.25, 0);
        else glowBlock(ctx, X, Y, px, px, '#f4ede2', 0.14, 0);
      }
    }
    ctx.restore();
  },
};
/* ---------------- app ---------------- */
function App() {
  const cfg = useDialKit('Indicator', {
    variant: { type: 'select', options: Object.keys(VARIANTS), default: 'palette' },
    state: { type: 'select', options: ['hidden', 'armed', 'listen', 'transcribe', 'record', 'save'], default: 'listen' },
    voice: { type: 'select', options: ['fake', 'silent', 'mic'], default: 'fake' },
    desktop: { type: 'select', options: ['light', 'photo', 'dark'], default: 'light' },
    capsule: { height: [16, 10, 32, 1], idleW: [16, 8, 40, 1], activeW: [56, 24, 140, 1] },
    chip: { w: [104, 56, 220, 2], h: [30, 18, 64, 1] },
    line: { bars: [21, 7, 41, 2], thickness: [2.5, 1, 6, 0.5], tilt: [1.6, 0, 3, 0.1], mirror: true },
    eye: { pupil: [0.2, 0, 0.5, 0.02], blinkEvery: [3.6, 1.5, 8, 0.1], envelope: [0.8, 0, 1, 0.05] },
    meterSpring: { response: [0.15, 0.05, 0.6, 0.01], damping: [0.86, 0.3, 1.2, 0.01] },
    statusSpring: { response: [0.3, 0.1, 0.8, 0.01], damping: [0.7, 0.3, 1.2, 0.01] },
    meterGain: [3, 0.5, 6, 0.1],
    glow: { inner: [1, 0, 2.5, 0.05], outer: [1, 0, 3, 0.05] },
    bands: { bars: [8, 4, 16, 1], gain: [1.5, 0.5, 3, 0.1], attack: [28, 2, 60, 1], release: [6, 0.5, 20, 0.5] },
    warnDelay: [1.2, 0.3, 4, 0.1],
    recPulseHz: [0.9, 0.2, 3, 0.05],
    pupil: { min: [2, 1, 5, 0.5], max: [5, 2, 9, 0.5] },
    colors: { listen: '#50c8f5', record: '#ff3b30', warn: '#ffb340' },
  }, { id: 'see-indicator', persist: true });
  const cfgRef = useRef(cfg); cfgRef.current = cfg;
  const cvRef = useRef(null);
  useEffect(() => {
    if (cfg.voice === 'mic' && !micAnalyser) enableMic().catch(() => {});
  }, [cfg.voice]);
  useEffect(() => {
    const cv = cvRef.current, ctx = cv.getContext('2d');
    let raf, last = performance.now();
    const loopId = Math.random().toString(36).slice(2, 7);
    const loop = (now) => {
      window.__loopIds = window.__loopIds || {};
      window.__loopIds[loopId] = (window.__loopIds[loopId] || 0) + 1;
      const c = cfgRef.current;
      const dt = Math.min(0.05, (now - last) / 1000); last = now;
      const t = now / 1000;
      /* meter at 10 Hz */
      if (t >= S.sampleAt) {
        const raw = c.voice === 'silent' ? { avg: 0.004, peak: 0.006 }
          : c.voice === 'mic' && micAnalyser ? micLevel() : fakeLevel(t);
        S.mT = raw.avg; S.pT = raw.peak; S.sampleAt = t + 0.1;
        if (raw.avg < 0.045) S.silentFor += t - (S.lastSampleT ?? t); else S.silentFor = 0;
        S.lastSampleT = t;
      }
      const active = c.state === 'listen' || c.state === 'record';
      const avg = Math.min(1, stepSpring(S.m, active ? S.mT : 0, dt, c.meterSpring.response, c.meterSpring.damping) * c.meterGain);
      const peak = Math.min(1, stepSpring(S.p, active ? S.pT : 0, dt, c.meterSpring.response, c.meterSpring.damping) * c.meterGain);
      /* bands */
      const target = !active || c.voice === 'silent' ? null
        : c.voice === 'mic' && micAnalyser ? micBands() : fakeBands(t);
      for (let b = 0; b < N_BANDS; b++) {
        const tv = target ? target[b] : 0;
        if (tv > S.bands[b]) S.bands[b] += (tv - S.bands[b]) * Math.min(1, dt * c.bands.attack);
        else S.bands[b] += (tv - S.bands[b]) * Math.min(1, dt * c.bands.release);
      }
      const warn = S.silentFor > c.warnDelay && active;
      window.__dbg = { silentFor: S.silentFor, warnDelay: c.warnDelay, voice: c.voice, state: c.state, warn };
      const sc = stepSpring(S.sc, c.state === 'hidden' ? 0 : 1, dt, c.statusSpring.response, c.statusSpring.damping);
      const wT = active ? (warn ? c.capsule.idleW : c.capsule.activeW)
        : c.state === 'transcribe' || c.state === 'save' ? (c.capsule.idleW + c.capsule.activeW) / 2 : c.capsule.idleW;
      const w = stepSpring(S.w, wT, dt, c.statusSpring.response, c.statusSpring.damping);
      const d = { avg, peak, w, warn, sc, status: c.state, elapsed: (performance.now() - S.t0) / 1000 };
      ctx.setTransform(1, 0, 0, 1, 0, 0); ctx.clearRect(0, 0, cv.width, cv.height);
      ctx.setTransform(2, 0, 0, 2, 0, 0);
      if (sc > 0.02) {
        ctx.save();
        if (c.variant !== 'eyeline') {
          ctx.translate(180, 30); ctx.scale(sc, sc); ctx.translate(-180, -30);
        }
        ctx.globalAlpha = Math.max(0, Math.min(1, sc));
        try { VARIANTS[c.variant](ctx, 180, 30, t, d, c); } catch (e) { document.title = 'ERR ' + e; }
        ctx.restore();
      }
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(raf);
  }, []);
  const deskBg = cfg.desktop === 'light' ? '#e9e5dc'
    : cfg.desktop === 'dark' ? '#1d1f24'
    : 'linear-gradient(135deg,#f5b26b,#e2766d 50%,#7b5ea7)';
  return (
    <div style={{ position: 'fixed', inset: 0, background: deskBg }}>
      <div style={{ position: 'fixed', top: 0, left: 0, right: 0, height: 25,
        background: cfg.desktop === 'light' ? 'rgba(250,248,244,0.85)' : 'rgba(20,21,25,0.85)',
        display: 'flex', alignItems: 'center', gap: 18, padding: '0 14px',
        fontSize: 12.5, color: cfg.desktop === 'light' ? '#3c3a36' : '#c9cbd1' }}>
        <b></b><span>File</span><span>Edit</span><span>View</span><span>Window</span><span>Help</span>
      </div>
      <div style={{ position: 'absolute', left: '50%', top: 70, transform: 'translateX(-50%)',
        width: 620, height: '100vh', background: cfg.desktop === 'dark' ? '#26282e' : '#fff',
        borderRadius: '8px 8px 0 0', boxShadow: '0 4px 24px rgba(0,0,0,0.18)', padding: '46px 54px' }}>
        {[62, 88, 74, 91, 55, 83].map((w2, i) => (
          <div key={i} style={{ height: 10, borderRadius: 3, margin: '14px 0', width: w2 + '%',
            background: cfg.desktop === 'dark' ? '#34373f' : '#dcd8cf' }} />
        ))}
      </div>
      <canvas ref={cvRef} width={720} height={120}
        style={{ position: 'fixed', top: 34, left: '50%', transform: 'translateX(-50%)',
          width: 360, height: 60, pointerEvents: 'none', zIndex: 5 }} />
    </div>
  );
}
createRoot(document.getElementById('root')).render(
  <React.StrictMode><App /><DialRoot /></React.StrictMode>
);
