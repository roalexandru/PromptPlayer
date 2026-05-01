//! §2 — trigger matcher.
//!
//! - Trigger word(s) = contiguous run(s) of non-whitespace chars
//!   immediately preceding the commit char.
//! - Match is **case-insensitive** with **case propagation** (§2.2): typed prefix
//!   is preserved verbatim; the rest of the prompt is rendered with the user's case.
//! - **Multi-word triggers** supported with greedy longest-match. Multi-word match
//!   resets if user pauses >2s between words.
//! - **Multiple aliases per prompt** (§2.2).
//! - **Uniqueness** enforced at edit time.
//! - Failed match: commit char passes through.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Maximum trigger word count we'll search backward for.
pub const MAX_TRIGGER_WORDS: usize = 6;

/// Per §2.2: multi-word match resets if user pauses >2s between words.
pub const MULTI_WORD_RESET: Duration = Duration::from_secs(2);

/// Maximum age of the FIRST char of a candidate trigger word. Anything older
/// is considered stale context (e.g. residue from typing in another app
/// before switching to the target). Prevents `<old context>hi>` from being
/// treated as a single 8-char word.
pub const TRIGGER_FRESHNESS: Duration = Duration::from_secs(2);

/// Capacity of the keystroke ring buffer (~64 chars per §8.4 spec note).
pub const RING_CAPACITY: usize = 256;

/// One entry in the keystroke ring.
#[derive(Debug, Clone, Copy)]
pub struct KeyEntry {
    pub ch: char,
    pub at: Instant,
}

#[derive(Debug, Default)]
pub struct RingBuffer {
    entries: std::collections::VecDeque<KeyEntry>,
}

impl RingBuffer {
    pub fn push(&mut self, ch: char, at: Instant) {
        if self.entries.len() == RING_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(KeyEntry { ch, at });
    }

    /// Drop the last N entries (e.g. when we suppress a commit char).
    pub fn pop_last(&mut self, n: usize) {
        for _ in 0..n {
            self.entries.pop_back();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &KeyEntry> {
        self.entries.iter()
    }
}

/// A registered trigger (canonical lowercase) and which prompt it points to.
#[derive(Debug, Clone)]
pub struct TriggerEntry {
    pub canonical: String,
    pub prompt_id: PromptId,
    pub word_count: usize,
    pub commit_char: char,
}

pub type PromptId = String;

/// Index of triggers grouped by canonical lowercase form for O(1) lookup.
#[derive(Debug, Default)]
pub struct MatchIndex {
    by_canonical: HashMap<String, Vec<TriggerEntry>>,
}

impl MatchIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a trigger. Multiple entries per (canonical, commit_char) are allowed
    /// — they're disambiguated at fire time by scope (§4). Same `prompt_id`
    /// inserted twice errors.
    pub fn insert(&mut self, entry: TriggerEntry) -> Result<(), DuplicateError> {
        let bucket = self
            .by_canonical
            .entry(entry.canonical.clone())
            .or_default();
        if bucket
            .iter()
            .any(|e| e.commit_char == entry.commit_char && e.prompt_id == entry.prompt_id)
        {
            return Err(DuplicateError {
                canonical: entry.canonical.clone(),
                commit_char: entry.commit_char,
            });
        }
        bucket.push(entry);
        Ok(())
    }

    pub fn clear(&mut self) {
        self.by_canonical.clear();
    }

    pub fn len(&self) -> usize {
        self.by_canonical.values().map(|v| v.len()).sum()
    }

    fn lookup_all(&self, canonical: &str, commit_char: char) -> Vec<&TriggerEntry> {
        self.by_canonical
            .get(canonical)
            .map(|bucket| {
                bucket
                    .iter()
                    .filter(|e| e.commit_char == commit_char)
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("duplicate trigger '{canonical}' with commit char '{commit_char}'")]
pub struct DuplicateError {
    pub canonical: String,
    pub commit_char: char,
}

/// Successful match.
#[derive(Debug, Clone)]
pub struct Match {
    pub prompt_id: PromptId,
    /// Number of chars in the buffer that are part of the trigger
    /// (excluding the commit char itself; that's already separately consumed).
    pub trigger_chars: usize,
    /// Word count consumed.
    pub word_count: usize,
    /// The exact original-case form the user typed for the trigger.
    /// Used for case propagation (§2.2).
    pub typed_form: String,
}

/// Try to match the buffer ending right before `commit_char`.
///
/// `index`: the trigger index.
/// `buffer`: keystroke ring (the most recent N typed chars).
/// `commit_char`: the commit char that just landed.
/// `now`: current instant (for the §2.2 multi-word 2s reset rule).
///
/// Returns the FIRST candidate of the longest matching trigger (greedy multi-word).
/// Use `match_buffer_all` if you need every prompt that shares this trigger.
pub fn match_buffer(
    index: &MatchIndex,
    buffer: &RingBuffer,
    commit_char: char,
    now: Instant,
) -> Option<Match> {
    match_buffer_all(index, buffer, commit_char, now)
        .into_iter()
        .next()
}

/// Returns ALL candidate matches (longest-first, then by match order). Used by
/// the scope resolver to pick the best per foreground app.
pub fn match_buffer_all(
    index: &MatchIndex,
    buffer: &RingBuffer,
    commit_char: char,
    now: Instant,
) -> Vec<Match> {
    if buffer.is_empty() {
        return Vec::new();
    }

    // Walk backward from the end, collecting words separated by whitespace.
    // Stop if we hit MAX_TRIGGER_WORDS or a stale word boundary (>2s pause).
    let entries: Vec<&KeyEntry> = buffer.iter().collect();

    // Trim trailing whitespace (shouldn't normally exist; commit char came after content).
    let mut end = entries.len();
    while end > 0 && entries[end - 1].ch.is_whitespace() {
        end -= 1;
    }
    if end == 0 {
        return Vec::new();
    }

    // Identify word ranges from the back: each word is contiguous non-whitespace
    // typed within the freshness window. A char older than `TRIGGER_FRESHNESS`
    // acts as an implicit boundary so stale context (typing in another app,
    // long pauses) doesn't get glued onto the trigger word.
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut i = end;
    while i > 0 && words.len() < MAX_TRIGGER_WORDS {
        let mut j = i;
        // Skip whitespace going back.
        while j > 0 && entries[j - 1].ch.is_whitespace() {
            j -= 1;
        }
        if j == 0 {
            break;
        }
        // Bail if even the last non-whitespace char is itself stale; nothing
        // older could be fresher.
        if now.duration_since(entries[j - 1].at) > TRIGGER_FRESHNESS {
            break;
        }
        let word_end = j;
        while j > 0 && !entries[j - 1].ch.is_whitespace() {
            // Treat stale chars as a word boundary too.
            if now.duration_since(entries[j - 1].at) > TRIGGER_FRESHNESS {
                break;
            }
            j -= 1;
        }
        words.push((j, word_end));
        i = j;
    }
    // words[0] is the most recent word.

    // Try longest match first: starting from the longest possible word count, downward.
    for k in (1..=words.len()).rev() {
        // Check the §2.2 freshness rule: between word boundaries we need <=2s gaps.
        // Specifically, the time between the LAST char of word[i] and the FIRST char of word[i-1]
        // (i.e., across the whitespace gap) must be ≤2s.
        let mut stale = false;
        for w in 0..k - 1 {
            let prev_word_first_char = entries[words[w].0]; // word[w] is later in time; first char in buffer order
            let next_word_last_char = entries[words[w + 1].1 - 1]; // word[w+1] is earlier word; its last char preceded the gap
            let gap = prev_word_first_char
                .at
                .duration_since(next_word_last_char.at);
            if gap > MULTI_WORD_RESET {
                stale = true;
                break;
            }
        }
        if stale {
            continue;
        }

        // Build the trigger string oldest-to-newest, joining with single space.
        let parts: Vec<String> = (0..k)
            .rev()
            .map(|wi| {
                let (s, e) = words[wi];
                entries[s..e].iter().map(|ke| ke.ch).collect::<String>()
            })
            .collect();
        let typed_form = parts.join(" ");
        let canonical = typed_form.to_lowercase();

        let candidates = index.lookup_all(&canonical, commit_char);
        if !candidates.is_empty() {
            let first_idx = words[k - 1].0;
            let last_idx = words[0].1;
            let trigger_chars = last_idx - first_idx;
            let last_char_age = now.duration_since(entries[last_idx - 1].at);
            if last_char_age > Duration::from_secs(10) {
                continue;
            }
            // Freshness — the FIRST char of the trigger word(s) must have been
            // typed within TRIGGER_FRESHNESS of `now`. This prevents old context
            // from being concatenated into the trigger.
            let first_char_age = now.duration_since(entries[first_idx].at);
            if first_char_age > TRIGGER_FRESHNESS {
                continue;
            }
            return candidates
                .into_iter()
                .map(|entry| Match {
                    prompt_id: entry.prompt_id.clone(),
                    trigger_chars,
                    word_count: k,
                    typed_form: typed_form.clone(),
                })
                .collect();
        }
    }
    Vec::new()
}

/// Apply case propagation per §2.2: render `body` with the same case style
/// the user typed for `typed_form`.
///
/// Strategy:
/// - If typed_form is ALL CAPS → uppercase the body.
/// - If typed_form is Title Case (first char upper, rest lower) → capitalize first body char.
/// - Else (all lower or mixed) → body untouched.
pub fn propagate_case(typed_form: &str, body: &str) -> String {
    let alpha: Vec<char> = typed_form.chars().filter(|c| c.is_alphabetic()).collect();
    if alpha.is_empty() {
        return body.to_string();
    }
    let all_upper = alpha.iter().all(|c| c.is_uppercase());
    let title = alpha[0].is_uppercase() && alpha.iter().skip(1).all(|c| c.is_lowercase());
    if all_upper {
        body.to_uppercase()
    } else if title {
        let mut out = String::with_capacity(body.len());
        let mut chars = body.chars();
        if let Some(first) = chars.next() {
            for c in first.to_uppercase() {
                out.push(c);
            }
        }
        out.extend(chars);
        out
    } else {
        body.to_string()
    }
}

/// Wrapper holding the matcher state for the running app (Tauri-friendly).
#[derive(Default)]
pub struct MatcherState {
    pub index: RwLock<MatchIndex>,
    pub buffer: RwLock<RingBuffer>,
}

impl MatcherState {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn rebuild_index(&self, entries: Vec<TriggerEntry>) -> Result<(), DuplicateError> {
        let mut idx = self.index.write();
        idx.clear();
        for e in entries {
            idx.insert(e)?;
        }
        Ok(())
    }

    pub fn observe_char(&self, ch: char, at: Instant) {
        self.buffer.write().push(ch, at);
    }

    /// User pressed Backspace — drop the last char from the buffer (mimics what
    /// the focused app sees so subsequent matches reflect actual screen state).
    pub fn observe_backspace(&self, _at: Instant) {
        self.buffer.write().pop_last(1);
    }

    pub fn pop_last_chars(&self, n: usize) {
        self.buffer.write().pop_last(n);
    }

    pub fn last_char(&self) -> Option<char> {
        self.buffer.read().iter().last().map(|e| e.ch)
    }

    /// Try to match given the just-typed commit char.
    pub fn try_match(&self, commit_char: char, now: Instant) -> Option<Match> {
        let idx = self.index.read();
        let buf = self.buffer.read();
        match_buffer(&idx, &buf, commit_char, now)
    }

    /// All candidates sharing the matched trigger (for §4 scope resolution).
    pub fn try_match_all(&self, commit_char: char, now: Instant) -> Vec<Match> {
        let idx = self.index.read();
        let buf = self.buffer.read();
        match_buffer_all(&idx, &buf, commit_char, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(prompt: &str, trigger: &str, words: usize) -> TriggerEntry {
        TriggerEntry {
            canonical: trigger.to_lowercase(),
            prompt_id: prompt.to_string(),
            word_count: words,
            commit_char: '>',
        }
    }

    fn build(now: Instant, text: &str) -> RingBuffer {
        let mut b = RingBuffer::default();
        for (i, c) in text.chars().enumerate() {
            b.push(c, now + Duration::from_millis(i as u64 * 100));
        }
        b
    }

    #[test]
    fn same_prompt_id_rejected() {
        let mut idx = MatchIndex::new();
        idx.insert(t("p1", "build", 1)).unwrap();
        // Same prompt_id + same canonical + same commit_char → duplicate.
        let err = idx.insert(t("p1", "build", 1));
        assert!(err.is_err());
    }

    #[test]
    fn different_prompts_can_share_trigger_for_scopes() {
        // §4 — multiple scoped prompts coexist with the same trigger.
        let mut idx = MatchIndex::new();
        idx.insert(t("cursor-intro", "intro", 1)).unwrap();
        idx.insert(t("slack-intro", "intro", 1)).unwrap();
        let now = Instant::now();
        let buf = build(now, "intro");
        let all = match_buffer_all(&idx, &buf, '>', now + Duration::from_millis(600));
        assert_eq!(all.len(), 2, "both candidates should match");
    }

    #[test]
    fn single_word_match_case_insensitive() {
        let mut idx = MatchIndex::new();
        idx.insert(t("p1", "build", 1)).unwrap();
        let now = Instant::now();
        let buf = build(now, "Build");
        let m = match_buffer(&idx, &buf, '>', now + Duration::from_millis(600)).unwrap();
        assert_eq!(m.prompt_id, "p1");
        assert_eq!(m.trigger_chars, 5);
        assert_eq!(m.typed_form, "Build");
    }

    #[test]
    fn multi_word_greedy_longest_match() {
        let mut idx = MatchIndex::new();
        idx.insert(t("short", "build", 1)).unwrap();
        idx.insert(t("long", "build me", 2)).unwrap();
        let now = Instant::now();
        let buf = build(now, "Build me");
        let m = match_buffer(&idx, &buf, '>', now + Duration::from_millis(900)).unwrap();
        assert_eq!(m.prompt_id, "long");
        assert_eq!(m.word_count, 2);
    }

    #[test]
    fn multi_word_resets_when_stale_and_last_word_is_trigger() {
        let mut idx = MatchIndex::new();
        idx.insert(t("short", "build", 1)).unwrap();
        idx.insert(t("long", "build me", 2)).unwrap();
        let now = Instant::now();
        // User types "Build", waits 5s, types "Build>" — multi-word "Build Build" not registered;
        // stale gap rules out the multi-word combo anyway; single-word "Build" should fire.
        let mut buf = RingBuffer::default();
        for c in "Build".chars() {
            buf.push(c, now);
        }
        for c in " Build".chars() {
            buf.push(c, now + Duration::from_secs(5));
        }
        let m = match_buffer(&idx, &buf, '>', now + Duration::from_secs(6)).unwrap();
        assert_eq!(m.prompt_id, "short");
    }

    #[test]
    fn no_match_when_last_word_isnt_a_trigger_and_multi_is_stale() {
        let mut idx = MatchIndex::new();
        idx.insert(t("short", "build", 1)).unwrap();
        idx.insert(t("long", "build me", 2)).unwrap();
        let now = Instant::now();
        // User types "Build", waits 5s, types "me>" — multi-word stale; "me" alone isn't a trigger.
        let mut buf = RingBuffer::default();
        for c in "Build".chars() {
            buf.push(c, now);
        }
        for c in " me".chars() {
            buf.push(c, now + Duration::from_secs(5));
        }
        assert!(match_buffer(&idx, &buf, '>', now + Duration::from_secs(6)).is_none());
    }

    #[test]
    fn no_match_passes_through() {
        let idx = MatchIndex::new();
        let now = Instant::now();
        let buf = build(now, "unknown");
        assert!(match_buffer(&idx, &buf, '>', now + Duration::from_millis(700)).is_none());
    }

    #[test]
    fn case_propagation_rules() {
        assert_eq!(propagate_case("Build", "me a thing"), "Me a thing");
        assert_eq!(propagate_case("BUILD", "me a thing"), "ME A THING");
        assert_eq!(propagate_case("build", "me a thing"), "me a thing");
        assert_eq!(propagate_case("buIld", "me a thing"), "me a thing");
    }

    #[test]
    fn matcher_state_rebuild() {
        let s = MatcherState::shared();
        s.rebuild_index(vec![t("p1", "build", 1)]).unwrap();
        let now = Instant::now();
        s.observe_char('B', now);
        s.observe_char('u', now + Duration::from_millis(100));
        s.observe_char('i', now + Duration::from_millis(200));
        s.observe_char('l', now + Duration::from_millis(300));
        s.observe_char('d', now + Duration::from_millis(400));
        let m = s.try_match('>', now + Duration::from_millis(450)).unwrap();
        assert_eq!(m.prompt_id, "p1");
    }
}
