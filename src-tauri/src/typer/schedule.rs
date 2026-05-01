//! §3.4 — pre-computed keystroke schedule.
//!
//! When a prompt fires, we pre-compute the entire keystroke schedule before
//! sending the first key. Stolen from Duey.ai's pattern; nobody else does it.
//!
//! Why: main-thread jitter (GC pauses, scheduling, OS interrupts) skews
//! per-key timing if scheduled live. Pre-computing produces a list of
//! `{key, absolute_time_ms}` tuples; the typer thread sleeps to each
//! absolute time. Drift stays bounded; profile statistics actually match.

use crate::typer::distributions::{
    self, jitter, sample_burst_iki, sample_iki, sample_paragraph_pause, sample_pre_submit_pause,
    sample_pre_typing_pause, sample_sentence_pause, sample_typo_noticed_pause, sample_word_pause,
};
use crate::typer::profiles::Profile;
use crate::typer::typos::{
    adjacent_qwerty, sample_kind, sample_latency, should_inject_typo, TypoKind,
};
use rand::Rng;
use serde::Serialize;

/// One scheduled keystroke event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ScheduledKey {
    pub key: Key,
    pub absolute_time_ms: u64,
    pub is_correction: bool,
    pub is_burst: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub enum Key {
    Char(char),
    Backspace,
    Enter,
}

/// Behavior options for `schedule`.
#[derive(Debug, Clone, Copy)]
pub struct ScheduleOptions {
    /// Apply §9.3 RDP-mode adjustments: 30ms IKI floor, ×1.3 multiplier.
    pub rdp_mode: bool,
    /// §3.1 — pre-typing pause after the suppressed `>`. Skip when typing from picker.
    pub include_pre_typing_pause: bool,
}

impl Default for ScheduleOptions {
    fn default() -> Self {
        Self {
            rdp_mode: false,
            include_pre_typing_pause: true,
        }
    }
}

/// Spec §3.4 signature: `schedule(text, profile, rng) -> Vec<ScheduledKey>`.
///
/// Walks `text` once, emits keys + corrections + bursts + pauses,
/// returns a strictly time-monotonic list.
pub fn schedule<R: Rng + ?Sized>(
    text: &str,
    profile: &Profile,
    options: &ScheduleOptions,
    rng: &mut R,
) -> Vec<ScheduledKey> {
    let chars: Vec<char> = text.chars().collect();
    let prompt_len = chars.len();
    let mut keys: Vec<ScheduledKey> = Vec::with_capacity(chars.len() + 8);
    let mut cursor: f64 = 0.0;

    if options.include_pre_typing_pause {
        cursor += sample_pre_typing_pause(rng, profile.pause_variance_scale);
    }

    // Burst state: §3.1 — every 6–14 words, drop into burst for 8–20 chars.
    let mut burst_remaining: usize = 0;
    let mut words_since_last_burst: usize = 0;
    let mut next_burst_at_words: usize = sample_burst_word_count(rng);

    let mut text_pos: usize = 0;
    let mut typed_so_far: usize = 0; // counts emitted CHARACTERS (excluding backspaces)

    while text_pos < chars.len() {
        // Boundary pause BEFORE this char (if any).
        let boundary = classify_boundary(&chars, text_pos);
        if let Some(b) = boundary {
            cursor += match b {
                Boundary::Word => sample_word_pause(rng, profile.pause_variance_scale),
                Boundary::Sentence => sample_sentence_pause(rng, profile.pause_variance_scale),
                Boundary::Paragraph => sample_paragraph_pause(rng, profile.pause_variance_scale),
            };
        }

        // Typo decision (subject to §3.2 skip rules).
        let inject_typo = profile.typos_enabled
            && should_inject_typo(rng, text_pos, prompt_len, profile.typo_rate);

        if inject_typo {
            let kind = sample_kind(rng);
            emit_typo_sequence(
                &chars,
                &mut text_pos,
                &mut cursor,
                &mut keys,
                &mut burst_remaining,
                &mut words_since_last_burst,
                &mut next_burst_at_words,
                kind,
                profile,
                options,
                rng,
            );
        } else {
            // Plain emit.
            let in_burst = burst_remaining > 0;
            let iki = if in_burst {
                sample_burst_iki(rng)
            } else {
                sample_iki(rng)
            };
            cursor += apply_iki_adjustments(iki, profile, options, rng);
            keys.push(ScheduledKey {
                key: Key::Char(chars[text_pos]),
                absolute_time_ms: cursor.round() as u64,
                is_correction: false,
                is_burst: in_burst,
            });
            update_burst_state(
                chars[text_pos],
                &mut burst_remaining,
                &mut words_since_last_burst,
                &mut next_burst_at_words,
                rng,
            );
            text_pos += 1;
            typed_so_far += 1;
        }
    }

    // §3.1 pre-submit pause (single most realism-defining touch).
    if profile.pre_submit_pause_enabled {
        cursor += sample_pre_submit_pause(rng, profile.pause_variance_scale);
    }

    if profile.send_final_enter {
        keys.push(ScheduledKey {
            key: Key::Enter,
            absolute_time_ms: cursor.round() as u64,
            is_correction: false,
            is_burst: false,
        });
    }

    let _ = typed_so_far;
    keys
}

fn sample_burst_word_count<R: Rng + ?Sized>(rng: &mut R) -> usize {
    rng.gen_range(6..=14)
}

fn sample_burst_char_count<R: Rng + ?Sized>(rng: &mut R) -> usize {
    rng.gen_range(8..=20)
}

#[derive(Debug, Clone, Copy)]
enum Boundary {
    Word,
    Sentence,
    Paragraph,
}

/// Classify the boundary preceding chars[i].
/// Returns None if no boundary applies.
fn classify_boundary(chars: &[char], i: usize) -> Option<Boundary> {
    if i == 0 {
        return None;
    }
    let prev = chars[i - 1];
    // Paragraph: previous two chars are both '\n', and we're now typing something else.
    if prev == '\n' && i >= 2 && chars[i - 2] == '\n' && chars[i] != '\n' {
        return Some(Boundary::Paragraph);
    }
    // Sentence: previous is space, char before that is .!?, current is non-space.
    if prev == ' ' && i >= 2 && matches!(chars[i - 2], '.' | '!' | '?') && chars[i] != ' ' {
        return Some(Boundary::Sentence);
    }
    // Word: previous is space, current is non-space (and not already a sentence).
    if prev == ' ' && chars[i] != ' ' {
        return Some(Boundary::Word);
    }
    None
}

fn apply_iki_adjustments<R: Rng + ?Sized>(
    base_iki_ms: f64,
    profile: &Profile,
    options: &ScheduleOptions,
    rng: &mut R,
) -> f64 {
    let mut v = base_iki_ms * profile.iki_scale;
    if options.rdp_mode {
        v *= 1.3;
        v = v.max(30.0);
    }
    v += jitter(rng);
    v.max(distributions::IKI_MIN_MS)
}

fn update_burst_state<R: Rng + ?Sized>(
    just_typed: char,
    burst_remaining: &mut usize,
    words_since_last_burst: &mut usize,
    next_burst_at_words: &mut usize,
    rng: &mut R,
) {
    if *burst_remaining > 0 {
        *burst_remaining -= 1;
        return;
    }
    if just_typed == ' ' {
        *words_since_last_burst += 1;
        if *words_since_last_burst >= *next_burst_at_words {
            *burst_remaining = sample_burst_char_count(rng);
            *words_since_last_burst = 0;
            *next_burst_at_words = sample_burst_word_count(rng);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_typo_sequence<R: Rng + ?Sized>(
    chars: &[char],
    text_pos: &mut usize,
    cursor: &mut f64,
    keys: &mut Vec<ScheduledKey>,
    burst_remaining: &mut usize,
    words_since_last_burst: &mut usize,
    next_burst_at_words: &mut usize,
    kind: TypoKind,
    profile: &Profile,
    options: &ScheduleOptions,
    rng: &mut R,
) {
    let target = chars[*text_pos];
    let in_burst = *burst_remaining > 0;
    let latency = sample_latency(rng) as usize;
    // How many ORIGINAL chars are consumed by this typo+correction.
    // Substitution: 1 wrong char, then `latency` more original chars typed, then notice & retype all `latency+1`.
    // Transposition: 2 chars swapped (wrong = chars[pos+1] then chars[pos]); also followed by `latency` chars + correct.
    // Omission: skip chars[pos] entirely; type next `latency` chars; notice; backspace `latency`, retype chars[pos..pos+latency+1] correctly. So the "missed" char enters at correction time.
    //
    // To keep state coherent, we cap latency at how many original chars remain after `text_pos`.
    let remaining = chars.len() - *text_pos - 1;
    let latency = latency.min(remaining);

    match kind {
        TypoKind::Substitution => {
            let wrong = adjacent_qwerty(target, rng);
            // Emit wrong char in place of target.
            let iki = if in_burst {
                sample_burst_iki(rng)
            } else {
                sample_iki(rng)
            };
            *cursor += apply_iki_adjustments(iki, profile, options, rng);
            keys.push(ScheduledKey {
                key: Key::Char(wrong),
                absolute_time_ms: cursor.round() as u64,
                is_correction: false,
                is_burst: in_burst,
            });
            update_burst_state(
                target,
                burst_remaining,
                words_since_last_burst,
                next_burst_at_words,
                rng,
            );
            *text_pos += 1;

            // Type `latency` more correct chars naturally.
            for _ in 0..latency {
                if *text_pos >= chars.len() {
                    break;
                }
                let in_b = *burst_remaining > 0;
                let iki = if in_b {
                    sample_burst_iki(rng)
                } else {
                    sample_iki(rng)
                };
                *cursor += apply_iki_adjustments(iki, profile, options, rng);
                let c = chars[*text_pos];
                keys.push(ScheduledKey {
                    key: Key::Char(c),
                    absolute_time_ms: cursor.round() as u64,
                    is_correction: false,
                    is_burst: in_b,
                });
                update_burst_state(
                    c,
                    burst_remaining,
                    words_since_last_burst,
                    next_burst_at_words,
                    rng,
                );
                *text_pos += 1;
            }

            // "I noticed" pause.
            *cursor += sample_typo_noticed_pause(rng);

            // Backspace (latency + 1) times — fast hammer.
            for _ in 0..(latency + 1) {
                *cursor += backspace_iki(rng);
                keys.push(ScheduledKey {
                    key: Key::Backspace,
                    absolute_time_ms: cursor.round() as u64,
                    is_correction: true,
                    is_burst: false,
                });
            }

            // Retype the (latency + 1) chars correctly: from text_pos - (latency+1) up to text_pos - 1 (inclusive).
            let retype_start = text_pos.saturating_sub(latency + 1);
            for j in retype_start..*text_pos {
                let in_b = *burst_remaining > 0;
                let iki = if in_b {
                    sample_burst_iki(rng)
                } else {
                    sample_iki(rng)
                };
                *cursor += apply_iki_adjustments(iki, profile, options, rng);
                keys.push(ScheduledKey {
                    key: Key::Char(chars[j]),
                    absolute_time_ms: cursor.round() as u64,
                    is_correction: true,
                    is_burst: in_b,
                });
            }
        }
        TypoKind::Transposition => {
            // Swap chars[pos] and chars[pos+1]: type chars[pos+1] first, then chars[pos].
            if *text_pos + 1 >= chars.len() {
                // Fall back to a plain emit.
                let iki = if in_burst {
                    sample_burst_iki(rng)
                } else {
                    sample_iki(rng)
                };
                *cursor += apply_iki_adjustments(iki, profile, options, rng);
                keys.push(ScheduledKey {
                    key: Key::Char(target),
                    absolute_time_ms: cursor.round() as u64,
                    is_correction: false,
                    is_burst: in_burst,
                });
                update_burst_state(
                    target,
                    burst_remaining,
                    words_since_last_burst,
                    next_burst_at_words,
                    rng,
                );
                *text_pos += 1;
                return;
            }
            let next = chars[*text_pos + 1];
            // Emit swapped pair.
            for &c in &[next, target] {
                let in_b = *burst_remaining > 0;
                let iki = if in_b {
                    sample_burst_iki(rng)
                } else {
                    sample_iki(rng)
                };
                *cursor += apply_iki_adjustments(iki, profile, options, rng);
                keys.push(ScheduledKey {
                    key: Key::Char(c),
                    absolute_time_ms: cursor.round() as u64,
                    is_correction: false,
                    is_burst: in_b,
                });
                update_burst_state(
                    c,
                    burst_remaining,
                    words_since_last_burst,
                    next_burst_at_words,
                    rng,
                );
            }
            *text_pos += 2;

            // Latency, notice, backspace 2, retype correctly.
            for _ in 0..latency {
                if *text_pos >= chars.len() {
                    break;
                }
                let in_b = *burst_remaining > 0;
                let iki = if in_b {
                    sample_burst_iki(rng)
                } else {
                    sample_iki(rng)
                };
                *cursor += apply_iki_adjustments(iki, profile, options, rng);
                let c = chars[*text_pos];
                keys.push(ScheduledKey {
                    key: Key::Char(c),
                    absolute_time_ms: cursor.round() as u64,
                    is_correction: false,
                    is_burst: in_b,
                });
                update_burst_state(
                    c,
                    burst_remaining,
                    words_since_last_burst,
                    next_burst_at_words,
                    rng,
                );
                *text_pos += 1;
            }

            *cursor += sample_typo_noticed_pause(rng);

            for _ in 0..(latency + 2) {
                *cursor += backspace_iki(rng);
                keys.push(ScheduledKey {
                    key: Key::Backspace,
                    absolute_time_ms: cursor.round() as u64,
                    is_correction: true,
                    is_burst: false,
                });
            }

            let retype_start = text_pos.saturating_sub(latency + 2);
            for j in retype_start..*text_pos {
                let in_b = *burst_remaining > 0;
                let iki = if in_b {
                    sample_burst_iki(rng)
                } else {
                    sample_iki(rng)
                };
                *cursor += apply_iki_adjustments(iki, profile, options, rng);
                keys.push(ScheduledKey {
                    key: Key::Char(chars[j]),
                    absolute_time_ms: cursor.round() as u64,
                    is_correction: true,
                    is_burst: in_b,
                });
            }
        }
        TypoKind::Omission => {
            // Skip chars[pos] entirely. Advance text_pos by 1 without emitting.
            *text_pos += 1;
            // Type `latency` more correct chars naturally.
            for _ in 0..latency {
                if *text_pos >= chars.len() {
                    break;
                }
                let in_b = *burst_remaining > 0;
                let iki = if in_b {
                    sample_burst_iki(rng)
                } else {
                    sample_iki(rng)
                };
                *cursor += apply_iki_adjustments(iki, profile, options, rng);
                let c = chars[*text_pos];
                keys.push(ScheduledKey {
                    key: Key::Char(c),
                    absolute_time_ms: cursor.round() as u64,
                    is_correction: false,
                    is_burst: in_b,
                });
                update_burst_state(
                    c,
                    burst_remaining,
                    words_since_last_burst,
                    next_burst_at_words,
                    rng,
                );
                *text_pos += 1;
            }

            *cursor += sample_typo_noticed_pause(rng);

            // Backspace `latency` times (we never typed the omitted char, so only the followups need removing).
            for _ in 0..latency {
                *cursor += backspace_iki(rng);
                keys.push(ScheduledKey {
                    key: Key::Backspace,
                    absolute_time_ms: cursor.round() as u64,
                    is_correction: true,
                    is_burst: false,
                });
            }

            // Retype: omitted char + the `latency` correct followups.
            let retype_start = text_pos.saturating_sub(latency + 1);
            for j in retype_start..*text_pos {
                let in_b = *burst_remaining > 0;
                let iki = if in_b {
                    sample_burst_iki(rng)
                } else {
                    sample_iki(rng)
                };
                *cursor += apply_iki_adjustments(iki, profile, options, rng);
                keys.push(ScheduledKey {
                    key: Key::Char(chars[j]),
                    absolute_time_ms: cursor.round() as u64,
                    is_correction: true,
                    is_burst: in_b,
                });
            }
        }
    }
}

/// Backspace cadence: real users hammer; ~50ms with mild jitter.
fn backspace_iki<R: Rng + ?Sized>(rng: &mut R) -> f64 {
    let base = 50.0;
    base + rng.gen_range(-10.0..=15.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typer::profiles::Profile;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn det_rng() -> ChaCha8Rng {
        ChaCha8Rng::seed_from_u64(123)
    }

    #[test]
    fn deterministic_with_seed() {
        let text = "Build me a React component that displays a list of users with avatars.";
        let profile = Profile::SALES_ENGINEER;
        let opts = ScheduleOptions::default();
        let a = schedule(text, &profile, &opts, &mut det_rng());
        let b = schedule(text, &profile, &opts, &mut det_rng());
        assert_eq!(a, b, "schedule must be deterministic given a seeded RNG");
    }

    #[test]
    fn times_are_monotonic_non_decreasing() {
        let text = "Build me a React component that displays a list of users with avatars and online status.";
        let profile = Profile::SALES_ENGINEER;
        let opts = ScheduleOptions::default();
        let s = schedule(text, &profile, &opts, &mut det_rng());
        for w in s.windows(2) {
            assert!(w[0].absolute_time_ms <= w[1].absolute_time_ms);
        }
    }

    #[test]
    fn no_typos_below_threshold_or_in_first_5_chars() {
        // Short prompt: never any typos.
        let mut profile = Profile::SALES_ENGINEER;
        profile.typo_rate = 1.0;
        let opts = ScheduleOptions::default();
        let s = schedule("hi there", &profile, &opts, &mut det_rng());
        assert!(!s.iter().any(|k| k.is_correction));
        assert!(!s.iter().any(|k| matches!(k.key, Key::Backspace)));
    }

    #[test]
    fn rdp_mode_increases_total_time() {
        let text = "Build me a React component that displays a list of users with avatars and online status.";
        let mut profile = Profile::SALES_ENGINEER;
        profile.typos_enabled = false;
        let normal = schedule(
            text,
            &profile,
            &ScheduleOptions {
                rdp_mode: false,
                include_pre_typing_pause: false,
            },
            &mut det_rng(),
        );
        let rdp = schedule(
            text,
            &profile,
            &ScheduleOptions {
                rdp_mode: true,
                include_pre_typing_pause: false,
            },
            &mut det_rng(),
        );
        let normal_end = normal.last().unwrap().absolute_time_ms;
        let rdp_end = rdp.last().unwrap().absolute_time_ms;
        assert!(
            rdp_end as f64 > normal_end as f64 * 1.15,
            "RDP mode should be ≥1.15× slower; got {} vs {}",
            rdp_end,
            normal_end
        );
    }

    #[test]
    fn pre_typing_pause_when_enabled() {
        let mut profile = Profile::SALES_ENGINEER;
        profile.typos_enabled = false;
        let with = schedule(
            "hello world this is text",
            &profile,
            &ScheduleOptions {
                rdp_mode: false,
                include_pre_typing_pause: true,
            },
            &mut det_rng(),
        );
        let without = schedule(
            "hello world this is text",
            &profile,
            &ScheduleOptions {
                rdp_mode: false,
                include_pre_typing_pause: false,
            },
            &mut det_rng(),
        );
        // First key with pre-typing should be ≥ ~700ms; without should be small (a single IKI).
        assert!(
            with[0].absolute_time_ms > 700,
            "pre-typing pause expected; got {}",
            with[0].absolute_time_ms
        );
        assert!(
            without[0].absolute_time_ms < 700,
            "no pre-typing pause expected; got {}",
            without[0].absolute_time_ms
        );
    }

    #[test]
    fn typos_produce_corrections() {
        let mut profile = Profile::SALES_ENGINEER;
        profile.typo_rate = 0.5; // very high to reliably trigger
        let text = "the quick brown fox jumps over the lazy dog and then the cat ran away too";
        let opts = ScheduleOptions::default();
        let s = schedule(text, &profile, &opts, &mut det_rng());
        let backspaces = s.iter().filter(|k| matches!(k.key, Key::Backspace)).count();
        assert!(backspaces > 0, "high typo rate should produce backspaces");
        let corrections = s.iter().filter(|k| k.is_correction).count();
        assert!(corrections > 0);
    }

    #[test]
    fn final_enter_emitted_when_enabled() {
        let mut profile = Profile::SALES_ENGINEER;
        profile.typos_enabled = false;
        profile.send_final_enter = true;
        let s = schedule(
            "hello world",
            &profile,
            &ScheduleOptions::default(),
            &mut det_rng(),
        );
        assert!(matches!(s.last().unwrap().key, Key::Enter));
    }
}
