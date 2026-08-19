//! Second-order sections and their magnitude response, for the `bq_*` builtins a
//! profile is built from.
//!
//! A filter graph is a *cascade*: its response is the product of its sections, so
//! the gain a band asks for and the gain the chain delivers are different numbers
//! wherever two bands overlap. Everything that needs the second number — fitting
//! the band gains in `dsp::design_eq`, reporting peak gain before a chain reaches a
//! speaker — evaluates it here, offline, from the coefficients themselves. That is
//! also why it is reliable: computing the cascade is exact, while recording a
//! deployed chain measures the room, the mic and every other stream into the sink.
//!
//! Coefficients follow the RBJ audio-EQ cookbook with the `Q` parameterization,
//! matching PipeWire's builtin `bq_*` labels. Two conventions matter when reading
//! the numbers: a peaking section hits its full gain at `f0`, while a shelf hits
//! *half* its gain there (`f0` is the shelf midpoint).
use crate::dsp::RATE;
use rustfft::num_complex::Complex;
use std::f64::consts::PI;

/// Butterworth section Qs for a 4th-order response: two cascaded sections.
pub const BUTTERWORTH_Q4: [f64; 2] = [0.541_196_100_146_197, 1.306_562_964_876_377];

/// A section normalized so `a0 == 1`.
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

fn norm(b: [f64; 3], a: [f64; 3]) -> Biquad {
    Biquad { b0: b[0] / a[0], b1: b[1] / a[0], b2: b[2] / a[0], a1: a[1] / a[0], a2: a[2] / a[0] }
}

impl Biquad {
    fn omega(f0: f64) -> (f64, f64, f64) {
        let w = 2.0 * PI * f0 / RATE as f64;
        (w.cos(), w.sin(), w)
    }

    pub fn peaking(f0: f64, q: f64, gain_db: f64) -> Biquad {
        let big_a = 10f64.powf(gain_db / 40.0);
        let (cos_w, sin_w, _) = Self::omega(f0);
        let alpha = sin_w / (2.0 * q);
        norm(
            [1.0 + alpha * big_a, -2.0 * cos_w, 1.0 - alpha * big_a],
            [1.0 + alpha / big_a, -2.0 * cos_w, 1.0 - alpha / big_a],
        )
    }

    pub fn lowshelf(f0: f64, q: f64, gain_db: f64) -> Biquad {
        let big_a = 10f64.powf(gain_db / 40.0);
        let (cos_w, sin_w, _) = Self::omega(f0);
        let alpha = sin_w / (2.0 * q);
        let sq = 2.0 * big_a.sqrt() * alpha;
        let (ap, am) = (big_a + 1.0, big_a - 1.0);
        norm(
            [
                big_a * (ap - am * cos_w + sq),
                2.0 * big_a * (am - ap * cos_w),
                big_a * (ap - am * cos_w - sq),
            ],
            [ap + am * cos_w + sq, -2.0 * (am + ap * cos_w), ap + am * cos_w - sq],
        )
    }

    pub fn highshelf(f0: f64, q: f64, gain_db: f64) -> Biquad {
        let big_a = 10f64.powf(gain_db / 40.0);
        let (cos_w, sin_w, _) = Self::omega(f0);
        let alpha = sin_w / (2.0 * q);
        let sq = 2.0 * big_a.sqrt() * alpha;
        let (ap, am) = (big_a + 1.0, big_a - 1.0);
        norm(
            [
                big_a * (ap + am * cos_w + sq),
                -2.0 * big_a * (am + ap * cos_w),
                big_a * (ap + am * cos_w - sq),
            ],
            [ap - am * cos_w + sq, 2.0 * (am - ap * cos_w), ap - am * cos_w - sq],
        )
    }

    pub fn highpass(f0: f64, q: f64) -> Biquad {
        let (cos_w, sin_w, _) = Self::omega(f0);
        let alpha = sin_w / (2.0 * q);
        let k = (1.0 + cos_w) / 2.0;
        norm([k, -(1.0 + cos_w), k], [1.0 + alpha, -2.0 * cos_w, 1.0 - alpha])
    }

    /// Build from a PipeWire builtin label. `None` for labels with no magnitude
    /// contribution to model, so a caller can evaluate a chain it did not build.
    pub fn from_label(label: &str, f0: f64, q: f64, gain_db: f64) -> Option<Biquad> {
        match label {
            "bq_peaking" => Some(Self::peaking(f0, q, gain_db)),
            "bq_lowshelf" => Some(Self::lowshelf(f0, q, gain_db)),
            "bq_highshelf" => Some(Self::highshelf(f0, q, gain_db)),
            "bq_highpass" => Some(Self::highpass(f0, q)),
            _ => None,
        }
    }

    /// Magnitude at `f` in dB.
    pub fn magnitude_db(&self, f: f64) -> f64 {
        let z = Complex::new(0.0, -2.0 * PI * f / RATE as f64).exp();
        let num = Complex::new(self.b0, 0.0) + z * self.b1 + z * z * self.b2;
        let den = Complex::new(1.0, 0.0) + z * self.a1 + z * z * self.a2;
        20.0 * (num / den).norm().max(1e-30).log10()
    }
}

/// Magnitude of a cascade at `f` in dB — the sum of its sections.
pub fn cascade_db(sections: &[Biquad], f: f64) -> f64 {
    sections.iter().map(|s| s.magnitude_db(f)).sum()
}

/// Log-spaced frequencies over `[lo, hi]`, `per_octave` points per octave.
pub fn log_grid(lo: f64, hi: f64, per_octave: usize) -> Vec<f64> {
    let octaves = (hi / lo).log2();
    let n = (octaves * per_octave as f64).ceil() as usize + 1;
    (0..n).map(|i| lo * 2f64.powf(octaves * i as f64 / (n - 1).max(1) as f64)).collect()
}

/// The cascade's highest gain and where it occurs — the headroom a chain consumes.
pub fn peak_gain_db(sections: &[Biquad]) -> (f64, f64) {
    log_grid(20.0, 20_000.0, 48)
        .into_iter()
        .map(|f| (cascade_db(sections, f), f))
        .fold((f64::NEG_INFINITY, 0.0), |acc, x| if x.0 > acc.0 { x } else { acc })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn peaking_is_exact_at_its_corner() {
        for &g in &[-6.0, -1.5, 3.0, 8.0] {
            let f = Biquad::peaking(1000.0, 1.2, g);
            assert!(close(f.magnitude_db(1000.0), g, 1e-9), "gain {g}");
        }
    }

    #[test]
    fn peaking_rolls_off_to_unity_away_from_its_corner() {
        let f = Biquad::peaking(1000.0, 1.2, 8.0);
        assert!(close(f.magnitude_db(20.0), 0.0, 0.05));
        assert!(close(f.magnitude_db(20_000.0), 0.0, 0.2));
    }

    /// A shelf reaches half its gain at f0 and the full gain in its passband — the
    /// convention that decides what a band's stated gain actually delivers.
    #[test]
    fn shelves_are_half_gain_at_their_corner() {
        let ls = Biquad::lowshelf(80.0, 0.6, 8.0);
        assert!(close(ls.magnitude_db(80.0), 4.0, 1e-6));
        assert!(close(ls.magnitude_db(1.0), 8.0, 0.01));

        let hs = Biquad::highshelf(10.0, 0.7, 24.0);
        assert!(close(hs.magnitude_db(10.0), 12.0, 1e-6));
        assert!(close(hs.magnitude_db(1000.0), 24.0, 0.01));
    }

    #[test]
    fn butterworth_pair_is_minus_three_db_at_the_corner() {
        let hp: Vec<Biquad> = BUTTERWORTH_Q4.iter().map(|&q| Biquad::highpass(100.0, q)).collect();
        assert!(close(cascade_db(&hp, 100.0), -3.0103, 1e-3));
        // 4th order: 24 dB per octave below the corner.
        assert!(close(cascade_db(&hp, 50.0) - cascade_db(&hp, 100.0), -21.1, 0.5));
        assert!(close(cascade_db(&hp, 1000.0), 0.0, 1e-3));
    }

    /// The reason a cascade has to be evaluated rather than assumed: nine bands
    /// spaced under an octave apart at Q=1.2 overlap, so their gains add.
    #[test]
    fn overlapping_bands_sum_beyond_what_each_requests() {
        let bands = [80.0, 150.0, 300.0, 500.0, 1000.0, 2000.0, 3500.0, 6000.0, 10_000.0];
        let cascade: Vec<Biquad> =
            bands.iter().map(|&f| Biquad::peaking(f, 1.2, 4.0)).collect();
        let mid = cascade_db(&cascade, 500.0);
        assert!(mid > 6.5, "expected the cascade to overshoot a +4 dB request, got {mid}");
    }

    #[test]
    fn peak_gain_finds_the_maximum_and_its_frequency() {
        let one = [Biquad::peaking(2000.0, 1.2, 6.0)];
        let (g, f) = peak_gain_db(&one);
        assert!(close(g, 6.0, 0.05), "gain {g}");
        assert!((f - 2000.0).abs() < 40.0, "freq {f}");
    }

    #[test]
    fn unknown_labels_are_skipped_rather_than_guessed() {
        assert!(Biquad::from_label("mixer", 1000.0, 1.0, 0.0).is_none());
        assert!(Biquad::from_label("bq_peaking", 1000.0, 1.0, 3.0).is_some());
    }
}
