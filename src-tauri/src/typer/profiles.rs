//! §3.3 — typing profiles. Three named presets; per-prompt YAML overrides.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, specta::Type)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileKind {
    #[default]
    SalesEngineer,
    FastPresenter,
    ThoughtfulCeo,
    Custom,
}

/// Per-profile dials for the IKI median, pause mean and σ, plus a
/// profile-aware floor — the global one swallowed any scale below ~0.43.
#[derive(Debug, Clone, Copy)]
pub struct Profile {
    pub kind: ProfileKind,
    pub iki_scale: f64,
    pub iki_min_ms: f64,
    pub typo_rate: f64,
    pub pause_scale: f64,
    pub pause_variance_scale: f64,
    pub burst_enabled: bool,
    pub typos_enabled: bool,
    pub pre_submit_pause_enabled: bool,
    pub send_final_enter: bool,
    /// Self-edits: type part of a word, hesitate, delete it, retype. Modelled
    /// separately from typos because it is a *thinking* artifact, not a motor
    /// error — which is why the CEO profile has the most and Fast Presenter
    /// has none.
    pub rephrase_enabled: bool,
    /// Probability of a false start at any given word boundary.
    pub rephrase_rate: f64,
}

impl Default for Profile {
    fn default() -> Self {
        Self::SALES_ENGINEER
    }
}

impl Profile {
    /// Sales Engineer — calm, plausible-human, ~75 WPM. The 60 ms floor is the
    /// realistic lower bound for sustained human typing.
    pub const SALES_ENGINEER: Profile = Profile {
        kind: ProfileKind::SalesEngineer,
        iki_scale: 0.50, // median ~70 ms
        iki_min_ms: 60.0,
        typo_rate: 1.0 / 90.0,
        pause_scale: 0.9, // word pauses ~160 ms median
        pause_variance_scale: 1.0,
        burst_enabled: true,
        typos_enabled: true,
        pre_submit_pause_enabled: true,
        send_final_enter: false,
        rephrase_enabled: true,
        rephrase_rate: 0.012,
    };

    /// Fast Presenter — confident demo cadence, ~180 WPM. The lower
    /// `iki_min_ms` is what stops the global floor clamping the scale away.
    pub const FAST_PRESENTER: Profile = Profile {
        kind: ProfileKind::FastPresenter,
        iki_scale: 0.22, // median ~31 ms
        iki_min_ms: 22.0,
        typo_rate: 1.0 / 200.0,
        pause_scale: 0.4, // word ~72 ms, sentence ~240 ms
        pause_variance_scale: 0.4,
        burst_enabled: true,
        typos_enabled: true,
        pre_submit_pause_enabled: true,
        send_final_enter: false,
        // A confident, time-pressed presenter does not visibly rethink.
        rephrase_enabled: false,
        rephrase_rate: 0.0,
    };

    /// Thoughtful CEO — deliberate cadence with longer reflection at sentence
    /// boundaries. Target ~45 WPM.
    pub const THOUGHTFUL_CEO: Profile = Profile {
        kind: ProfileKind::ThoughtfulCeo,
        iki_scale: 0.85, // median ~119 ms
        iki_min_ms: 60.0,
        typo_rate: 1.0 / 120.0,
        pause_scale: 1.3, // longer boundary pauses
        pause_variance_scale: 1.5,
        burst_enabled: true,
        typos_enabled: true,
        pre_submit_pause_enabled: true,
        send_final_enter: false,
        // §3.3 lists this profile's pause variance as "high (more re-reads)";
        // false starts are what "re-reads" actually look like on screen.
        rephrase_enabled: true,
        rephrase_rate: 0.03,
    };

    pub fn from_kind(kind: ProfileKind) -> Self {
        match kind {
            ProfileKind::SalesEngineer => Self::SALES_ENGINEER,
            ProfileKind::FastPresenter => Self::FAST_PRESENTER,
            ProfileKind::ThoughtfulCeo => Self::THOUGHTFUL_CEO,
            ProfileKind::Custom => Self::SALES_ENGINEER, // user-customized via overrides
        }
    }

    /// Apply per-prompt YAML overrides (§7.1 `typing-overrides:` block).
    pub fn with_overrides(mut self, overrides: &TypingOverrides) -> Self {
        let overrides = overrides.normalized();
        if let Some(median_ms) = overrides.iki_median_ms {
            // Retarget IKI median: scale relative to baseline 140ms.
            self.iki_scale = median_ms / 140.0;
        }
        if let Some(rate) = overrides.typo_rate {
            self.typo_rate = rate;
        }
        if let Some(scale) = overrides.pause_variance_scale {
            self.pause_variance_scale = scale;
        }
        if let Some(b) = overrides.burst_enabled {
            self.burst_enabled = b;
        }
        if let Some(t) = overrides.typos_enabled {
            self.typos_enabled = t;
        }
        if let Some(p) = overrides.pre_submit_pause_enabled {
            self.pre_submit_pause_enabled = p;
        }
        if let Some(e) = overrides.send_final_enter {
            self.send_final_enter = e;
        }
        if let Some(rate) = overrides.rephrase_rate {
            self.rephrase_rate = rate;
            self.rephrase_enabled = rate > 0.0;
        }
        if let Some(e) = overrides.rephrase_enabled {
            self.rephrase_enabled = e;
        }
        if overrides.iki_median_ms.is_some()
            || overrides.typo_rate.is_some()
            || overrides.pause_variance_scale.is_some()
            || overrides.rephrase_rate.is_some()
        {
            self.kind = ProfileKind::Custom;
        }
        self
    }
}

/// §7.1 `typing-overrides:` mapping.
#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "kebab-case", default)]
pub struct TypingOverrides {
    pub iki_median_ms: Option<f64>,
    pub typo_rate: Option<f64>,
    pub pause_variance_scale: Option<f64>,
    pub burst_enabled: Option<bool>,
    pub typos_enabled: Option<bool>,
    pub pre_submit_pause_enabled: Option<bool>,
    pub send_final_enter: Option<bool>,
    pub rephrase_enabled: Option<bool>,
    pub rephrase_rate: Option<f64>,
}

impl TypingOverrides {
    pub fn normalized(&self) -> Self {
        Self {
            iki_median_ms: positive_finite(self.iki_median_ms),
            typo_rate: self
                .typo_rate
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 1.0)),
            pause_variance_scale: positive_finite(self.pause_variance_scale),
            burst_enabled: self.burst_enabled,
            typos_enabled: self.typos_enabled,
            pre_submit_pause_enabled: self.pre_submit_pause_enabled,
            send_final_enter: self.send_final_enter,
            rephrase_enabled: self.rephrase_enabled,
            rephrase_rate: self
                .rephrase_rate
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 1.0)),
        }
    }
}

fn positive_finite(v: Option<f64>) -> Option<f64> {
    v.filter(|v| v.is_finite() && *v > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_baseline_profiles_have_distinct_iki_scales() {
        let s = Profile::SALES_ENGINEER.iki_scale;
        let f = Profile::FAST_PRESENTER.iki_scale;
        let c = Profile::THOUGHTFUL_CEO.iki_scale;
        assert!(f < s && s < c, "fast < sales < ceo: {} {} {}", f, s, c);
    }

    #[test]
    fn overrides_apply_and_flip_to_custom() {
        let p = Profile::SALES_ENGINEER.with_overrides(&TypingOverrides {
            iki_median_ms: Some(80.0),
            ..Default::default()
        });
        assert_eq!(p.kind, ProfileKind::Custom);
        assert!((p.iki_scale - 80.0 / 140.0).abs() < 1e-9);
    }

    #[test]
    fn numeric_overrides_are_normalized_before_use() {
        let p = Profile::SALES_ENGINEER.with_overrides(&TypingOverrides {
            iki_median_ms: Some(f64::NAN),
            typo_rate: Some(2.5),
            pause_variance_scale: Some(0.0),
            ..Default::default()
        });
        assert_eq!(p.iki_scale, Profile::SALES_ENGINEER.iki_scale);
        assert_eq!(p.typo_rate, 1.0);
        assert_eq!(
            p.pause_variance_scale,
            Profile::SALES_ENGINEER.pause_variance_scale
        );
    }
}
