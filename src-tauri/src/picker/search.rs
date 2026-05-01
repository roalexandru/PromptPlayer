//! §5.2 — fuzzy search index for the picker.
//! Uses `nucleo-matcher` (Helix's matcher) which produces highlight spans.

use crate::prompts::Prompt;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub prompt_id: String,
    pub score: u32,
    /// Char offsets (in the haystack) that matched the query. Frontend
    /// renders these as highlighted.
    pub highlights: Vec<u32>,
}

pub struct SearchIndex {
    matcher: Matcher,
    /// Stable list of prompts in current order; rebuilt on hot-reload.
    haystacks: Vec<(String, String)>, // (id, haystack)
}

impl SearchIndex {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            haystacks: Vec::new(),
        }
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
            body: body.into(),
            source_path: None,
        }
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
}
