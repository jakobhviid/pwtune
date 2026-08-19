//! Signal generation, measurement, and the parametric-EQ design.
//!
//! A log sine sweep goes out, the recording comes back, and the two are compared by
//! deconvolution: dividing in the frequency domain recovers the system's impulse
//! response, whose arrival gives both the response and the playback delay. The delay
//! matters because the recorder starts before playback does, and by an amount nobody
//! controls — so a measurement that assumed a fixed offset would compare the top of
//! the reference sweep against silence.
use anyhow::{anyhow, Result};
use crate::biquad::{cascade_db, peak_gain_db, Biquad, BUTTERWORTH_Q4, LINKWITZ_RILEY_Q4};
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

/// Regularization floor for the spectral division, relative to the reference's peak
/// power. Outside the swept band the reference carries no energy, and this is what
/// keeps the division from amplifying whatever noise sits there.
const REGULARIZATION: f64 = 1e-6;
/// Seconds of impulse response kept after the arrival. Long enough that 50 Hz is
/// still resolved (2.5 Hz per bin), short enough to drop the late tail and the
/// silence around the sweep.
const IR_WINDOW: f64 = 0.4;
/// Seconds kept before the arrival, so a slightly early onset is not clipped.
const IR_PRE: f64 = 0.005;
/// How far the arrival must stand above the noise floor, as a multiple of what noise
/// alone would produce.
///
/// The peak-to-RMS ratio of `n` Gaussian samples is about `sqrt(2 ln n)` — near 5 for
/// the lengths here — so the bar has to be set against that rather than at a fixed
/// number, or it drifts with the length of the recording. A real measurement clears
/// it by an order of magnitude; a recording with no sweep in it does not clear it at
/// all.
const MIN_ARRIVAL_MARGIN: f64 = 1.5;

/// The peak-to-RMS ratio noise alone would give over `n` samples.
fn noise_peak_ratio(n: usize) -> f64 {
    (2.0 * (n.max(2) as f64).ln()).sqrt()
}
/// Smoothing width, as a fraction of an octave either side.
const SMOOTH_OCTAVE: f64 = 1.0 / 6.0;

fn next_pow2(n: usize) -> usize {
    let mut p = 1usize;
    while p < n {
        p <<= 1;
    }
    p
}

/// The system's impulse response, and how far its arrival stands above the noise.
///
/// The sweep spreads its energy over time, so the recording is compared to the
/// reference by dividing in the frequency domain rather than by taking a ratio of
/// per-segment power. That collapses the sweep back into a single arrival, which
/// does two things a power ratio cannot: it recovers the sweep's processing gain
/// (rising with frequency, since a log sweep dwells ever more briefly in each bin),
/// and it locates the arrival — so the result does not depend on how long the
/// recorder was running before playback began. Harmonic distortion lands ahead of
/// the arrival and is left outside the window.
fn impulse_response(reference: &[f64], recording: &[f64]) -> (Vec<f64>, f64) {
    let n = next_pow2(reference.len() + recording.len());
    let mut planner = FftPlanner::new();
    let forward = planner.plan_fft_forward(n);
    let inverse = planner.plan_fft_inverse(n);

    let pad = |x: &[f64]| -> Vec<Complex<f64>> {
        (0..n).map(|i| Complex::new(x.get(i).copied().unwrap_or(0.0), 0.0)).collect()
    };
    let mut refspec = pad(reference);
    let mut recspec = pad(recording);
    forward.process(&mut refspec);
    forward.process(&mut recspec);

    let peak_power = refspec.iter().map(|c| c.norm_sqr()).fold(0.0f64, f64::max);
    let eps = peak_power * REGULARIZATION;
    let mut spec: Vec<Complex<f64>> = refspec
        .iter()
        .zip(&recspec)
        .map(|(&r, &c)| c * r.conj() / (r.norm_sqr() + eps))
        .collect();
    inverse.process(&mut spec);

    let ir: Vec<f64> = spec.iter().map(|c| c.re / n as f64).collect();
    let peak = ir.iter().fold(0.0f64, |a, &v| a.max(v.abs()));
    let rms = (ir.iter().map(|v| v * v).sum::<f64>() / ir.len() as f64).sqrt();
    let confidence = if rms > 0.0 { peak / rms } else { 0.0 };
    (ir, confidence)
}

/// Fractional-octave moving average of a dB curve, over the swept band only.
///
/// A kernel of constant width in Hz spans octaves at the bottom of the sweep and a
/// sliver at the top, which smears hardest where resolution is scarcest and leaves
/// the noisiest region untouched. A constant fraction of an octave treats the whole
/// spectrum alike, and clamping the kernel to the swept band stops bins that were
/// never excited from leaking into ones that were.
fn smooth_octave(freqs: &[f64], db: &[f64], fraction: f64, lo: f64, hi: f64) -> Vec<f64> {
    let n = db.len();
    if n == 0 {
        return Vec::new();
    }
    let mut cumulative = vec![0.0f64; n + 1];
    for i in 0..n {
        cumulative[i + 1] = cumulative[i] + db[i];
    }
    let step = if freqs.len() > 1 { freqs[1] - freqs[0] } else { 1.0 };
    let index = |f: f64| -> usize { ((f / step).round().max(0.0) as usize).min(n - 1) };
    let (i_lo, i_hi) = (index(lo), index(hi));
    let k = 2f64.powf(fraction);
    (0..n)
        .map(|i| {
            let f = freqs[i].clamp(lo, hi);
            let a = index(f / k).max(i_lo);
            let b = index(f * k).min(i_hi);
            if b >= a {
                (cumulative[b + 1] - cumulative[a]) / (b - a + 1) as f64
            } else {
                db[i.clamp(i_lo, i_hi)]
            }
        })
        .collect()
}

/// Returns (freqs, response_db), or None if the recording is too short to hold the
/// sweep. Errors when no sweep can be found in it at all.
pub fn analyze_response(sweep_path: &str, rec_path: &str) -> Result<Option<(Vec<f64>, Vec<f64>)>> {
    let sweep = read_wav_mono(sweep_path)?;
    let rec = read_wav_mono(rec_path)?;
    if rec.len() < sweep.len() {
        return Ok(None);
    }
    let (ir, confidence) = impulse_response(&sweep, &rec);
    let floor = noise_peak_ratio(ir.len()) * MIN_ARRIVAL_MARGIN;
    if confidence < floor {
        return Err(anyhow!(
            "no sweep in the recording: the strongest arrival is {confidence:.1}x the noise \
             floor, and noise alone reaches {:.1}x. Is the speaker actually playing, is the mic \
             in front of it, and is anything muted?",
            noise_peak_ratio(ir.len())
        ));
    }
    let arrival = ir
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0);

    let pre = (IR_PRE * RATE as f64) as usize;
    let post = (IR_WINDOW * RATE as f64) as usize;
    let start = arrival.saturating_sub(pre);
    let end = (arrival + post).min(ir.len());
    if end <= start + RATE as usize / 100 {
        return Ok(None);
    }

    // Taper both ends so the cut itself does not ring: in over the lead-in, out over
    // the last quarter, where only the decaying tail remains.
    let len = end - start;
    let lead = arrival - start;
    let fade = len / 4;
    let mut windowed: Vec<f64> = Vec::with_capacity(len);
    for (i, &v) in ir[start..end].iter().enumerate() {
        let mut w = 1.0;
        if lead > 0 && i < lead {
            w *= 0.5 - 0.5 * (PI * (i as f64 / lead as f64 + 1.0)).cos();
        }
        if fade > 0 && i >= len - fade {
            let t = (i - (len - fade)) as f64 / fade as f64;
            w *= 0.5 + 0.5 * (PI * t).cos();
        }
        windowed.push(v * w);
    }

    let nfft = next_pow2(windowed.len());
    let mut spec: Vec<Complex<f64>> = (0..nfft)
        .map(|i| Complex::new(windowed.get(i).copied().unwrap_or(0.0), 0.0))
        .collect();
    FftPlanner::new().plan_fft_forward(nfft).process(&mut spec);

    let bins = nfft / 2 + 1;
    let raw: Vec<f64> = spec[..bins].iter().map(|c| 20.0 * c.norm().max(1e-30).log10()).collect();
    let freqs: Vec<f64> = (0..bins).map(|k| k as f64 * RATE as f64 / nfft as f64).collect();
    let smoothed = smooth_octave(&freqs, &raw, SMOOTH_OCTAVE, SWEEP_LOW, SWEEP_HIGH);
    Ok(Some((freqs, smoothed)))
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
/// Every band peaks. A shelf holds its gain past its corner without bound, which at
/// the bottom buys excursion and no sound, and at the top extrapolates a deficit
/// measured at 10 kHz out to 20 kHz where nothing was measured at all — and reaches
/// only half its stated gain at its own corner, so the number a band carries is not
/// the number it delivers. A peaking section rolls off either side of its corner, so
/// a correction stays where the measurement put it.
const BAND_PLAN: &[(f64, &str, f64)] = &[
    (80.0, "bq_peaking", 1.2),
    (150.0, "bq_peaking", 1.2),
    (300.0, "bq_peaking", 1.2),
    (500.0, "bq_peaking", 1.2),
    (1000.0, "bq_peaking", 1.2),
    (2000.0, "bq_peaking", 1.2),
    (3500.0, "bq_peaking", 1.2),
    (6000.0, "bq_peaking", 1.2),
    (10000.0, "bq_peaking", 1.2),
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

/// The correction the response asks for, per frequency bin: distance from the target
/// curve. An uncalibrated measurement fixes the response only up to a constant, and
/// that constant is resolved later, once the bands being emitted are known.
fn correction_curve(freqs: &[f64], measured: &[f64]) -> Vec<f64> {
    let target = interp_target(freqs);
    target.iter().zip(measured).map(|(&t, &m)| t - m).collect()
}

/// Most gain the EQ may apply anywhere, before makeup gain.
///
/// A per-band clamp does not bound this: the bands overlap, so nine of them clamped
/// at +8 dB each realised +12.5 dB where they met. The quantity that costs headroom
/// and stresses a driver is the cascade's peak, so that is what gets a budget.
const REALISED_PEAK_MAX: f64 = 6.0;

/// Scale every gain by one factor until the cascade's peak fits `budget`.
///
/// Uniform scaling keeps the shape the fit arrived at, and the peak is monotonic in
/// the factor, so a bisection lands on it directly.
fn fit_within_peak(plan: &[(f64, &str, f64)], gains: &[f64], budget: f64) -> Vec<f64> {
    let peak = |g: &[f64]| peak_gain_db(&sections_of(plan, g)).0;
    if gains.is_empty() || peak(gains) <= budget {
        return gains.to_vec();
    }
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        let scaled: Vec<f64> = gains.iter().map(|g| g * mid).collect();
        if peak(&scaled) > budget {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    gains.iter().map(|g| g * lo).collect()
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
    let mut want: Vec<f64> = usable.iter().map(|&(cf, _, _)| band_mean(freqs, &curve, cf)).collect();

    // Resolve the measurement's unknown constant by centring the correction on zero
    // over the bands actually being emitted, which makes the EQ loudness-neutral in
    // the mean. The bands are log-spaced, so a plain mean over them is a log-weighted
    // mean over frequency. Anchoring to a fixed window instead would assume that
    // window is already flat — and for this class of speaker the usual candidate,
    // 500-2000 Hz, is where the cabinet resonates.
    if !want.is_empty() {
        let mean = want.iter().sum::<f64>() / want.len() as f64;
        for w in want.iter_mut() {
            *w -= mean;
        }
    }

    // Fit, drop the bands too small to earn a stage, then fit again: dropping a
    // section changes what the rest of the cascade delivers.
    let first = fit_gains(&usable, &want, lim);
    let keep: Vec<usize> = (0..usable.len()).filter(|&i| first[i].abs() > GAIN_FLOOR).collect();
    let plan: Vec<(f64, &str, f64)> = keep.iter().map(|&i| usable[i]).collect();
    let want: Vec<f64> = keep.iter().map(|&i| want[i]).collect();
    let gains = fit_within_peak(&plan, &fit_gains(&plan, &want, lim), REALISED_PEAK_MAX);

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

/// Which two-section alignment a 4th-order high-pass uses.
///
/// Butterworth is the default because it is what has been listened to. Linkwitz-Riley
/// is the more protective choice — 6.02 dB down at the corner against 3.01, and it
/// passes materially less excursion energy below it — and it is what vendor tuning
/// data for at least one laptop chassis actually specifies. Which one sounds right is
/// an ear question, so it is offered rather than decided.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alignment {
    Butterworth,
    LinkwitzRiley,
}

impl Alignment {
    pub fn qs(self) -> [f64; 2] {
        match self {
            Alignment::Butterworth => BUTTERWORTH_Q4,
            Alignment::LinkwitzRiley => LINKWITZ_RILEY_Q4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Alignment::Butterworth => "butterworth",
            Alignment::LinkwitzRiley => "linkwitz-riley",
        }
    }

    /// Attenuation at the corner, in dB.
    pub fn corner_db(self) -> f64 {
        match self {
            Alignment::Butterworth => -3.01,
            Alignment::LinkwitzRiley => -6.02,
        }
    }

    pub fn parse(s: &str) -> Option<Alignment> {
        match s.to_lowercase().replace('_', "-").as_str() {
            "butterworth" | "bw" => Some(Alignment::Butterworth),
            "linkwitz-riley" | "linkwitzriley" | "lr" | "lr4" => Some(Alignment::LinkwitzRiley),
            _ => None,
        }
    }

    pub fn names() -> &'static [&'static str] {
        &["butterworth", "linkwitz-riley"]
    }
}

/// A 4th-order high-pass at `corner`, as two cascaded sections.
///
/// Below a driver's usable range, signal moves the cone without being heard. The chain
/// is: amplitude drives displacement and current, protection detects it, and protection
/// responds by reducing gain *broadband*. So removing that content can leave the rest
/// louder even though the filter only subtracts — at levels where protection is
/// engaging. Quieter than that, a high-pass is simply quieter.
pub fn highpass_bands(corner: f64, align: Alignment) -> Vec<Band> {
    align
        .qs()
        .iter()
        .enumerate()
        .map(|(i, &q)| Band {
            freq: corner,
            gain: 0.0,
            q,
            kind: "bq_highpass",
            node: format!("highpass{}", i + 1),
            note: format!(
                "high-pass {corner:.0} Hz, 4th-order {} ({:.2} dB at the corner), section {} of 2",
                align.name(),
                align.corner_db(),
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

    /// A recording of the sweep with nothing wrong with it: the same samples, delayed
    /// the way `play_and_record` delays them, and otherwise untouched. Whatever
    /// `analyze_response` returns for this is pure method error, because the speaker
    /// and mic in it are perfect.
    #[test]
    fn an_undistorted_recording_measures_flat() {
        let dir = std::env::temp_dir().join(format!("pwtune-align-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sweep_path = dir.join("sweep.wav");
        let rec_path = dir.join("rec.wav");
        let sweep = gen_sweep_i16();
        write_wav(sweep_path.to_str().unwrap(), &sweep).unwrap();

        // play_and_record spawns parecord, sleeps 1 s, then starts paplay.
        let lag = RATE as usize;
        let mut rec: Vec<i16> = vec![0; lag];
        rec.extend_from_slice(&sweep);
        rec.extend(std::iter::repeat_n(0, RATE as usize + RATE as usize / 2));
        write_wav(rec_path.to_str().unwrap(), &rec).unwrap();

        let (freqs, db) =
            analyze_response(sweep_path.to_str().unwrap(), rec_path.to_str().unwrap())
                .unwrap()
                .expect("recording long enough");
        let mut worst = (0.0f64, 0.0f64);
        for &cf in DISPLAY_FREQS {
            let v = value_at(&freqs, &db, cf);
            println!("  {cf:>5} Hz  {v:>+7.1} dB");
            if v.abs() > worst.0.abs() {
                worst = (v, cf);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            worst.0.abs() < 3.0,
            "a perfect recording reads {:+.1} dB at {:.0} Hz — the analysis is not aligned with the reference",
            worst.0,
            worst.1
        );
    }

    /// The end-to-end check: put the sweep through a *known* filter, hand the result
    /// back as a recording, and require the measurement to recover that filter's
    /// computed response. Nothing here is approximated — both sides come from the
    /// same coefficients — so any disagreement is the measurement's.
    #[test]
    fn a_known_filter_is_recovered_from_its_own_sweep() {
        let sections = [
            Biquad::peaking(1000.0, 1.2, 6.0),
            Biquad::peaking(250.0, 1.2, -5.0),
            Biquad::highshelf(6000.0, 0.7, 4.0),
        ];
        let sweep = gen_sweep_i16();
        // Direct form I through each section, as PipeWire would run it.
        let mut signal: Vec<f64> = sweep.iter().map(|&v| v as f64 / 32768.0).collect();
        for s in &sections {
            let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
            for v in signal.iter_mut() {
                let x0 = *v;
                let y0 = s.b0 * x0 + s.b1 * x1 + s.b2 * x2 - s.a1 * y1 - s.a2 * y2;
                x2 = x1;
                x1 = x0;
                y2 = y1;
                y1 = y0;
                *v = y0;
            }
        }

        let dir = std::env::temp_dir().join(format!("pwtune-known-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sp = dir.join("sweep.wav");
        let rp = dir.join("rec.wav");
        write_wav(sp.to_str().unwrap(), &sweep).unwrap();
        let mut rec: Vec<i16> = vec![0; RATE as usize];
        rec.extend(signal.iter().map(|&v| (v * 32768.0).clamp(-32768.0, 32767.0) as i16));
        rec.extend(std::iter::repeat_n(0, RATE as usize));
        write_wav(rp.to_str().unwrap(), &rec).unwrap();

        let (freqs, db) = analyze_response(sp.to_str().unwrap(), rp.to_str().unwrap())
            .unwrap()
            .expect("long enough");

        // Deconvolution recovers the transfer function up to the window's energy loss,
        // so compare shape: remove the mean offset over the compared points.
        let pts: Vec<(f64, f64, f64)> = DISPLAY_FREQS
            .iter()
            .map(|&cf| (cf, value_at(&freqs, &db, cf), cascade_db(&sections, cf)))
            .collect();
        let offset = pts.iter().map(|(_, m, e)| m - e).sum::<f64>() / pts.len() as f64;
        let mut worst = (0.0f64, 0.0f64);
        for (cf, measured, expected) in &pts {
            let err = (measured - offset) - expected;
            println!("  {cf:>5} Hz  measured {:>+6.2}  expected {expected:>+6.2}  err {err:>+5.2}", measured - offset);
            if err.abs() > worst.0.abs() {
                worst = (err, *cf);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            worst.0.abs() < 1.0,
            "recovered response is off by {:+.2} dB at {:.0} Hz",
            worst.0,
            worst.1
        );
    }

    /// Deterministic noise, so the threshold tests do not depend on a random seed.
    fn noise(n: usize, amplitude: f64) -> Vec<f64> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                ((state >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2.0 * amplitude
            })
            .collect()
    }

    fn write_pair(tag: &str, rec: &[f64]) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("pwtune-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (sp, rp) = (dir.join("sweep.wav"), dir.join("rec.wav"));
        write_wav(sp.to_str().unwrap(), &gen_sweep_i16()).unwrap();
        let pcm: Vec<i16> =
            rec.iter().map(|&v| (v * 32768.0).clamp(-32768.0, 32767.0) as i16).collect();
        write_wav(rp.to_str().unwrap(), &pcm).unwrap();
        (sp, rp)
    }

    /// A recording with no sweep in it is refused, rather than returning a curve made
    /// of room noise. This is the check that would have caught the analysis comparing
    /// a reference against silence.
    #[test]
    fn a_recording_without_a_sweep_is_refused() {
        let n = gen_sweep_i16().len() + RATE as usize;
        let (sp, rp) = write_pair("noiseonly", &noise(n, 0.05));
        let err = analyze_response(sp.to_str().unwrap(), rp.to_str().unwrap())
            .expect_err("noise alone must not measure as a response");
        assert!(err.to_string().contains("no sweep"), "{err}");
        let _ = std::fs::remove_dir_all(sp.parent().unwrap());
    }

    /// And a real recording is not refused for being noisy. At 20 dB SNR — poor for a
    /// measurement — the arrival still clears the bar with room to spare, and the
    /// response still comes back flat.
    #[test]
    fn a_noisy_but_real_recording_is_accepted() {
        let sweep = gen_sweep_i16();
        let lag = RATE as usize;
        let mut rec = vec![0.0f64; lag];
        rec.extend(sweep.iter().map(|&v| v as f64 / 32768.0));
        rec.extend(std::iter::repeat_n(0.0, RATE as usize));
        for (r, n) in rec.iter_mut().zip(noise(usize::MAX.min(1 << 24), 0.07)) {
            *r += n;
        }
        let (sp, rp) = write_pair("noisy", &rec);
        let (freqs, db) = analyze_response(sp.to_str().unwrap(), rp.to_str().unwrap())
            .unwrap()
            .expect("long enough");
        let vals: Vec<f64> = DISPLAY_FREQS.iter().map(|&f| value_at(&freqs, &db, f)).collect();
        let spread = vals.iter().cloned().fold(f64::MIN, f64::max)
            - vals.iter().cloned().fold(f64::MAX, f64::min);
        println!("  noisy-recording spread across the display band: {spread:.2} dB");
        assert!(spread < 3.0, "a 20 dB SNR recording should still read flat, got {spread:.2} dB");
        let _ = std::fs::remove_dir_all(sp.parent().unwrap());
    }

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

    /// The contract is the SHAPE the cascade delivers, not its absolute level: an
    /// uncalibrated measurement fixes the response only up to a constant, and any
    /// constant is level, which `--boost` and the speaker's own volume control set.
    /// So a 4 dB deficit must come back as a 4 dB tilt, wherever the anchor puts it.
    #[test]
    fn realised_cascade_delivers_the_shape_the_measurement_asks_for() {
        let (freqs, measured) = measured_with_low_deficit(400.0, 4.0);
        let sections = bands_to_sections(&design_eq(&freqs, &measured, Limits::default()));
        let lifted = [80.0, 150.0, 300.0].map(|cf| cascade_db(&sections, cf));
        let flat = [1000.0, 2000.0].map(|cf| cascade_db(&sections, cf));
        let lift = lifted.iter().sum::<f64>() / 3.0 - flat.iter().sum::<f64>() / 2.0;
        assert!(
            (lift - 4.0).abs() < 0.5,
            "expected a 4 dB tilt between the deficit region and the flat one, got {lift:+.2} dB"
        );
    }

    /// Nothing rings between the band centres: the region the measurement says is
    /// already correct comes out smooth, not rippled by neighbouring bands.
    #[test]
    fn the_region_already_on_target_comes_out_smooth() {
        let (freqs, measured) = measured_with_low_deficit(400.0, 4.0);
        let sections = bands_to_sections(&design_eq(&freqs, &measured, Limits::default()));
        let vals: Vec<f64> = [700.0, 1000.0, 1400.0, 2000.0, 2800.0, 4000.0]
            .iter()
            .map(|&cf| cascade_db(&sections, cf))
            .collect();
        let spread = vals.iter().cloned().fold(f64::MIN, f64::max)
            - vals.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread < 1.5, "ripple of {spread:.2} dB across a region that needs no correction");
    }

    /// Overlapping bands add, so the per-band clamp cannot bound what the chain
    /// delivers. However far the target sits, the cascade's peak stays inside budget.
    #[test]
    fn the_realised_peak_stays_inside_its_budget() {
        for deficit in [4.0, 12.0, 40.0] {
            let (freqs, measured) = measured_with_low_deficit(400.0, deficit);
            let sections = bands_to_sections(&design_eq(&freqs, &measured, Limits::default()));
            if sections.is_empty() {
                continue;
            }
            let (peak, at) = crate::biquad::peak_gain_db(&sections);
            assert!(
                peak <= REALISED_PEAK_MAX + 0.2,
                "a {deficit} dB deficit realised {peak:+.2} dB at {at:.0} Hz, over the \
                 {REALISED_PEAK_MAX} dB budget"
            );
        }
    }

    /// A declared corner keeps the designer half an octave clear of it, so no band
    /// can undo the filter that is there to protect the driver.
    #[test]
    fn no_band_is_placed_where_it_would_undo_the_high_pass() {
        let (freqs, measured) = measured_with_low_deficit(400.0, 12.0);
        let lim = crate::speaker::by_name("laptop").unwrap().limits(Some(150.0));
        for b in design_eq(&freqs, &measured, lim) {
            assert!(
                b.freq >= 150.0 * 2f64.sqrt() - 1e-9,
                "band at {} Hz sits inside a 150 Hz high-pass's transition",
                b.freq
            );
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
