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

/// Per-profile parameters. `iki_scale` multiplies sampled IKI to retarget median;
/// `pause_variance_scale` multiplies the σ of all pause distributions.
///
/// `iki_min_ms` is the profile-aware floor applied AFTER scaling; without it,
/// the global 60 ms floor in `distributions::IKI_MIN_MS` would silently swallow
/// any `iki_scale < ~0.43` (since base median is ~140 ms). Fast Presenter and
/// the picker's "Fast" override drop the floor so their scale actually takes
/// effect; Sales Engineer / CEO keep the realistic 60 ms human-typing floor.
///
/// `pause_scale` multiplies the *mean* of all boundary pauses (word, sentence,
/// paragraph). The old design only scaled σ via `pause_variance_scale`, so even
/// at low `iki_scale` the chars flew past but every space stalled the same
/// ~180 ms — visibly choppy. Cutting the mean too is what makes "fast" feel
/// continuous instead of stuttery.
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
}

impl Default for Profile {
    fn default() -> Self {
        Self::SALES_ENGINEER
    }
}

impl Profile {
    /// Sales Engineer — calm, plausible-human cadence. Target ~75 WPM with
    /// realistic word/sentence pauses. The 60 ms floor here is the realistic
    /// lower bound for sustained human typing (faster than that = "presenter").
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
    };

    /// Fast Presenter — confident demo cadence. Target ~180 WPM. The lower
    /// `iki_min_ms` is what unblocks the scale: previously `iki_scale=0.32`
    /// was silently clamped to the global 60 ms floor on the bulk of chars.
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
        if overrides.iki_median_ms.is_some()
            || overrides.typo_rate.is_some()
            || overrides.pause_variance_scale.is_some()
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
}
