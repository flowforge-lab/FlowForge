use super::*;

#[test]
fn empty_input_is_new_session() {
    assert_eq!(auto_title(""), "New session");
    assert_eq!(auto_title("   "), "New session");
}

#[test]
fn skips_leading_stop_words() {
    // "how do i" are all stop-words; lands on "deploy".
    assert_eq!(auto_title("how do i deploy"), "Deploy");
}

#[test]
fn keeps_at_least_one_word_when_all_stop_words() {
    // Every word is a stop-word; the loop keeps the last one.
    assert_eq!(auto_title("please help me"), "Me");
}

#[test]
fn scales_word_count_with_length() {
    // <= 25 chars -> 2 words (only *leading* stop-words are skipped).
    assert_eq!(auto_title("deploy parser service now"), "Deploy parser");
    // 26..=50 chars -> 3 words.
    let s = "refactor the session store and its tests";
    assert_eq!(s.len(), 40);
    assert_eq!(auto_title(s), "Refactor the session");
}

#[test]
fn strips_punctuation_when_matching_stop_words() {
    // "How," normalizes to "how" (a stop-word) and is skipped.
    assert_eq!(auto_title("How, exactly?"), "Exactly?");
}

#[test]
fn capitalizes_first_letter() {
    assert_eq!(auto_title("parser"), "Parser");
}
