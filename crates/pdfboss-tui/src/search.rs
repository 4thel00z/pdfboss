//! Incremental object search: case-insensitive matching over object
//! numbers, dict keys, name values and string contents. Results stream in
//! from a background task tagged with a generation; stale generations are
//! dropped here.

use pdfboss_core::{ObjRef, Object};

/// One search result.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SearchHit {
    pub r: ObjRef,
}

/// Search bar + result-set model.
pub struct SearchState {
    /// Whether the status-bar input is open (keystrokes edit the query).
    pub active: bool,
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub cursor: Option<usize>,
    pub generation: u64,
    /// Whether a background task is still streaming results.
    pub running: bool,
}

impl SearchState {
    pub fn new() -> SearchState {
        SearchState {
            active: false,
            query: String::new(),
            hits: Vec::new(),
            cursor: None,
            generation: 0,
            running: false,
        }
    }

    /// Opens the input (`/`).
    pub fn open(&mut self) {
        self.active = true;
        self.query.clear();
        self.hits.clear();
        self.cursor = None;
        self.running = false;
    }

    /// Closes the input and discards everything (Esc). Bumps `generation`
    /// (mirroring `restart()`) so hits from a still-running search task
    /// tagged with the pre-cancel generation are rejected by `add_hit`
    /// instead of silently repopulating the just-cleared hit list.
    pub fn cancel(&mut self) {
        self.active = false;
        self.generation += 1;
        self.query.clear();
        self.hits.clear();
        self.cursor = None;
        self.running = false;
    }

    /// Closes the input keeping hits for n/N navigation (Enter).
    pub fn accept(&mut self) {
        self.active = false;
    }

    /// Appends a character; returns the new generation to search under.
    pub fn push_char(&mut self, c: char) -> u64 {
        self.query.push(c);
        self.restart()
    }

    /// Removes the last character; `None` when the query was empty.
    pub fn pop_char(&mut self) -> Option<u64> {
        self.query.pop()?;
        Some(self.restart())
    }

    fn restart(&mut self) -> u64 {
        self.generation += 1;
        self.hits.clear();
        self.cursor = None;
        self.running = !self.query.is_empty();
        self.generation
    }

    /// Adds a hit if it belongs to the current generation.
    pub fn add_hit(&mut self, generation: u64, hit: SearchHit) -> bool {
        if generation != self.generation {
            return false;
        }
        self.hits.push(hit);
        true
    }

    /// Marks the current generation's task finished.
    pub fn finish(&mut self, generation: u64) {
        if generation == self.generation {
            self.running = false;
        }
    }

    /// Advances to the next hit, wrapping.
    pub fn next_hit(&mut self) -> Option<SearchHit> {
        if self.hits.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => 0,
            Some(index) => (index + 1) % self.hits.len(),
        };
        self.cursor = Some(next);
        Some(self.hits[next])
    }

    /// Steps back to the previous hit, wrapping.
    pub fn prev_hit(&mut self) -> Option<SearchHit> {
        if self.hits.is_empty() {
            return None;
        }
        let prev = match self.cursor {
            None => self.hits.len() - 1,
            Some(0) => self.hits.len() - 1,
            Some(index) => index - 1,
        };
        self.cursor = Some(prev);
        Some(self.hits[prev])
    }

    /// Status-bar text while the input is open.
    pub fn status_line(&self) -> String {
        let running = if self.running { " \u{2026}" } else { "" };
        format!("/{} \u{b7} {} hits{}", self.query, self.hits.len(), running)
    }
}

impl Default for SearchState {
    fn default() -> SearchState {
        SearchState::new()
    }
}

/// Case-insensitive match of `query` against an object's number, dict keys,
/// name values and string contents (recursing through arrays, dicts and
/// stream dicts).
pub fn object_matches(query: &str, num: u32, object: &Object) -> bool {
    let needle = query.to_ascii_lowercase();
    if needle.is_empty() {
        return false;
    }
    if num.to_string().contains(&needle) {
        return true;
    }
    value_matches(&needle, object)
}

fn value_matches(needle: &str, object: &Object) -> bool {
    match object {
        Object::Name(name) => name.0.to_ascii_lowercase().contains(needle),
        Object::String(bytes) => String::from_utf8_lossy(bytes)
            .to_ascii_lowercase()
            .contains(needle),
        Object::Array(items) => items.iter().any(|item| value_matches(needle, item)),
        Object::Dict(dict) => dict.iter().any(|(key, value)| {
            key.0.to_ascii_lowercase().contains(needle) || value_matches(needle, value)
        }),
        Object::Stream(stream) => stream.dict.iter().any(|(key, value)| {
            key.0.to_ascii_lowercase().contains(needle) || value_matches(needle, value)
        }),
        Object::Null | Object::Bool(..) | Object::Int(..) | Object::Real(..) | Object::Ref(..) => {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{Dict, Name, ObjRef, Object};

    fn page_dict() -> Object {
        let mut dict = Dict::new();
        dict.insert(
            Name("Type".to_string()),
            Object::Name(Name("Page".to_string())),
        );
        dict.insert(
            Name("Contents".to_string()),
            Object::Ref(ObjRef { num: 13, gen: 0 }),
        );
        dict.insert(
            Name("Note".to_string()),
            Object::String(b"Hello World".to_vec()),
        );
        Object::Dict(dict)
    }

    #[test]
    fn matches_object_number_keys_names_and_strings() {
        let object = page_dict();
        assert!(object_matches("12", 12, &object), "object number");
        assert!(object_matches("contents", 12, &object), "dict key");
        assert!(object_matches("page", 12, &object), "name value");
        assert!(object_matches("hello w", 12, &object), "string content");
        assert!(object_matches("PAGE", 12, &object), "case-insensitive");
        assert!(!object_matches("zebra", 12, &object));
    }

    #[test]
    fn matches_nested_arrays_and_stream_dicts() {
        let inner = page_dict();
        let object = Object::Array(vec![Object::Int(7), inner]);
        assert!(object_matches("hello", 3, &object));
        let mut stream_dict = Dict::new();
        stream_dict.insert(
            Name("Filter".to_string()),
            Object::Name(Name("FlateDecode".to_string())),
        );
        let stream = Object::Stream(pdfboss_core::Stream {
            dict: stream_dict,
            data: Vec::new(),
        });
        assert!(object_matches("flate", 3, &stream));
    }

    #[test]
    fn generation_bumps_invalidate_stale_hits() {
        let mut search = SearchState::new();
        search.open();
        let first = search.push_char('a');
        assert!(search.add_hit(
            first,
            SearchHit {
                r: ObjRef { num: 1, gen: 0 }
            }
        ));
        let second = search.push_char('b');
        assert_ne!(first, second);
        assert!(!search.add_hit(
            first,
            SearchHit {
                r: ObjRef { num: 2, gen: 0 }
            }
        ));
        assert_eq!(search.hits.len(), 0, "new keystroke cleared old hits");
        assert!(search.add_hit(
            second,
            SearchHit {
                r: ObjRef { num: 3, gen: 0 }
            }
        ));
        assert!(search.running);
        search.finish(second);
        assert!(!search.running);
    }

    #[test]
    fn next_and_prev_wrap_over_hits() {
        let mut search = SearchState::new();
        search.open();
        let generation = search.push_char('x');
        for num in [4u32, 8, 15] {
            search.add_hit(
                generation,
                SearchHit {
                    r: ObjRef { num, gen: 0 },
                },
            );
        }
        assert_eq!(search.next_hit().map(|hit| hit.r.num), Some(4));
        assert_eq!(search.next_hit().map(|hit| hit.r.num), Some(8));
        assert_eq!(search.next_hit().map(|hit| hit.r.num), Some(15));
        assert_eq!(search.next_hit().map(|hit| hit.r.num), Some(4), "wraps");
        assert_eq!(
            search.prev_hit().map(|hit| hit.r.num),
            Some(15),
            "wraps back"
        );
    }

    #[test]
    fn cancel_invalidates_in_flight_hits() {
        let mut search = SearchState::new();
        search.open();
        let generation = search.push_char('a');
        search.cancel();
        assert!(
            !search.add_hit(
                generation,
                SearchHit {
                    r: ObjRef { num: 1, gen: 0 }
                }
            ),
            "cancel must invalidate the pre-cancel generation"
        );
        assert!(search.hits.is_empty());
    }

    #[test]
    fn pop_char_and_cancel() {
        let mut search = SearchState::new();
        search.open();
        assert!(search.active);
        search.push_char('a');
        search.push_char('b');
        assert_eq!(search.pop_char(), Some(3), "third generation");
        assert_eq!(search.query, "a");
        search.pop_char();
        assert_eq!(search.pop_char(), None, "empty query pops nothing");
        search.cancel();
        assert!(!search.active);
        assert!(search.query.is_empty());
        assert!(search.hits.is_empty());
    }

    #[test]
    fn status_line_reports_query_and_hits() {
        let mut search = SearchState::new();
        search.open();
        let generation = search.push_char('p');
        search.add_hit(
            generation,
            SearchHit {
                r: ObjRef { num: 3, gen: 0 },
            },
        );
        assert_eq!(search.status_line(), "/p \u{b7} 1 hits \u{2026}");
        search.finish(generation);
        assert_eq!(search.status_line(), "/p \u{b7} 1 hits");
    }
}
