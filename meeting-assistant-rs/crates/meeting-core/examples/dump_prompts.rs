//! Dump every prompt string as JSON so it can be diffed against the Python
//! original. Parity harness, not application code.
//!
//! ```text
//! cargo run --example dump_prompts
//! ```
//!
//! Delete this once `app.py` is gone.

use meeting_core::config::{Language, SummaryType};
use meeting_core::prompts;

fn main() {
    let mut out = serde_json::Map::new();

    for (tag, language) in [("en", Language::En), ("es", Language::Es)] {
        out.insert(
            format!("system_prompt.{tag}"),
            prompts::system_prompt(language).into(),
        );
        out.insert(
            format!("extraction_prompt.{tag}"),
            prompts::extraction_prompt(language).into(),
        );
        out.insert(
            format!("no_invent.{tag}"),
            prompts::no_invent(language).into(),
        );

        for summary_type in SummaryType::ALL {
            if summary_type == SummaryType::Custom {
                continue; // not a literal; it comes from config
            }
            out.insert(
                format!("instructions.{tag}.{}", summary_type.as_str()),
                prompts::summary_instructions(language, summary_type, "").into(),
            );
        }
    }

    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
