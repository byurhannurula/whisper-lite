//! Post-processing between the model and the text field.
//!
//! Raw Whisper output is close to usable but not quite: it carries leading/trailing whitespace,
//! occasionally emits bracketed non-speech annotations like `[BLANK_AUDIO]` or `(wind blowing)`,
//! and keeps the filler words people do not want typed out.
//!
//! All rule-based on purpose — no model, ~microseconds, and easy to reason about.

/// Filler words removed when they stand alone as a whole word.
///
/// Deliberately short. "like" and "so" are excluded because they are frequently meaningful
/// ("something like that", "so I said"), and wrongly deleting a real word is far more annoying
/// than leaving an "um" in.
const FILLERS: &[&str] = &["um", "uh", "erm", "uhh", "umm", "hmm", "mhm"];

/// Phrases Whisper emits when handed silence or noise rather than speech.
///
/// These come from its training data — subtitle tracks are full of them — and it will produce
/// one confidently when there is nothing to transcribe. `suppress_nst` stops the bracketed
/// annotations; these are plain sentences, so they need catching here.
const HALLUCINATIONS: &[&str] = &[
    "thank you",
    "thanks for watching",
    "thank you for watching",
    "please subscribe",
    "subtitles by the amara.org community",
    "you",
    "bye",
];

/// True if the whole output is one of Whisper's stock silence phrases.
///
/// Only ever matched against the *entire* transcript — "thank you" is a perfectly normal thing
/// to dictate, so discarding it mid-sentence would be worse than the problem.
fn is_hallucination(text: &str) -> bool {
    // Punctuation is dropped before comparing, so "Thank you." matches "thank you".
    let normalised: String = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    // Output that is nothing but punctuation is never something the user said.
    if normalised.is_empty() {
        return true;
    }

    HALLUCINATIONS.contains(&normalised.as_str())
}

pub fn clean(raw: &str, remove_fillers: bool, autocapitalize: bool) -> String {
    let without_annotations = strip_annotations(raw);

    let mut words: Vec<&str> = without_annotations.split_whitespace().collect();

    if remove_fillers {
        words.retain(|w| !is_filler(w));
    }

    let joined = words.join(" ");
    let result = if autocapitalize {
        capitalize_sentences(&joined)
    } else {
        joined
    };

    if is_hallucination(&result) {
        return String::new();
    }

    result
}

/// Capitalises the first letter, and the first letter after each sentence ending.
///
/// Whisper punctuates reliably but is inconsistent about case after a full stop, so a two
/// sentence dictation often arrives as "Ship it. then benchmark." Closing quotes and brackets
/// are allowed to sit between the terminator and the next word, so `He said "no." then left`
/// still capitalises "then".
fn capitalize_sentences(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // Start of the string counts as the start of a sentence.
    let mut pending = true;

    for c in s.chars() {
        if pending && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            pending = false;
            continue;
        }

        if matches!(c, '.' | '!' | '?') {
            pending = true;
        } else if !matches!(c, ' ' | '"' | '\'' | ')' | ']' | '”' | '’' | '…') {
            // Anything else — a letter, digit, comma — means we are mid-sentence again. Without
            // this, "3.5 million" would capitalise the word after the decimal point.
            pending = false;
        }

        out.push(c);
    }

    out
}

/// Removes `[...]` and `(...)` spans, which Whisper uses for non-speech events.
///
/// Only *closed* spans are removed. An earlier version dropped everything after an unmatched
/// opening bracket, so a single stray paren silently truncated the rest of the user's text.
fn strip_annotations(s: &str) -> String {
    let mut out = String::with_capacity(s.len());

    // One stack, holding the opening character alongside its byte offset.
    //
    // Two separate stacks were wrong, because both indexed the *same* buffer: in `(a [b) hello
    // world]` the `)` truncated `out` back to 0 while the `[`'s offset of 3 was still pending, so
    // the later `]` truncated to a position inside text that had already been rewritten. That
    // silently deleted real words, and panicked in `String::truncate` when the stale offset
    // landed mid-character in multibyte text.
    let mut open: Vec<(char, usize)> = Vec::new();

    for c in s.chars() {
        match c {
            '[' | '(' => {
                open.push((c, out.len()));
                out.push(c);
            }
            ']' | ')' => {
                let wanted = if c == ']' { '[' } else { '(' };
                out.push(c);
                // Only the innermost opener of the matching kind closes a span. A closer that
                // does not match is ordinary punctuation the user dictated — `(1, 2]` is an
                // interval, not an annotation — so it stays and the stack is left alone.
                if open.last().map(|&(kind, _)| kind) == Some(wanted) {
                    let (_, from) = open.pop().expect("just checked the stack is non-empty");
                    out.truncate(from);
                }
            }
            _ => out.push(c),
        }
    }

    out
}

fn is_filler(word: &str) -> bool {
    let bare: String = word
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect::<String>()
        .to_lowercase();
    FILLERS.contains(&bare.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_brackets_do_not_eat_the_transcript() {
        // Used to return "He": the `)` truncated the buffer while the `[`'s offset was still
        // pending, so the `]` cut into text that had already been rewritten.
        assert_eq!(clean("(a [b) hello world]", false, true), "(a");
        // Same shape in multibyte text, which used to panic inside String::truncate.
        assert_eq!(clean("([b)日本]", false, true), "");
    }

    #[test]
    fn a_mismatched_closer_is_just_punctuation() {
        // A half-open interval is a perfectly normal thing to dictate, and the `]` must not be
        // treated as the end of an annotation.
        assert_eq!(
            clean("the range (1, 2] is open", false, true),
            "The range (1, 2] is open"
        );
    }

    #[test]
    fn capitalises_each_sentence() {
        assert_eq!(
            clean("ship it. then benchmark it", false, true),
            "Ship it. Then benchmark it"
        );
        assert_eq!(clean("really? yes! fine", false, true), "Really? Yes! Fine");
    }

    #[test]
    fn a_decimal_point_does_not_start_a_sentence() {
        // The digit after the point is not a sentence boundary, so "million" stays lowercase.
        assert_eq!(
            clean("it cost 3.5 million dollars", false, true),
            "It cost 3.5 million dollars"
        );
    }

    #[test]
    fn capitalises_past_a_closing_quote() {
        assert_eq!(
            clean("he said \"no.\" then left", false, true),
            "He said \"no.\" Then left"
        );
    }

    #[test]
    fn autocapitalize_can_be_turned_off() {
        // Off means genuinely untouched — not even the first letter.
        assert_eq!(
            clean("ship it. then benchmark", false, false),
            "ship it. then benchmark"
        );
    }

    #[test]
    fn trims_and_collapses_whitespace() {
        assert_eq!(clean("  hello   there  ", false, true), "Hello there");
    }

    #[test]
    fn strips_whisper_annotations() {
        assert_eq!(clean("[BLANK_AUDIO] hello", false, true), "Hello");
        assert_eq!(
            clean("hello (wind blowing) there", false, true),
            "Hello there"
        );
    }

    #[test]
    fn removes_fillers_only_when_asked() {
        assert_eq!(clean("um hello uh there", true, true), "Hello there");
        assert_eq!(clean("um hello uh there", false, true), "Um hello uh there");
    }

    #[test]
    fn removes_fillers_with_trailing_punctuation() {
        assert_eq!(clean("well, um, fine", true, true), "Well, fine");
    }

    #[test]
    fn keeps_words_that_merely_contain_a_filler() {
        // "humming" contains "umm"; deleting real words is worse than leaving a filler in.
        assert_eq!(clean("humming along", true, true), "Humming along");
    }

    #[test]
    fn keeps_like_and_so() {
        assert_eq!(
            clean("something like that", true, true),
            "Something like that"
        );
        assert_eq!(clean("so I said no", true, true), "So I said no");
    }

    #[test]
    fn drops_whisper_silence_hallucinations() {
        assert_eq!(clean("Thank you.", false, true), "");
        assert_eq!(clean("Thanks for watching!", false, true), "");
        assert_eq!(clean("you", false, true), "");
        assert_eq!(clean("Bye!", false, true), "");
    }

    #[test]
    fn drops_punctuation_only_output() {
        assert_eq!(clean(".", false, true), "");
        assert_eq!(clean(" ... ", false, true), "");
    }

    #[test]
    fn keeps_thank_you_inside_a_real_sentence() {
        // Only a whole-output match counts — "thank you" is an ordinary thing to dictate.
        assert_eq!(
            clean("thank you for the update, I will look at it", false, true),
            "Thank you for the update, I will look at it"
        );
    }

    #[test]
    fn unmatched_bracket_does_not_truncate_the_transcript() {
        // A stray opening paren used to delete everything after it.
        assert_eq!(
            clean("send the report (see notes", false, true),
            "Send the report (see notes"
        );
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(clean("", true, true), "");
        assert_eq!(clean("   [BLANK_AUDIO]  ", true, true), "");
    }
}

/// Case-insensitive substring search returning byte offsets into `haystack`.
///
/// Deliberately does not lowercase the haystack and search that: lowercasing is not
/// byte-length-preserving (Turkish `İ` is 2 bytes and lowercases to 3), so offsets found in a
/// lowercased copy do not map back onto the original. Applying them anyway splices mid-word or
/// panics on a non-char-boundary — and Turkish is one of the offered languages.
fn find_ignoring_case(haystack: &str, needle: &str, from: usize) -> Option<(usize, usize)> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }

    let needle_lower: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();

    for (offset, _) in haystack[from..].char_indices() {
        let start = from + offset;

        let mut candidate = haystack[start..].chars().flat_map(char::to_lowercase);
        if !needle_lower
            .iter()
            .all(|&want| candidate.next() == Some(want))
        {
            continue;
        }

        // Walk the original characters again to find where the match ends in *bytes*, since one
        // source character can produce several lowercase ones.
        let mut produced = 0usize;
        let mut end = start;
        for (i, c) in haystack[start..].char_indices() {
            produced += c.to_lowercase().count();
            end = start + i + c.len_utf8();
            if produced >= needle_lower.len() {
                break;
            }
        }

        return Some((start, end));
    }

    None
}

fn is_boundary(text: &str, at: usize, before: bool) -> bool {
    let neighbour = if before {
        text[..at].chars().next_back()
    } else {
        text[at..].chars().next()
    };
    !neighbour.is_some_and(char::is_alphanumeric)
}

/// Replaces spoken trigger phrases with their expansions.
///
/// Case-insensitive and whole-phrase only: a trigger must sit on word boundaries, so "cal link"
/// does not fire inside "physical inks". Longer triggers are applied first, so a two-word trigger
/// wins over a one-word one that is a prefix of it.
pub fn expand_snippets(text: &str, snippets: &[(String, String)]) -> String {
    if snippets.is_empty() || text.is_empty() {
        return text.to_string();
    }

    let mut ordered: Vec<&(String, String)> = snippets
        .iter()
        .filter(|(trigger, _)| !trigger.trim().is_empty())
        .collect();
    ordered.sort_by_key(|(trigger, _)| std::cmp::Reverse(trigger.len()));

    let mut out = text.to_string();

    for (trigger, replacement) in ordered {
        let needle = trigger.trim();
        // Search resumes *past* each replacement rather than restarting. Restarting meant a
        // replacement containing its own trigger looped forever — trigger "cal" with replacement
        // "cal.com" grew the string without bound until the app ran out of memory, on the main
        // thread, taking the user's dictation with it.
        let mut cursor = 0usize;

        while let Some((at, end)) = find_ignoring_case(&out, needle, cursor) {
            if is_boundary(&out, at, true) && is_boundary(&out, end, false) {
                out.replace_range(at..end, replacement);
                cursor = at + replacement.len();
            } else {
                // Not on a word boundary — step past this occurrence and keep looking.
                cursor = end;
            }
        }
    }

    out
}

#[cfg(test)]
mod snippet_tests {
    use super::*;

    fn snips() -> Vec<(String, String)> {
        vec![
            ("sign off".into(), "Best, Byurhan".into()),
            ("cal link".into(), "cal.com/byurhan".into()),
        ]
    }

    #[test]
    fn expands_a_trigger() {
        assert_eq!(expand_snippets("sign off", &snips()), "Best, Byurhan");
    }

    #[test]
    fn is_case_insensitive() {
        assert_eq!(expand_snippets("Sign Off", &snips()), "Best, Byurhan");
    }

    #[test]
    fn expands_within_a_sentence() {
        assert_eq!(
            expand_snippets("here is my cal link ok", &snips()),
            "here is my cal.com/byurhan ok"
        );
    }

    #[test]
    fn ignores_partial_word_matches() {
        // "cal link" must not fire inside "physical inks".
        let input = "physical inks are fine";
        assert_eq!(expand_snippets(input, &snips()), input);
    }

    #[test]
    fn empty_snippets_are_a_no_op() {
        assert_eq!(expand_snippets("hello", &[]), "hello");
    }

    #[test]
    fn self_referential_replacement_terminates() {
        // "cal" -> "cal.com" used to loop forever: the replacement contains the trigger, and the
        // following "." is a word boundary, so restarting the scan matched again every time.
        let s = vec![("cal".to_string(), "cal.com".to_string())];
        assert_eq!(expand_snippets("my cal here", &s), "my cal.com here");
    }

    #[test]
    fn replacement_equal_to_trigger_terminates() {
        let s = vec![("x".to_string(), "x".to_string())];
        assert_eq!(expand_snippets("x and x", &s), "x and x");
    }

    #[test]
    fn handles_multibyte_text_before_a_trigger() {
        // Turkish 'İ' is 2 bytes but lowercases to 3, so offsets taken from a lowercased copy
        // no longer line up with the original. This used to splice mid-word or panic.
        let s = vec![("cal link".to_string(), "cal.com".to_string())];
        assert_eq!(
            expand_snippets("İstanbul cal link tomorrow", &s),
            "İstanbul cal.com tomorrow"
        );
    }

    #[test]
    fn keeps_scanning_after_a_partial_word_match() {
        // The first "cal link" is inside a word; the second is not and must still be replaced.
        let s = vec![("cal link".to_string(), "LINK".to_string())];
        assert_eq!(
            expand_snippets("physical inks then cal link", &s),
            "physical inks then LINK"
        );
    }

    #[test]
    fn replaces_every_occurrence() {
        let s = vec![("sign off".to_string(), "Best".to_string())];
        assert_eq!(
            expand_snippets("sign off and sign off", &s),
            "Best and Best"
        );
    }

    #[test]
    fn blank_triggers_are_skipped() {
        let s = vec![("   ".to_string(), "x".to_string())];
        assert_eq!(expand_snippets("hello", &s), "hello");
    }
}
