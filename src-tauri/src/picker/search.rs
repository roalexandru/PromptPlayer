//! §5.2 — fuzzy search index for the picker.
//! Uses `nucleo-matcher` (Helix's matcher) which produces highlight spans.

use crate::prompts::Prompt;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct SearchHit {
    pub prompt_id: String,
    pub score: u32,
    /// Char offsets (in the haystack) that matched the query. Frontend
    /// renders these as highlighted.
    pub highlights: Vec<u32>,
}

/// Reorder `hits` so the ids in `recents` come first, in `recents` order.
///
/// §5.2 puts a recently-used tier at the top of the picker list. Without it an
/// empty query returns prompts in filesystem order, which is never the order
/// you want mid-demo — the prompt you reach for is nearly always one you just
/// used. Ordering is stable for everything not in `recents`, so the underlying
/// list order still shows through below the recents.
pub fn promote_recents(hits: &mut Vec<SearchHit>, recents: &[String]) {
    if recents.is_empty() || hits.is_empty() {
        return;
    }
    let rank = |id: &str| recents.iter().position(|r| r == id);
    // `sort_by_key` is stable, so non-recents keep their relative order.
    hits.sort_by_key(|h| rank(&h.prompt_id).unwrap_or(usize::MAX));
}

pub struct SearchIndex {
    matcher: Matcher,
    /// Stable list of prompts in current order; rebuilt on hot-reload.
    haystacks: Vec<(String, String)>, // (id, haystack)
    /// PromptStore generation we last built against. Used by
    /// `rebuild_if_stale` to skip redundant rebuilds.
    last_built_generation: u64,
}

impl SearchIndex {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            haystacks: Vec::new(),
            last_built_generation: 0,
        }
    }

    /// Rebuild only when the generation changed. Generation 0 always rebuilds,
    /// so the first build needs no extra bookkeeping.
    pub fn rebuild_if_stale(&mut self, current_generation: u64, prompts: &[Prompt]) {
        if current_generation != 0 && current_generation == self.last_built_generation {
            return;
        }
        self.rebuild(prompts);
        self.last_built_generation = current_generation;
    }

    pub fn rebuild(&mut self, prompts: &[Prompt]) {
        self.haystacks.clear();
        for p in prompts {
            // Searchable surface: name + description + triggers + tags + body excerpt + scoped app(s).
            let mut h = String::new();
            h.push_str(&p.name);
            h.push(' ');
            h.push_str(&p.description);
            h.push(' ');
            for t in &p.triggers {
                h.push_str(t);
                h.push(' ');
            }
            for tag in &p.tags {
                h.push_str(tag);
                h.push(' ');
            }
            // Truncated body to keep haystacks small.
            let body_excerpt: String = p.body.chars().take(120).collect();
            h.push_str(&body_excerpt);
            self.haystacks.push((p.id.clone(), h));
        }
    }

    pub fn query(&mut self, q: &str, limit: usize) -> Vec<SearchHit> {
        if q.is_empty() {
            // Return all prompts in order, no highlights.
            return self
                .haystacks
                .iter()
                .take(limit)
                .map(|(id, _)| SearchHit {
                    prompt_id: id.clone(),
                    score: 0,
                    highlights: Vec::new(),
                })
                .collect();
        }
        let pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
        let mut hits: Vec<SearchHit> = self
            .haystacks
            .iter()
            .filter_map(|(id, h)| {
                let mut indices = Vec::new();
                let chars: Vec<char> = h.chars().collect();
                let utf32 = nucleo_matcher::Utf32Str::Ascii(h.as_bytes());
                let _ = chars;
                let _ = utf32;
                // Use the convenience pattern.match to score and capture indices.
                let mut buf = Vec::new();
                let utf32 = nucleo_matcher::Utf32Str::new(h, &mut buf);
                let score = pattern.indices(utf32, &mut self.matcher, &mut indices);
                score.map(|s| SearchHit {
                    prompt_id: id.clone(),
                    score: s,
                    highlights: indices,
                })
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score));
        hits.truncate(limit);
        hits
    }
}

impl Default for SearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, name: &str, body: &str) -> Prompt {
        Prompt {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            triggers: vec![id.into()],
            commit_char: '>',
            priority: 0,
            typing_profile: Default::default(),
            typing_overrides: Default::default(),
            scope: None,
            filters: Vec::new(),
            hotkey: None,
            tags: Vec::new(),
            enabled: true,
            pinned: false,
            newline_mode: None,
            origin: Default::default(),
            body: body.into(),
            source_path: None,
        }
    }

    fn hit(id: &str) -> SearchHit {
        SearchHit {
            prompt_id: id.into(),
            score: 0,
            highlights: Vec::new(),
        }
    }

    #[test]
    fn promote_recents_puts_recents_first_in_their_own_order() {
        let mut hits = vec![hit("a"), hit("b"), hit("c"), hit("d")];
        promote_recents(&mut hits, &["c".into(), "a".into()]);
        let ids: Vec<&str> = hits.iter().map(|h| h.prompt_id.as_str()).collect();
        assert_eq!(ids, vec!["c", "a", "b", "d"]);
    }

    #[test]
    fn promote_recents_is_stable_for_non_recents() {
        let mut hits = vec![hit("a"), hit("b"), hit("c")];
        promote_recents(&mut hits, &["c".into()]);
        let ids: Vec<&str> = hits.iter().map(|h| h.prompt_id.as_str()).collect();
        assert_eq!(ids, vec!["c", "a", "b"], "a before b, as they came in");
    }

    #[test]
    fn promote_recents_ignores_unknown_ids() {
        let mut hits = vec![hit("a"), hit("b")];
        promote_recents(&mut hits, &["ghost".into(), "b".into()]);
        let ids: Vec<&str> = hits.iter().map(|h| h.prompt_id.as_str()).collect();
        assert_eq!(ids, vec!["b", "a"]);
    }

    #[test]
    fn promote_recents_handles_empty_inputs() {
        let mut hits = vec![hit("a")];
        promote_recents(&mut hits, &[]);
        assert_eq!(hits.len(), 1);
        let mut empty: Vec<SearchHit> = Vec::new();
        promote_recents(&mut empty, &["a".into()]);
        assert!(empty.is_empty());
    }

    #[test]
    fn empty_query_returns_all() {
        let mut idx = SearchIndex::new();
        idx.rebuild(&[p("a", "Alpha", ""), p("b", "Bravo", "")]);
        let hits = idx.query("", 10);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn fuzzy_query_ranks_by_score() {
        let mut idx = SearchIndex::new();
        idx.rebuild(&[
            p("alpha", "Alpha thing", ""),
            p("bravo", "Bravo refactoring", ""),
            p("charlie", "Charlie introduction", ""),
        ]);
        let hits = idx.query("refac", 5);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].prompt_id, "bravo");
    }

    #[test]
    fn rebuild_if_stale_is_idempotent_for_same_generation() {
        let mut idx = SearchIndex::new();
        let prompts = vec![p("alpha", "Alpha thing", "")];
        idx.rebuild_if_stale(7, &prompts);
        // Mutate the slice and call with same generation — must NOT re-pick
        // up the new state, because we promised the index it's still gen 7.
        let mutated = vec![p("zeta", "Zeta thing", "")];
        idx.rebuild_if_stale(7, &mutated);
        let hits = idx.query("zeta", 5);
        assert!(
            hits.is_empty(),
            "stale rebuild must not pick up new prompts"
        );
    }

    #[test]
    fn rebuild_if_stale_picks_up_new_generation() {
        let mut idx = SearchIndex::new();
        idx.rebuild_if_stale(1, &[p("alpha", "Alpha", "")]);
        idx.rebuild_if_stale(2, &[p("zeta", "Zeta", "")]);
        let hits = idx.query("zeta", 5);
        assert_eq!(hits.first().map(|h| h.prompt_id.as_str()), Some("zeta"));
    }

    #[test]
    fn rebuild_if_stale_zero_always_rebuilds() {
        // Generation 0 = "always rebuild" sentinel for safety.
        let mut idx = SearchIndex::new();
        idx.rebuild_if_stale(0, &[p("alpha", "Alpha", "")]);
        idx.rebuild_if_stale(0, &[p("zeta", "Zeta", "")]);
        let hits = idx.query("zeta", 5);
        assert_eq!(hits.first().map(|h| h.prompt_id.as_str()), Some("zeta"));
    }
}
