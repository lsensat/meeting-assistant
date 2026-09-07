//! Turning raw OS device names into something worth showing a person.
//!
//! Windows names a device by its role first and the hardware second:
//! `"Micrófono de los auriculares con micrófono (Plantronics Blackwire 3225
//! Series)"`. The part anyone recognises is in the parentheses, and the prefix
//! is both long enough to wrap the panel and localised by Windows, so it cannot
//! be matched against a fixed list.
//!
//! macOS names carry no parentheses and pass through untouched, so this needs
//! no `cfg`.
//!
//! # These are labels, never identities
//!
//! A device's raw name is the config key: `Config::microphone_name` stores it,
//! the recorder resolves against it, and the tray encodes it into menu item ids.
//! Nothing here may be fed back into any of those. It exists only to be
//! displayed.

use std::collections::HashMap;

/// Display labels for a list of raw device names, one per input, in order.
///
/// Takes the whole list rather than one name because shortening can collide:
/// a single adapter can expose `"Microphone (USB Audio)"` and
/// `"Line In (USB Audio)"`, which both reduce to `"USB Audio"` and would leave
/// the user picking blind between two identical rows. Colliding entries keep
/// their full names; everything else still shortens.
pub fn display_labels(names: &[String]) -> Vec<String> {
    let shortened: Vec<&str> = names.iter().map(|n| shorten(n)).collect();

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for label in &shortened {
        *counts.entry(label).or_insert(0) += 1;
    }

    shortened
        .iter()
        .zip(names)
        .map(|(label, raw)| {
            if counts[label] > 1 {
                raw.clone()
            } else {
                (*label).to_string()
            }
        })
        .collect()
}

/// The parenthesised tail of a name, or the whole name when there isn't one.
///
/// Anchored on the **last** `)` rather than the first, because the hardware name
/// can itself contain parentheses — `"Micrófono (Realtek(R) Audio)"` has to
/// yield `"Realtek(R) Audio"`, not `"Realtek(R"`.
fn shorten(name: &str) -> &str {
    let trimmed = name.trim_end();
    if !trimmed.ends_with(')') {
        return name;
    }

    let Some(open) = trimmed.find('(') else {
        return name;
    };
    let close = trimmed.len() - 1;

    let inner = trimmed[open + 1..close].trim();
    // A name that is *only* parentheses, or empty inside them, tells the user
    // less than the original.
    if inner.is_empty() || open == 0 {
        name
    } else {
        inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(names: &[&str]) -> Vec<String> {
        display_labels(&names.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_windows_name_reduces_to_the_hardware() {
        // Verbatim from the Windows machine that reported this.
        assert_eq!(
            labels(&["Micrófono de los auriculares con micrófono (Plantronics Blackwire 3225 Series)"]),
            vec!["Plantronics Blackwire 3225 Series"],
        );
    }

    #[test]
    fn nested_parentheses_anchor_on_the_last_one() {
        assert_eq!(labels(&["Micrófono (Realtek(R) Audio)"]), vec!["Realtek(R) Audio"]);
    }

    #[test]
    fn a_name_without_parentheses_is_untouched() {
        // Every macOS name looks like this.
        assert_eq!(labels(&["MacBook Air Speakers"]), vec!["MacBook Air Speakers"]);
    }

    #[test]
    fn colliding_names_keep_their_full_form() {
        // Both reduce to "USB Audio"; shortening would make them indistinguishable.
        let out = labels(&["Microphone (USB Audio)", "Line In (USB Audio)"]);
        assert_eq!(out, vec!["Microphone (USB Audio)", "Line In (USB Audio)"]);
    }

    #[test]
    fn a_collision_does_not_stop_others_shortening() {
        let out = labels(&[
            "Microphone (USB Audio)",
            "Line In (USB Audio)",
            "Micrófono (Plantronics Blackwire 3225 Series)",
        ]);
        assert_eq!(out[2], "Plantronics Blackwire 3225 Series");
    }

    #[test]
    fn a_name_that_is_only_parentheses_is_left_alone() {
        assert_eq!(labels(&["(Realtek Audio)"]), vec!["(Realtek Audio)"]);
        assert_eq!(labels(&["Microphone ()"]), vec!["Microphone ()"]);
    }
}
