//! Signal generation + analysis. Reimplements the slice of NumPy/SciPy the
//! Python version used: a log sine sweep, Welch PSD (Hann window, 50% overlap,
//! constant detrend), moving-average smoothing, linear interpolation, and the
//! parametric-EQ design. The frequency response is a RATIO of two PSDs computed
//! identically, so PSD scaling constants cancel — we only need the averaged
//! periodogram per bin, which makes this match SciPy's shape without its scaling.
use anyhow::{anyhow, Result};
use crate::biquad::{cascade_db, Biquad, BUTTERWORTH_Q4};
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

/// One section of a profile's filter graph. `node` and `note` travel with it
/// because a label does not say what a section is for — `bq_highshelf` is both the
/// makeup-gain stage and the top correction band — and these files are read and
/// hand-edited after they are written.
pub struct Band {
    pub freq: f64,
    pub gain: f64,
    pub q: f64,
    pub kind: &'static str,
    pub node: String,
    pub note: String,
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

/// Octave centres every response block is printed at, so measured and realised
/// curves line up column for column.
pub const DISPLAY_FREQS: &[f64] = &[63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];

pub fn print_response(freqs: &[f64], db: &[f64], label: &str) {
    println!("\n  {label}:");
    for &cf in DISPLAY_FREQS {
        let v = value_at(freqs, db, cf);
        let bars = ((v + 20.0) as i64).clamp(0, 50) as usize;
        let bar = "█".repeat(bars);
        let note = if cf >= 2000.0 { "   (mic-dependent up here — trust your ears)" } else { "" };
        println!("  {cf:>5} Hz: {v:>+6.1} dB  {bar}{note}");
    }
}

/// What the finished chain delivers at each display frequency. Its sections
/// overlap, so this is the product of the chain and not the sum of the gains its
/// bands ask for — the curve a listener actually gets.
pub fn print_chain_response(sections: &[Biquad], label: &str) {
    println!("\n  {label}:");
    for &cf in DISPLAY_FREQS {
        println!("  {cf:>5} Hz: {:>+6.1} dB", cascade_db(sections, cf));
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
/// Where the designer may place a section, and the shape it uses there: roughly one
/// per two-thirds of an octave from the low-mids up.
///
/// The low band peaks rather than shelves. A shelf holds its gain below its corner
/// all the way to DC, which on a small driver buys excursion and no sound; a peaking
/// section rolls off below its corner, so a correction aimed at 80 Hz stays near
/// 80 Hz. At the top a shelf is what is wanted — tweeter output falls away and
/// there is no excursion cost to lifting it.
const BAND_PLAN: &[(f64, &str, f64)] = &[
    (80.0, "bq_peaking", 1.2),
    (150.0, "bq_peaking", 1.2),
    (300.0, "bq_peaking", 1.2),
    (500.0, "bq_peaking", 1.2),
    (1000.0, "bq_peaking", 1.2),
    (2000.0, "bq_peaking", 1.2),
    (3500.0, "bq_peaking", 1.2),
    (6000.0, "bq_peaking", 1.2),
    (10000.0, "bq_highshelf", 0.7),
];

/// A band this small is not worth a filter stage.
const GAIN_FLOOR: f64 = 0.5;

/// The bounds a design works inside. A speaker class supplies these; on their own
/// they describe an unprotected full-range speaker, which is what a caller that
/// declares nothing gets.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Lowest frequency worth correcting. Nothing is emitted below it: under a
    /// high-pass a boost fights the filter, and under a driver's usable range it
    /// buys cone excursion and no sound.
    pub correct_from: f64,
    /// How far one band may be driven, in dB.
    pub max_cut: f64,
    pub max_boost: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { correct_from: 0.0, max_cut: -6.0, max_boost: 8.0 }
    }
}

/// Fit passes and step size for `fit_gains`. The band grid is spaced under an
/// octave apart, so each band's own section dominates its centre while its
/// neighbours contribute roughly three quarters between them — diagonally
/// dominant, and a half-step keeps the iteration from ringing on the way in.
const FIT_PASSES: usize = 200;
const FIT_RELAX: f64 = 0.5;

/// The correction the response asks for, per frequency bin: distance from the
/// target curve, with mic sensitivity divided out by referencing the 500–2000 Hz
/// mean (an uncalibrated mic gives relative levels, never absolute ones).
fn correction_curve(freqs: &[f64], measured: &[f64]) -> Vec<f64> {
    let target = interp_target(freqs);
    let mut error: Vec<f64> = target.iter().zip(measured).map(|(&t, &m)| t - m).collect();
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
    error
}

/// Mean correction over a third of an octave either side of `cf`, so one noisy bin
/// cannot set a band's gain on its own.
fn band_mean(freqs: &[f64], curve: &[f64], cf: f64) -> f64 {
    let (lo, hi) = (cf / 2f64.powf(1.0 / 3.0), cf * 2f64.powf(1.0 / 3.0));
    let vals: Vec<f64> = freqs
        .iter()
        .zip(curve)
        .filter(|(&f, _)| f >= lo && f <= hi)
        .map(|(_, &v)| v)
        .collect();
    if vals.is_empty() {
        curve[arg_nearest(freqs, cf)]
    } else {
        vals.iter().sum::<f64>() / vals.len() as f64
    }
}

fn sections_of(plan: &[(f64, &str, f64)], gains: &[f64]) -> Vec<Biquad> {
    plan.iter()
        .zip(gains)
        .filter_map(|(&(f, kind, q), &g)| Biquad::from_label(kind, f, q, g))
        .collect()
}

/// Solve for the gains whose *cascade* delivers `want` at each band centre.
///
/// The sections overlap, so they add to each other's output: nine bands each asked
/// for +4 dB deliver about +7 dB where they meet. Each pass re-evaluates the cascade
/// and nudges every band by what is still missing at its own centre, which converges
/// on gains that realise the requested curve instead of overshooting it.
fn fit_gains(plan: &[(f64, &str, f64)], want: &[f64], lim: Limits) -> Vec<f64> {
    let mut gains = vec![0.0; plan.len()];
    for _ in 0..FIT_PASSES {
        for i in 0..plan.len() {
            let realised = cascade_db(&sections_of(plan, &gains), plan[i].0);
            gains[i] =
                (gains[i] + FIT_RELAX * (want[i] - realised)).clamp(lim.max_cut, lim.max_boost);
        }
    }
    gains
}

/// Design corrective parametric EQ bands from a measured response, inside `lim`.
pub fn design_eq(freqs: &[f64], measured: &[f64], lim: Limits) -> Vec<Band> {
    let curve = correction_curve(freqs, measured);
    let usable: Vec<(f64, &str, f64)> =
        BAND_PLAN.iter().copied().filter(|&(cf, _, _)| cf >= lim.correct_from).collect();
    let want: Vec<f64> = usable.iter().map(|&(cf, _, _)| band_mean(freqs, &curve, cf)).collect();

    // Fit, drop the bands too small to earn a stage, then fit again: dropping a
    // section changes what the rest of the cascade delivers.
    let first = fit_gains(&usable, &want, lim);
    let keep: Vec<usize> = (0..usable.len()).filter(|&i| first[i].abs() > GAIN_FLOOR).collect();
    let plan: Vec<(f64, &str, f64)> = keep.iter().map(|&i| usable[i]).collect();
    let want: Vec<f64> = keep.iter().map(|&i| want[i]).collect();
    let gains = fit_gains(&plan, &want, lim);

    plan.iter()
        .zip(&gains)
        .enumerate()
        .map(|(i, (&(freq, kind, q), &g))| Band {
            freq,
            gain: (g * 10.0).round() / 10.0,
            q,
            kind,
            node: format!("eq_band{}", i + 1),
            note: format!("correct toward target at {freq:.0} Hz"),
        })
        .collect()
}

/// A 4th-order Butterworth high-pass at `corner`, as two cascaded sections.
///
/// Below a driver's usable range, signal moves the cone without being heard, and the
/// excursion costs headroom the audible band needs — amplifier protection responds
/// to the peaks by pulling everything down. Removing that content is what lets the
/// rest get louder, even though the filter only ever subtracts.
pub fn highpass_bands(corner: f64) -> Vec<Band> {
    BUTTERWORTH_Q4
        .iter()
        .enumerate()
        .map(|(i, &q)| Band {
            freq: corner,
            gain: 0.0,
            q,
            kind: "bq_highpass",
            node: format!("highpass{}", i + 1),
            note: format!(
                "high-pass {corner:.0} Hz, 4th-order Butterworth (section {} of 2)",
                i + 1
            ),
        })
        .collect()
}

/// Centre frequencies of bands pinned at the gain clamp. A pinned band means the
/// target sits further from the measurement than one filter is allowed to travel,
/// so the realised curve falls short of the requested one there by design.
pub fn saturated_bands(bands: &[Band], lim: Limits) -> Vec<f64> {
    bands
        .iter()
        .filter(|b| b.node.starts_with("eq_band"))
        .filter(|b| b.gain <= lim.max_cut + 0.05 || b.gain >= lim.max_boost - 0.05)
        .map(|b| b.freq)
        .collect()
}

/// The sections a set of bands compiles to, for evaluating what the chain delivers.
pub fn bands_to_sections(bands: &[Band]) -> Vec<Biquad> {
    bands.iter().filter_map(|b| Biquad::from_label(b.kind, b.freq, b.q, b.gain)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response that sits `deficit` dB below the target below `corner`, and on it
    /// above — so the correction curve asks for a low-end lift and nothing else.
    fn measured_with_low_deficit(corner: f64, deficit: f64) -> (Vec<f64>, Vec<f64>) {
        let nperseg = 8192usize;
        let freqs: Vec<f64> =
            (0..nperseg / 2 + 1).map(|k| k as f64 * RATE as f64 / nperseg as f64).collect();
        let target = interp_target(&freqs);
        let measured: Vec<f64> = freqs
            .iter()
            .zip(&target)
            .map(|(&f, &t)| if f < corner { t - deficit } else { t })
            .collect();
        (freqs, measured)
    }

    #[test]
    fn realised_cascade_matches_what_the_bands_ask_for() {
        let (freqs, measured) = measured_with_low_deficit(400.0, 4.0);
        let bands = design_eq(&freqs, &measured, Limits::default());
        let sections = bands_to_sections(&bands);
        for cf in [80.0, 150.0, 300.0] {
            let realised = cascade_db(&sections, cf);
            assert!(
                (realised - 4.0).abs() < 0.35,
                "at {cf} Hz the cascade delivers {realised:+.2} dB, not the +4 dB requested"
            );
        }
    }

    #[test]
    fn the_reference_band_is_left_alone() {
        let (freqs, measured) = measured_with_low_deficit(400.0, 4.0);
        let sections = bands_to_sections(&design_eq(&freqs, &measured, Limits::default()));
        for cf in [1000.0, 2000.0] {
            assert!(cascade_db(&sections, cf).abs() < 0.6, "{cf} Hz should stay flat");
        }
    }

    /// A flat response needs no filters at all.
    #[test]
    fn a_response_on_target_designs_no_bands() {
        let (freqs, measured) = measured_with_low_deficit(400.0, 0.0);
        assert!(design_eq(&freqs, &measured, Limits::default()).is_empty());
    }

    #[test]
    fn band_gains_stay_inside_the_clamp() {
        let (freqs, measured) = measured_with_low_deficit(400.0, 40.0);
        for b in design_eq(&freqs, &measured, Limits::default()) {
            let lim = Limits::default();
            assert!(b.gain >= lim.max_cut && b.gain <= lim.max_boost, "{} dB out of range", b.gain);
        }
    }

    /// The low band rolls off below its corner instead of holding gain down to DC,
    /// which is what keeps a correction aimed at 80 Hz from driving excursion at 20.
    #[test]
    fn the_low_band_does_not_hold_gain_to_dc() {
        let (freqs, measured) = measured_with_low_deficit(400.0, 6.0);
        let sections = bands_to_sections(&design_eq(&freqs, &measured, Limits::default()));
        let at_80 = cascade_db(&sections, 80.0);
        let at_20 = cascade_db(&sections, 20.0);
        assert!(at_80 > 2.0, "expected a low-end lift, got {at_80:+.2} dB");
        assert!(at_20 < at_80 - 3.0, "20 Hz ({at_20:+.2}) should fall well below 80 Hz ({at_80:+.2})");
    }
}
