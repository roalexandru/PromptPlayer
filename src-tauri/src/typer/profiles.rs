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
/// Sales Engineer is the baseline (1.0/1.0/1.0). Fast and CEO derive from there:
/// - Fast: target IKI median 100 ms → scale 100/140 ≈ 0.714, low variance.
/// - CEO:  target IKI median 220 ms → scale 220/140 ≈ 1.571, high variance.
#[derive(Debug, Clone, Copy)]
pub struct Profile {
    pub kind: ProfileKind,
    pub iki_scale: f64,
    pub typo_rate: f64,
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
    /// Sales Engineer — actual measured throughput ~65 WPM including all pauses.
    /// Effective IKI median ~77 ms (raw mixture median 140 ms × 0.55 scale).
    pub const SALES_ENGINEER: Profile = Profile {
        kind: ProfileKind::SalesEngineer,
        iki_scale: 0.55,
        typo_rate: 1.0 / 90.0,
        pause_variance_scale: 1.0,
        burst_enabled: true,
        typos_enabled: true,
        pre_submit_pause_enabled: true,
        send_final_enter: false,
    };

    /// Fast Presenter — actual measured throughput ~110 WPM. Effective IKI median ~45 ms.
    pub const FAST_PRESENTER: Profile = Profile {
        kind: ProfileKind::FastPresenter,
        iki_scale: 0.32,
        typo_rate: 1.0 / 150.0,
        pause_variance_scale: 0.5,
        burst_enabled: true,
        typos_enabled: true,
        pre_submit_pause_enabled: true,
        send_final_enter: false,
    };

    /// Thoughtful CEO — actual measured throughput ~50 WPM. Effective IKI median ~109 ms.
    pub const THOUGHTFUL_CEO: Profile = Profile {
        kind: ProfileKind::ThoughtfulCeo,
        iki_scale: 0.78,
        typo_rate: 1.0 / 120.0,
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
