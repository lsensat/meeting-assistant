//! Prompt templates for the Ollama summarisation pass.
//! Port of `summary_instructions` and `summarize_with_ollama` (`app.py:1962-2205`).
//!
//! Summarisation is map-reduce: each transcript chunk gets an extraction pass,
//! the partial results are concatenated, and one final pass renders them into
//! the chosen summary format.
//!
//! # The `\n` fix
//!
//! The Python built these messages with `f"{a}\\n\\n{b}"` — an escaped
//! backslash, so the model received the literal characters `\n\n` instead of
//! blank lines, gluing the transcript onto the end of the instruction text.
//! That was fixed in `app.py` before this port; the joins here use real
//! newlines. Do not reintroduce the escape.

use crate::config::{Language, SummaryType};

/// Separator between partial summaries in the reduce step.
const PARTIAL_SEPARATOR: &str = "\n\n---\n\n";

/// System prompt, sent with both the extraction and final passes.
///
/// Note it names the speaker labels, which are themselves localised
/// (`ME`/`MEETING` against `YO`/`REUNION`), so the prompt and the transcript
/// have to agree on language.
pub fn system_prompt(language: Language) -> &'static str {
    match language {
        Language::Es => {
            "
Eres un asistente especializado en analizar
reuniones de trabajo.

La información procede de una transcripción
automática y puede contener errores.

YO representa el micrófono del usuario.
REUNION representa el audio del ordenador.

No inventes información, responsables, fechas,
decisiones, acciones ni conclusiones.
Si algo no está claro, indícalo.
Responde siempre en español.
"
        }
        Language::En => {
            "
You are an assistant specialized in analyzing
work meetings.

The information comes from an automatic
transcript and may contain errors.

ME represents the user's microphone.
MEETING represents computer audio.

Do not invent information, owners, dates,
decisions, actions or conclusions.
If something is unclear, say so.
Always respond in English.
"
        }
    }
}

/// Instruction prefixed to each transcript chunk in the map step.
pub fn extraction_prompt(language: Language) -> &'static str {
    match language {
        Language::Es => {
            "
Extrae únicamente la información relevante:
temas, decisiones, acciones, responsables,
fechas, pendientes y riesgos.
No inventes datos.

TRANSCRIPCIÓN:
"
        }
        Language::En => {
            "
Extract only the relevant information:
topics, decisions, actions, owners, dates,
pending items and risks.
Do not invent information.

TRANSCRIPT:
"
        }
    }
}

/// The short reminder repeated in the final pass.
pub fn no_invent(language: Language) -> &'static str {
    match language {
        Language::Es => "No inventes información.",
        Language::En => "Do not invent information.",
    }
}

/// The format instruction for the final pass.
///
/// `custom_prompt` is only consulted for [`SummaryType::Custom`]; when it is
/// blank the language-specific fallback is used. Note the fallbacks are not
/// translations of each other — Spanish gets a one-liner while English reuses
/// the long default from `DEFAULT_CONFIG`. That asymmetry is in the original
/// and is preserved.
pub fn summary_instructions(
    language: Language,
    summary_type: SummaryType,
    custom_prompt: &str,
) -> String {
    if summary_type == SummaryType::Custom {
        let trimmed = custom_prompt.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
        return match language {
            Language::Es => "Resume la reunión sin inventar información.".to_string(),
            Language::En => crate::config::DEFAULT_CUSTOM_PROMPT.to_string(),
        };
    }

    let template = match (language, summary_type) {
        (Language::Es, SummaryType::Executive) => {
            "
Genera un resumen ejecutivo de la reunión.
Incluye:
# Resumen ejecutivo
# Decisiones clave
# Próximos pasos
Sé conciso y prioriza lo relevante.
"
        }
        (Language::Es, SummaryType::Actions) => {
            "
Céntrate en información accionable.
Usa:
# Decisiones tomadas
# Acciones
Para cada acción indica responsable y fecha
solo si aparecen explícitamente.
# Temas pendientes
# Bloqueos o riesgos
"
        }
        (Language::Es, SummaryType::Brief) => {
            "
Genera un resumen muy breve.
Usa:
# Resumen
# Decisiones
# Acciones
No añadas detalle innecesario.
"
        }
        (Language::Es, _) => {
            "
Genera un acta de reunión.
Usa:
# Resumen ejecutivo
# Decisiones tomadas
# Acciones
Para cada acción indica, solo si se conoce:
- Acción
- Responsable
- Fecha límite
# Temas pendientes
# Riesgos o problemas
# Otros puntos relevantes
"
        }

        (Language::En, SummaryType::Executive) => {
            "
Generate an executive summary of the meeting.
Use:
# Executive summary
# Key decisions
# Next steps
Be concise and prioritize what matters.
"
        }
        (Language::En, SummaryType::Actions) => {
            "
Focus on actionable information.
Use:
# Decisions
# Actions
For each action include owner and due date
only when explicitly stated.
# Pending topics
# Blockers or risks
"
        }
        (Language::En, SummaryType::Brief) => {
            "
Generate a very brief meeting summary.
Use:
# Summary
# Decisions
# Actions
Avoid unnecessary detail.
"
        }
        (Language::En, _) => {
            "
Generate meeting minutes.
Use:
# Executive summary
# Decisions
# Actions
For each action include, only when known:
- Action
- Owner
- Due date
# Pending topics
# Risks or issues
# Other relevant points
"
        }
    };

    template.to_string()
}

/// The user message for one chunk in the map step.
pub fn extraction_message(language: Language, chunk: &str) -> String {
    format!("{}\n\n{}", extraction_prompt(language), chunk)
}

/// Join the per-chunk results before the final pass.
pub fn combine_partials(partials: &[String]) -> String {
    partials.join(PARTIAL_SEPARATOR)
}

/// The user message for the final pass.
pub fn final_message(
    language: Language,
    summary_type: SummaryType,
    custom_prompt: &str,
    combined: &str,
) -> String {
    format!(
        "{}\n\n{}\n\n{}",
        summary_instructions(language, summary_type, custom_prompt),
        no_invent(language),
        combined
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_and_type_yields_a_non_empty_instruction() {
        for language in [Language::En, Language::Es] {
            for summary_type in SummaryType::ALL {
                let s = summary_instructions(language, summary_type, "");
                assert!(
                    !s.trim().is_empty(),
                    "{language:?}/{summary_type:?} was empty"
                );
            }
        }
    }

    #[test]
    fn non_custom_types_ignore_the_custom_prompt() {
        let with = summary_instructions(Language::En, SummaryType::Brief, "IGNORE ME");
        assert!(!with.contains("IGNORE ME"));
    }

    #[test]
    fn custom_type_uses_the_custom_prompt() {
        let s = summary_instructions(Language::En, SummaryType::Custom, "  Just the actions.  ");
        assert_eq!(s, "Just the actions.");
    }

    #[test]
    fn blank_custom_prompt_falls_back_per_language() {
        assert_eq!(
            summary_instructions(Language::Es, SummaryType::Custom, "   "),
            "Resume la reunión sin inventar información."
        );
        assert_eq!(
            summary_instructions(Language::En, SummaryType::Custom, ""),
            crate::config::DEFAULT_CUSTOM_PROMPT
        );
    }

    #[test]
    fn instructions_carry_the_expected_headings() {
        let minutes = summary_instructions(Language::En, SummaryType::MeetingMinutes, "");
        for heading in ["# Executive summary", "# Decisions", "# Actions"] {
            assert!(minutes.contains(heading), "missing {heading}");
        }

        let actas = summary_instructions(Language::Es, SummaryType::MeetingMinutes, "");
        for heading in ["# Resumen ejecutivo", "# Decisiones tomadas", "# Acciones"] {
            assert!(actas.contains(heading), "missing {heading}");
        }
    }

    #[test]
    fn system_prompt_names_the_localised_speaker_labels() {
        assert!(system_prompt(Language::En).contains("ME represents"));
        assert!(system_prompt(Language::En).contains("MEETING represents"));
        assert!(system_prompt(Language::Es).contains("YO representa"));
        assert!(system_prompt(Language::Es).contains("REUNION representa"));
    }

    // --- the \n regression ------------------------------------------------

    #[test]
    fn messages_use_real_newlines_not_escaped_ones() {
        let msg = extraction_message(Language::En, "[00:00:00] ME: hello");
        assert!(
            !msg.contains("\\n"),
            "literal backslash-n leaked into the prompt"
        );
        assert!(msg.contains("\n\n"));
        assert!(msg.ends_with("[00:00:00] ME: hello"));
    }

    #[test]
    fn partials_are_joined_with_a_real_separator() {
        let combined = combine_partials(&["first".to_string(), "second".to_string()]);
        assert_eq!(combined, "first\n\n---\n\nsecond");
        assert!(!combined.contains("\\n"));
    }

    #[test]
    fn final_message_orders_instruction_reminder_then_content() {
        let msg = final_message(Language::En, SummaryType::Brief, "", "PARTIALS");
        assert!(!msg.contains("\\n"));

        let reminder = msg.find(no_invent(Language::En)).expect("reminder present");
        let content = msg.find("PARTIALS").expect("content present");
        assert!(reminder < content, "reminder must precede the content");
        assert!(msg.ends_with("PARTIALS"));
    }

    #[test]
    fn single_partial_needs_no_separator() {
        assert_eq!(combine_partials(&["only".to_string()]), "only");
    }
}
