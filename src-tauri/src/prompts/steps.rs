//! Multi-step prompt sequences: type, wait, type the follow-up.
//!
//! An agent turn is rarely one message. "Review this diff" then, once it has
//! answered, "now add tests for what you changed" is the actual shape of the
//! work — and doing that by hand mid-demo means remembering the follow-up and
//! typing it under an audience's gaze.
//!
//! A step boundary is an HTML comment on its own line, so a prompt stays
//! readable (and renders unchanged) in any Markdown editor:
//!
//! ```markdown
//! Review the uncommitted changes on $GIT_BRANCH.
//!
//! <!-- pp:wait 25s -->
//!
//! Now add tests for what you changed.
//! ```
//!
//! ## What "wait" can and cannot mean
//! The app cannot see the agent's output — there is no screen reading here, and
//! §14 rules out `chrome.debugger`-style injection. So the wait is a **fixed
//! duration**, not "until the agent goes idle". That is the honest contract:
//! you size the pause to the work, the same way you would when driving the
//! demo by hand, and the kill-switch still cuts the whole sequence short.
//!
//! Because a follow-up only makes sense once the previous message was actually
//! sent, every step before the last is submitted with Enter — see
//! `Step::submit`.

use std::time::Duration;

/// Longest wait we will honour. A typo like `pp:wait 30m` should not be able
/// to park a demo for half an hour with no way to tell what happened.
pub const MAX_WAIT: Duration = Duration::from_secs(5 * 60);

/// One segment of a multi-step prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Text to type for this step.
    pub body: String,
    /// How long to wait *after* typing this step before starting the next one.
    /// `None` on the final step.
    pub wait_after: Option<Duration>,
}

impl Step {
    /// Whether this step ends with a submitting Enter.
    ///
    /// True for every step that has a follow-up: waiting for a response only
    /// means anything if the message was sent. The final step is left to the
    /// prompt's profile and the picker mode, so a sequence can still end in an
    /// unsent draft if that is what the author wanted.
    pub fn submit(&self) -> bool {
        self.wait_after.is_some()
    }
}

/// Parse `25s`, `1500ms`, `2m`, or a bare number of seconds.
pub fn parse_duration(raw: &str) -> Option<Duration> {
    let raw = raw.trim().to_ascii_lowercase();
    if raw.is_empty() {
        return None;
    }
    let (digits, unit) = match raw.find(|c: char| !c.is_ascii_digit() && c != '.') {
        Some(i) => (&raw[..i], raw[i..].trim()),
        None => (raw.as_str(), ""),
    };
    let value: f64 = digits.parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let secs = match unit {
        "ms" => value / 1000.0,
        "" | "s" | "sec" | "secs" => value,
        "m" | "min" | "mins" => value * 60.0,
        _ => return None,
    };
    Some(Duration::from_secs_f64(secs.min(MAX_WAIT.as_secs_f64())))
}

/// Recognise a step-boundary line and return its wait duration.
///
/// Accepts `<!-- pp:wait 25s -->` with any surrounding whitespace. A malformed
/// duration is *not* treated as a boundary, so the line stays visible in the
/// typed output where the author can see it went wrong — silently swallowing it
/// would produce one merged step and no explanation.
fn parse_boundary(line: &str) -> Option<Duration> {
    let t = line.trim();
    let inner = t.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let rest = inner
        .strip_prefix("pp:wait")
        .or_else(|| inner.strip_prefix("pp: wait"))?;
    parse_duration(rest)
}

/// True when `body` actually splits into more than one step.
///
/// Defined in terms of `split_steps` rather than "contains a boundary line",
/// because those two can disagree: a marker at the very start or end produces
/// an empty segment that `split_steps` drops. A consistency test caught the
/// mismatch, and one source of truth is the fix — this runs once per fire, not
/// once per keystroke.
pub fn is_multi_step(body: &str) -> bool {
    split_steps(body).len() > 1
}

/// Split a body into steps. A body with no boundaries yields exactly one step,
/// so callers can treat the single- and multi-step cases uniformly.
pub fn split_steps(body: &str) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    let mut current = String::new();
    for line in body.lines() {
        match parse_boundary(line) {
            Some(wait) => {
                steps.push(Step {
                    body: current.trim().to_string(),
                    wait_after: Some(wait),
                });
                current = String::new();
            }
            None => {
                current.push_str(line);
                current.push('\n');
            }
        }
    }
    steps.push(Step {
        body: current.trim().to_string(),
        wait_after: None,
    });
    // A boundary at the very start or end leaves an empty segment; drop those
    // rather than typing nothing and waiting.
    steps.retain(|s| !s.body.is_empty());
    if steps.is_empty() {
        steps.push(Step {
            body: String::new(),
            wait_after: None,
        });
    }
    // The last surviving step never waits, even if it inherited a boundary
    // from a trailing marker.
    if let Some(last) = steps.last_mut() {
        last.wait_after = None;
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_body_is_one_step() {
        let steps = split_steps("Just one message.\nWith two lines.");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].body, "Just one message.\nWith two lines.");
        assert!(steps[0].wait_after.is_none());
        assert!(!steps[0].submit(), "a single step is not force-submitted");
    }

    #[test]
    fn splits_on_a_wait_boundary() {
        let body = "Review the diff.\n\n<!-- pp:wait 25s -->\n\nNow add tests.";
        let steps = split_steps(body);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].body, "Review the diff.");
        assert_eq!(steps[0].wait_after, Some(Duration::from_secs(25)));
        assert_eq!(steps[1].body, "Now add tests.");
        assert!(steps[1].wait_after.is_none());
    }

    #[test]
    fn every_step_but_the_last_submits() {
        // A follow-up only makes sense if the previous message was sent.
        let steps = split_steps("one\n<!-- pp:wait 1s -->\ntwo\n<!-- pp:wait 1s -->\nthree");
        assert_eq!(steps.len(), 3);
        assert!(steps[0].submit());
        assert!(steps[1].submit());
        assert!(!steps[2].submit(), "the last step follows the picker mode");
    }

    #[test]
    fn multi_line_steps_keep_their_shape() {
        let body = "Line one\nLine two\n<!-- pp:wait 2s -->\nPara\n\nBreak";
        let steps = split_steps(body);
        assert_eq!(steps[0].body, "Line one\nLine two");
        assert_eq!(steps[1].body, "Para\n\nBreak");
    }

    #[test]
    fn leading_and_trailing_boundaries_do_not_produce_empty_steps() {
        let steps = split_steps("<!-- pp:wait 5s -->\nonly content\n<!-- pp:wait 5s -->");
        assert_eq!(steps.len(), 1, "{steps:?}");
        assert_eq!(steps[0].body, "only content");
        assert!(steps[0].wait_after.is_none(), "nothing follows, so no wait");
    }

    #[test]
    fn an_empty_body_still_yields_one_step() {
        let steps = split_steps("");
        assert_eq!(steps.len(), 1);
        assert!(steps[0].body.is_empty());
    }

    #[test]
    fn a_malformed_marker_is_left_in_the_text() {
        // Better to type a visibly-wrong line than to merge the steps and
        // leave the author wondering why the follow-up never came.
        let body = "one\n<!-- pp:wait forever -->\ntwo";
        assert!(!is_multi_step(body));
        let steps = split_steps(body);
        assert_eq!(steps.len(), 1);
        assert!(steps[0].body.contains("pp:wait forever"));
    }

    #[test]
    fn ordinary_html_comments_are_not_boundaries() {
        for body in [
            "text\n<!-- a normal comment -->\nmore",
            "text\n<!-- ppwait 5s -->\nmore",
            "text\n<!-- pp:sleep 5s -->\nmore",
        ] {
            assert!(!is_multi_step(body), "{body:?}");
        }
    }

    #[test]
    fn boundary_tolerates_whitespace_and_the_spaced_spelling() {
        for line in [
            "<!-- pp:wait 5s -->",
            "   <!--   pp:wait   5s   -->   ",
            "<!-- pp: wait 5s -->",
        ] {
            assert_eq!(
                parse_boundary(line),
                Some(Duration::from_secs(5)),
                "{line:?}"
            );
        }
    }

    #[test]
    fn parses_every_duration_unit() {
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("25s"), Some(Duration::from_secs(25)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("1.5s"), Some(Duration::from_millis(1500)));
    }

    #[test]
    fn rejects_nonsense_durations() {
        for bad in ["", "soon", "-5s", "5h", "5 hours", "s"] {
            assert!(parse_duration(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn caps_an_absurd_wait() {
        // A typo must not park the demo indefinitely.
        assert_eq!(parse_duration("120m"), Some(MAX_WAIT));
        assert_eq!(parse_duration("99999s"), Some(MAX_WAIT));
    }

    #[test]
    fn is_multi_step_matches_split_steps() {
        for body in [
            "plain",
            "a\n<!-- pp:wait 1s -->\nb",
            "<!-- pp:wait 1s -->\nonly",
        ] {
            assert_eq!(
                is_multi_step(body),
                split_steps(body).iter().any(|s| s.wait_after.is_some()),
                "{body:?}"
            );
        }
    }
}
