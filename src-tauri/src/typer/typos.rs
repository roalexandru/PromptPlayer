//! §3.2 — typo model.
//!
//! - Rate: ~1 typo per 90 chars (profile-configurable).
//! - Skip rules: prompts <30 chars, within trigger word, first 5 chars.
//! - Type distribution: 80% adjacent-substitution, 15% transposition, 5% omission.
//! - Layout-aware: QWERTY default; AZERTY/Colemak optional.
//! - Detection latency: 1–3 chars uniform.
//! - "I noticed" pause: Normal(350, 100) ms.
//! - Correction: backspace (latency+1), retype.

use rand::Rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypoKind {
    /// Hit an adjacent QWERTY key instead of the target.
    Substitution,
    /// Swap with the next character.
    Transposition,
    /// Skip the character entirely.
    Omission,
}

#[derive(Debug, Clone, Copy)]
pub struct TypoEvent {
    pub kind: TypoKind,
    /// Number of characters typed AFTER the typo before user notices it (1–3).
    pub detection_latency_chars: u8,
}

/// Sample a typo kind per the §3.2 distribution (80/15/5).
pub fn sample_kind<R: Rng + ?Sized>(rng: &mut R) -> TypoKind {
    let r: f64 = rng.gen();
    if r < 0.80 {
        TypoKind::Substitution
    } else if r < 0.95 {
        TypoKind::Transposition
    } else {
        TypoKind::Omission
    }
}

/// Detection latency 1–3 chars uniform (§3.2).
pub fn sample_latency<R: Rng + ?Sized>(rng: &mut R) -> u8 {
    rng.gen_range(1..=3)
}

/// Should we emit a typo at the given position? Applies the §3.2 skip rules.
///
/// - `prompt_len` is the total char count of the body.
/// - `position` is the current 0-indexed char position in the body.
/// - `profile_typo_rate` is the per-char probability.
pub fn should_inject_typo<R: Rng + ?Sized>(
    rng: &mut R,
    position: usize,
    prompt_len: usize,
    profile_typo_rate: f64,
) -> bool {
    if prompt_len < 30 {
        return false;
    }
    if position < 5 {
        return false;
    }
    // Need at least one char after position to even emit a typo + correction.
    if position + 1 >= prompt_len {
        return false;
    }
    rng.gen::<f64>() < profile_typo_rate
}

/// Pick a wrong character that simulates a finger slip.
/// QWERTY adjacency by default. Returns the original char if no neighbor exists
/// (rare, e.g. obscure punctuation).
pub fn adjacent_qwerty<R: Rng + ?Sized>(c: char, rng: &mut R) -> char {
    let lower = c.to_ascii_lowercase();
    let neighbors: &[char] = match lower {
        // top row
        'q' => &['w', 'a', '1', '2'],
        'w' => &['q', 'e', 'a', 's', '2', '3'],
        'e' => &['w', 'r', 's', 'd', '3', '4'],
        'r' => &['e', 't', 'd', 'f', '4', '5'],
        't' => &['r', 'y', 'f', 'g', '5', '6'],
        'y' => &['t', 'u', 'g', 'h', '6', '7'],
        'u' => &['y', 'i', 'h', 'j', '7', '8'],
        'i' => &['u', 'o', 'j', 'k', '8', '9'],
        'o' => &['i', 'p', 'k', 'l', '9', '0'],
        'p' => &['o', 'l', '0', '['],
        // home row
        'a' => &['q', 'w', 's', 'z'],
        's' => &['a', 'd', 'w', 'e', 'z', 'x'],
        'd' => &['s', 'f', 'e', 'r', 'x', 'c'],
        'f' => &['d', 'g', 'r', 't', 'c', 'v'],
        'g' => &['f', 'h', 't', 'y', 'v', 'b'],
        'h' => &['g', 'j', 'y', 'u', 'b', 'n'],
        'j' => &['h', 'k', 'u', 'i', 'n', 'm'],
        'k' => &['j', 'l', 'i', 'o', 'm', ','],
        'l' => &['k', ';', 'o', 'p', ',', '.'],
        // bottom row
        'z' => &['a', 's', 'x'],
        'x' => &['z', 'c', 's', 'd'],
        'c' => &['x', 'v', 'd', 'f'],
        'v' => &['c', 'b', 'f', 'g'],
        'b' => &['v', 'n', 'g', 'h'],
        'n' => &['b', 'm', 'h', 'j'],
        'm' => &['n', ',', 'j', 'k'],
        // digits row
        '1' => &['2', 'q'],
        '2' => &['1', '3', 'q', 'w'],
        '3' => &['2', '4', 'w', 'e'],
        '4' => &['3', '5', 'e', 'r'],
        '5' => &['4', '6', 'r', 't'],
        '6' => &['5', '7', 't', 'y'],
        '7' => &['6', '8', 'y', 'u'],
        '8' => &['7', '9', 'u', 'i'],
        '9' => &['8', '0', 'i', 'o'],
        '0' => &['9', '-', 'o', 'p'],
        // common punctuation
        ' ' => &['c', 'v', 'b', 'n', 'm'],
        ',' => &['m', '.'],
        '.' => &[',', '/'],
        ';' => &['l', '\''],
        _ => &[],
    };
    if neighbors.is_empty() {
        return c;
    }
    let pick = neighbors[rng.gen_range(0..neighbors.len())];
    if c.is_ascii_uppercase() {
        pick.to_ascii_uppercase()
    } else {
        pick
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn skip_rules_short_prompt() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        for _ in 0..1000 {
            assert!(!should_inject_typo(&mut rng, 10, 20, 1.0));
        }
    }

    #[test]
    fn skip_rules_first_5_chars() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        for pos in 0..5 {
            assert!(!should_inject_typo(&mut rng, pos, 100, 1.0));
        }
    }

    #[test]
    fn typo_rate_distribution() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let mut hits = 0;
        let n = 100_000;
        for i in 6..n {
            if should_inject_typo(&mut rng, i, n + 100, 1.0 / 90.0) {
                hits += 1;
            }
        }
        // expected ~1109; allow 30% tolerance
        let expected = (n - 6) as f64 / 90.0;
        assert!(
            (hits as f64 - expected).abs() / expected < 0.3,
            "typo rate off: {} vs ~{}",
            hits,
            expected
        );
    }

    #[test]
    fn kind_distribution_8015_5() {
        let mut rng = ChaCha8Rng::seed_from_u64(3);
        let mut s = 0;
        let mut t = 0;
        let mut o = 0;
        for _ in 0..100_000 {
            match sample_kind(&mut rng) {
                TypoKind::Substitution => s += 1,
                TypoKind::Transposition => t += 1,
                TypoKind::Omission => o += 1,
            }
        }
        let total = (s + t + o) as f64;
        assert!((s as f64 / total - 0.80).abs() < 0.02);
        assert!((t as f64 / total - 0.15).abs() < 0.02);
        assert!((o as f64 / total - 0.05).abs() < 0.02);
    }

    #[test]
    fn adjacent_returns_a_neighbor_for_alpha() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        for c in "abcdefghijklmnopqrstuvwxyz".chars() {
            let n = adjacent_qwerty(c, &mut rng);
            assert_ne!(n, c, "expected different neighbor for {}", c);
        }
    }

    #[test]
    fn adjacent_preserves_case() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let n = adjacent_qwerty('A', &mut rng);
        assert!(n.is_ascii_uppercase());
    }
}
