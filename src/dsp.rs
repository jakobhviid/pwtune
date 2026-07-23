//! Signal generation + analysis. Reimplements the slice of NumPy/SciPy the
//! Python version used: a log sine sweep, Welch PSD (Hann window, 50% overlap,
//! constant detrend), moving-average smoothing, linear interpolation, and the
//! parametric-EQ design. The frequency response is a RATIO of two PSDs computed
//! identically, so PSD scaling constants cancel — we only need the averaged
//! periodogram per bin, which makes this match SciPy's shape without its scaling.
use anyhow::{anyhow, Result};
use rustfft::{num_complex::Complex, FftPlanner};
use std::f64::consts::PI;

pub const RATE: u32 = 48000;
pub const DURATION: f64 = 3.0;
pub const SWEEP_LOW: f64 = 50.0;
pub const SWEEP_HIGH: f64 = 20000.0;
pub const SETTLE: f64 = 0.5;
pub const DEFAULT_ITERATIONS: usize = 3;

const TARGET: &[(f64, f64)] = &[
    (50.0, 6.0), (100.0, 5.0), (200.0, 4.0), (400.0, 2.0), (800.0, 0.0),
    (1500.0, 0.0), (2500.0, -1.0), (4000.0, 0.0), (8000.0, 0.0), (16000.0, -1.0),
];

pub struct Band {
    pub freq: f64,
    pub gain: f64,
    pub q: f64,
    pub kind: &'static str,
}

fn n_settle() -> usize { (SETTLE * RATE as f64) as usize }
fn n_sweep() -> usize { (DURATION * RATE as f64) as usize }

/// Log sine sweep with leading/trailing silence, as i16 samples.
pub fn gen_sweep_i16() -> Vec<i16> {
    let ns = n_settle();
    let nw = n_sweep();
    let mut out = vec![0i16; ns];
    let k = (SWEEP_HIGH / SWEEP_LOW).ln();
    for i in 0..nw {
        let t = i as f64 * DURATION / nw as f64; // linspace endpoint=False
        let phase = 2.0 * PI * SWEEP_LOW * DURATION / k * ((t / DURATION * k).exp() - 1.0);
        let s = 0.7 * phase.sin();
        out.push((s * 32767.0) as i16);
    }
    out.extend(std::iter::repeat(0i16).take(ns));
    out
}

pub fn write_wav(path: &str, samples: &[i16]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(())
}

/// Read a WAV as mono f64 in [-1,1). Takes channel 0 if multi-channel.
fn read_wav_mono(path: &str) -> Result<Vec<f64>> {
    let mut r = hound::WavReader::open(path)?;
    let ch = r.spec().channels as usize;
    let raw: Vec<i32> = match r.spec().sample_format {
        hound::SampleFormat::Int => r.samples::<i32>().collect::<std::result::Result<_, _>>()?,
        hound::SampleFormat::Float => r
            .samples::<f32>()
            .map(|s| s.map(|v| (v * 32768.0) as i32))
            .collect::<std::result::Result<_, _>>()?,
    };
    let mono: Vec<f64> = if ch <= 1 {
        raw.iter().map(|&v| v as f64 / 32768.0).collect()
    } else {
        raw.iter().step_by(ch).map(|&v| v as f64 / 32768.0).collect()
    };
    Ok(mono)
}

/// Welch PSD: averaged |FFT(hann * detrended segment)|^2 per bin (no scaling).
fn welch(x: &[f64], nperseg: usize) -> Vec<f64> {
    let noverlap = nperseg / 2;
    let step = (nperseg - noverlap).max(1);
    let win: Vec<f64> = (0..nperseg)
        .map(|n| 0.5 - 0.5 * (2.0 * PI * n as f64 / nperseg as f64).cos()) // periodic Hann
        .collect();
    let nbins = nperseg / 2 + 1;
    let mut psd = vec![0.0f64; nbins];
    let mut nseg = 0usize;
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(nperseg);
    let mut start = 0usize;
    while start + nperseg <= x.len() {
        let seg = &x[start..start + nperseg];
        let mean = seg.iter().sum::<f64>() / nperseg as f64; // detrend='constant'
        let mut buf: Vec<Complex<f64>> = seg
            .iter()
            .enumerate()
            .map(|(i, &v)| Complex::new((v - mean) * win[i], 0.0))
            .collect();
        fft.process(&mut buf);
        for (k, item) in psd.iter_mut().enumerate() {
            *item += buf[k].norm_sqr();
        }
        nseg += 1;
        start += step;
    }
    if nseg > 0 {
        for v in psd.iter_mut() {
            *v /= nseg as f64;
        }
    }
    psd
}

/// scipy.ndimage.uniform_filter1d(x, size), mode='reflect' (edge duplicated).
fn uniform_filter_reflect(x: &[f64], size: usize) -> Vec<f64> {
    let n = x.len() as i64;
    let half = (size / 2) as i64;
    let refl = |i: i64| -> usize {
        if n == 1 {
            return 0;
        }
        let p = 2 * n;
        let mut j = i % p;
        if j < 0 {
            j += p;
        }
        if j >= n {
            j = p - 1 - j;
        }
        j as usize
    };
    (0..x.len())
        .map(|i| {
            let i = i as i64;
            let mut s = 0.0;
            for k in (i - half)..=(i + half) {
                s += x[refl(k)];
            }
            s / size as f64
        })
        .collect()
}

/// Returns (freqs, response_db) or None if the recording is too short.
pub fn analyze_response(sweep_path: &str, rec_path: &str) -> Result<Option<(Vec<f64>, Vec<f64>)>> {
    let sweep = read_wav_mono(sweep_path)?;
    let rec = read_wav_mono(rec_path)?;
    let margin = (0.1 * RATE as f64) as usize;
    let start = n_settle() + margin;
    let mut end = n_settle() + n_sweep() - margin;
    if rec.len() < end {
        end = end.min(rec.len().saturating_sub(margin));
        if end <= start {
            return Ok(None);
        }
    }
    if sweep.len() < end {
        return Err(anyhow!("sweep file shorter than expected"));
    }
    let refs = &sweep[start..end];
    let recs = &rec[start..end];
    let nperseg = 8192.min(refs.len() / 4).max(1);
    let psd_ref = welch(refs, nperseg);
    let psd_rec = welch(recs, nperseg);
    let eps = 1e-12;
    let db: Vec<f64> = psd_ref
        .iter()
        .zip(&psd_rec)
        .map(|(&r, &c)| 10.0 * ((c + eps) / (r + eps)).log10())
        .collect();
    let sm = uniform_filter_reflect(&db, 15);
    let freqs: Vec<f64> = (0..psd_ref.len())
        .map(|k| k as f64 * RATE as f64 / nperseg as f64)
        .collect();
    Ok(Some((freqs, sm)))
}

fn arg_nearest(freqs: &[f64], cf: f64) -> usize {
    let mut best = 0;
    let mut bd = f64::INFINITY;
    for (i, &f) in freqs.iter().enumerate() {
        let d = (f - cf).abs();
        if d < bd {
            bd = d;
            best = i;
        }
    }
    best
}

pub fn value_at(freqs: &[f64], db: &[f64], cf: f64) -> f64 {
    db[arg_nearest(freqs, cf)]
}

pub fn print_response(freqs: &[f64], db: &[f64], label: &str) {
    println!("\n  {label}:");
    for &cf in &[63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0] {
        let v = value_at(freqs, db, cf);
        let bars = ((v + 20.0) as i64).clamp(0, 50) as usize;
        let bar = "█".repeat(bars);
        let note = if cf >= 2000.0 { "   (mic-dependent up here — trust your ears)" } else { "" };
        println!("  {cf:>5} Hz: {v:>+6.1} dB  {bar}{note}");
    }
}

fn interp_target(freqs: &[f64]) -> Vec<f64> {
    freqs
        .iter()
        .map(|&f| {
            if f <= TARGET[0].0 {
                return TARGET[0].1;
            }
            if f >= TARGET[TARGET.len() - 1].0 {
                return TARGET[TARGET.len() - 1].1;
            }
            for w in TARGET.windows(2) {
                let (f0, v0) = w[0];
                let (f1, v1) = w[1];
                if f >= f0 && f <= f1 {
                    return v0 + (v1 - v0) * (f - f0) / (f1 - f0);
                }
            }
            0.0
        })
        .collect()
}

/// Design corrective parametric EQ bands from a measured response.
pub fn design_eq(freqs: &[f64], measured: &[f64]) -> Vec<Band> {
    let target = interp_target(freqs);
    let mut error: Vec<f64> = target.iter().zip(measured).map(|(&t, &m)| t - m).collect();
    // Normalize out mic sensitivity using the 500-2000 Hz band as reference.
    let refv: Vec<f64> = freqs
        .iter()
        .zip(&error)
        .filter(|(&f, _)| (500.0..=2000.0).contains(&f))
        .map(|(_, &e)| e)
        .collect();
    if !refv.is_empty() {
        let m = refv.iter().sum::<f64>() / refv.len() as f64;
        for e in error.iter_mut() {
            *e -= m;
        }
    }
    let mut bands = Vec::new();
    for &ef in &[80.0, 150.0, 300.0, 500.0, 1000.0, 2000.0, 3500.0, 6000.0, 10000.0] {
        let idx = arg_nearest(freqs, ef);
        let corr = error[idx].clamp(-6.0, 8.0);
        if corr.abs() > 0.5 {
            let (kind, q) = if ef <= 120.0 {
                ("bq_lowshelf", 0.6)
            } else if ef >= 8000.0 {
                ("bq_highshelf", 0.7)
            } else {
                ("bq_peaking", 1.2)
            };
            bands.push(Band { freq: ef, gain: (corr * 10.0).round() / 10.0, q, kind });
        }
    }
    bands
}
