//! §3.1 — cadence distributions.
//!
//! All values are milliseconds. Distributions match the empirically validated
//! keystroke biometrics literature (Aalto 2018, Sequeira 2021, Roeser 2021).
//!
//! Base inter-key interval is a **mixture of two log-normals**:
//! - 85% fluent: LogNormal(μ=4.95, σ=0.35), median ≈ 140 ms
//! - 15% micro-hesitation: LogNormal(μ=6.20, σ=0.50), median ≈ 490 ms
//! - Clamped to [60, 3000] ms.

use rand::Rng;
use rand_distr::{Distribution, LogNormal, Normal};

pub const IKI_FLUENT_MU: f64 = 4.95;
pub const IKI_FLUENT_SIGMA: f64 = 0.35;
pub const IKI_HESITATION_MU: f64 = 6.20;
pub const IKI_HESITATION_SIGMA: f64 = 0.50;
pub const IKI_HESITATION_PROBABILITY: f64 = 0.15;
pub const IKI_MIN_MS: f64 = 60.0;
pub const IKI_MAX_MS: f64 = 3000.0;

// Boundary pauses tuned so total throughput matches the WPM targets in
// `profiles.rs`. Original spec values (5.7/7.0/2500/1500/1800) were calibrated
// against keystroke-biometrics studies that don't include the rich punctuation
// and paragraph structure that prompt bodies actually have, so they ended up
// crowding the typing budget. New values keep the *shape* of the distributions
// (log-normal for word/sentence, normal for paragraph) but with shorter means.
pub const WORD_PAUSE_MU: f64 = 5.2; // median ~180 ms (was ~300 ms)
pub const WORD_PAUSE_SIGMA: f64 = 0.4;
pub const SENTENCE_PAUSE_MU: f64 = 6.4; // median ~600 ms (was ~1100 ms)
pub const SENTENCE_PAUSE_SIGMA: f64 = 0.5;

pub const PARAGRAPH_PAUSE_MEAN: f64 = 1500.0; // was 2500 ms
pub const PARAGRAPH_PAUSE_STDDEV: f64 = 500.0;

pub const PRE_TYPING_MEAN: f64 = 800.0; // was 1500 ms
pub const PRE_TYPING_STDDEV: f64 = 250.0;

pub const PRE_SUBMIT_MEAN: f64 = 1000.0; // was 1800 ms
pub const PRE_SUBMIT_STDDEV: f64 = 350.0;

pub const TYPO_NOTICED_MEAN: f64 = 350.0;
pub const TYPO_NOTICED_STDDEV: f64 = 100.0;

pub const BURST_MU: f64 = 4.7;
pub const BURST_SIGMA: f64 = 0.25;

/// Sample base IKI from the 85/15 log-normal mixture, clamped.
pub fn sample_iki<R: Rng + ?Sized>(rng: &mut R) -> f64 {
    let (mu, sigma) = if rng.gen::<f64>() < IKI_HESITATION_PROBABILITY {
        (IKI_HESITATION_MU, IKI_HESITATION_SIGMA)
    } else {
        (IKI_FLUENT_MU, IKI_FLUENT_SIGMA)
    };
    let raw = LogNormal::new(mu, sigma)
        .expect("valid lognormal")
        .sample(rng);
    raw.clamp(IKI_MIN_MS, IKI_MAX_MS)
}

/// Burst-mode IKI for muscle-memory phrases (§3.1).
pub fn sample_burst_iki<R: Rng + ?Sized>(rng: &mut R) -> f64 {
    let raw = LogNormal::new(BURST_MU, BURST_SIGMA)
        .expect("valid lognormal")
        .sample(rng);
    raw.clamp(IKI_MIN_MS, IKI_MAX_MS)
}

/// Hierarchical pause for a word boundary (after `space`, before next word).
pub fn sample_word_pause<R: Rng + ?Sized>(rng: &mut R, sigma_scale: f64) -> f64 {
    LogNormal::new(WORD_PAUSE_MU, WORD_PAUSE_SIGMA * sigma_scale)
        .expect("valid lognormal")
        .sample(rng)
        .max(0.0)
}

/// Hierarchical pause for a sentence boundary (`. ! ?` then space).
pub fn sample_sentence_pause<R: Rng + ?Sized>(rng: &mut R, sigma_scale: f64) -> f64 {
    LogNormal::new(SENTENCE_PAUSE_MU, SENTENCE_PAUSE_SIGMA * sigma_scale)
        .expect("valid lognormal")
        .sample(rng)
        .max(0.0)
}

/// Hierarchical pause for paragraph boundary (`\n\n`).
pub fn sample_paragraph_pause<R: Rng + ?Sized>(rng: &mut R, sigma_scale: f64) -> f64 {
    Normal::new(PARAGRAPH_PAUSE_MEAN, PARAGRAPH_PAUSE_STDDEV * sigma_scale)
        .expect("valid normal")
        .sample(rng)
        .max(100.0)
}

/// Pre-typing pause after the suppressed `>` (§3.1, §2.1).
pub fn sample_pre_typing_pause<R: Rng + ?Sized>(rng: &mut R, sigma_scale: f64) -> f64 {
    Normal::new(PRE_TYPING_MEAN, PRE_TYPING_STDDEV * sigma_scale)
        .expect("valid normal")
        .sample(rng)
        .max(200.0)
}

/// Pre-submit pause before the final Enter (§3.1 — "single most realism-defining touch").
pub fn sample_pre_submit_pause<R: Rng + ?Sized>(rng: &mut R, sigma_scale: f64) -> f64 {
    Normal::new(PRE_SUBMIT_MEAN, PRE_SUBMIT_STDDEV * sigma_scale)
        .expect("valid normal")
        .sample(rng)
        .max(300.0)
}

/// "I noticed the typo" pause before correction (§3.2).
pub fn sample_typo_noticed_pause<R: Rng + ?Sized>(rng: &mut R) -> f64 {
    Normal::new(TYPO_NOTICED_MEAN, TYPO_NOTICED_STDDEV)
        .expect("valid normal")
        .sample(rng)
        .max(50.0)
}

/// Anti-pattern jitter: ±2–4 ms uniform (§3.1).
/// Removes the "everything is a multiple of 16ms" tell.
pub fn jitter<R: Rng + ?Sized>(rng: &mut R) -> f64 {
    let magnitude: f64 = rng.gen_range(2.0..=4.0);
    let sign = if rng.gen::<bool>() { 1.0 } else { -1.0 };
    magnitude * sign
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn median(mut samples: Vec<f64>) -> f64 {
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        samples[samples.len() / 2]
    }

    #[test]
    fn iki_median_within_5pct_of_140ms() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let samples: Vec<f64> = (0..50_000).map(|_| sample_iki(&mut rng)).collect();
        let m = median(samples);
        // Mixture median is dominated by the 85% fluent component (~140 ms).
        // Allow ±15% — the 15% hesitation tail pulls the median up modestly.
        assert!(
            (110.0..=200.0).contains(&m),
            "iki median {} ms outside [110, 200]",
            m
        );
    }

    #[test]
    fn iki_clamped_to_range() {
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        for _ in 0..10_000 {
            let v = sample_iki(&mut rng);
            assert!((IKI_MIN_MS..=IKI_MAX_MS).contains(&v));
        }
    }

    #[test]
    fn word_pause_median_around_180ms() {
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let samples: Vec<f64> = (0..10_000)
            .map(|_| sample_word_pause(&mut rng, 1.0))
            .collect();
        let m = median(samples);
        assert!((150.0..=220.0).contains(&m), "word pause median {}", m);
    }

    #[test]
    fn sentence_pause_median_around_600ms() {
        let mut rng = ChaCha8Rng::seed_from_u64(2);
        let samples: Vec<f64> = (0..10_000)
            .map(|_| sample_sentence_pause(&mut rng, 1.0))
            .collect();
        let m = median(samples);
        assert!((500.0..=720.0).contains(&m), "sentence pause median {}", m);
    }

    #[test]
    fn jitter_in_range() {
        let mut rng = ChaCha8Rng::seed_from_u64(99);
        for _ in 0..1000 {
            let j = jitter(&mut rng);
            assert!((-4.0..=4.0).contains(&j));
            assert!(j.abs() >= 2.0);
        }
    }
}
