//! Temporary CLI driver for Phase M3: run the post-recording pipeline over an
//! existing meeting folder. Pairs with the `rec` binary from Phase M2.
//!
//! ```text
//! cargo run --release --bin process -- --folder /tmp/ma-test1
//! cargo run --release --bin process -- --folder /tmp/x --model base --ollama qwen3.5:4b
//! cargo run --release --bin process -- --download base
//! cargo run --release --bin process -- --list
//! ```

use std::path::PathBuf;
use std::time::Instant;

use meeting_assistant::summary::ProviderConfig;
use meeting_assistant::pipeline::{self, PipelineConfig, Progress, Stage, StageState};
use meeting_assistant::{ollama, platform, whisper};
use meeting_core::config::{SummaryProvider, Language, SummaryType};
use meeting_core::i18n;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--list") {
        list();
        return;
    }

    if let Some(model) = flag(&args, "--download") {
        download(&model);
        return;
    }

    let Some(folder) = flag(&args, "--folder").map(PathBuf::from) else {
        eprintln!("usage: process --folder <meeting folder> [--model base] [--ollama <model>]");
        eprintln!("       process --download <model>");
        eprintln!("       process --list");
        std::process::exit(2);
    };

    let whisper_model = flag(&args, "--model").unwrap_or_else(|| "base".into());
    let language = match flag(&args, "--language").as_deref() {
        Some("es") => Language::Es,
        _ => Language::En,
    };

    let ollama_model = match flag(&args, "--ollama") {
        Some(model) => model,
        None => match ollama::list_models() {
            Ok(models) if !models.is_empty() => models[0].clone(),
            Ok(_) => {
                eprintln!("Ollama is running but has no models installed.");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
    };

    let config = PipelineConfig {
        // Empty means "leave it where it is" — the CLI is pointed at a folder
        // that already exists and must not relocate it.
        output_folder: PathBuf::new(),
        folder,
        meeting_title: flag(&args, "--title").unwrap_or_default(),
        whisper_model,
        transcription_language: flag(&args, "--transcription-language"),
        // The CLI drives the local path only. Exercising a remote endpoint
        // from here would mean handling keychain prompts in a terminal.
        provider: ProviderConfig {
            provider: SummaryProvider::Ollama,
            ollama_model,
            api_base_url: String::new(),
            api_model: String::new(),
        },
        language,
        summary_type: SummaryType::MeetingMinutes,
        custom_summary_prompt: String::new(),
        keep_audio: true,
        speaker_me: i18n::tr(language, "speaker_me").to_string(),
        speaker_meeting: i18n::tr(language, "speaker_meeting").to_string(),
    };

    println!("folder  : {}", config.folder.display());
    println!("whisper : {}", config.whisper_model);
    println!("ollama  : {}", config.provider.ollama_model);
    println!("language: {}\n", language.as_str());

    let started = Instant::now();
    let mut last_status = String::new();

    let result = pipeline::run(config, |progress| match progress {
        Progress::Stage(stage, state) => {
            println!("\n[{}] {}", stage_name(stage), state_name(state));
        }
        Progress::Status(status) => {
            // Percent updates repeat constantly; only print real changes.
            if status != last_status {
                println!("  {status}");
                last_status = status;
            }
        }
    });

    match result {
        Ok(output) => {
            println!("\n----------------------------------------------------------");
            println!("done in {:.1}s", started.elapsed().as_secs_f64());
            println!("segments   : {}", output.segment_count);
            println!("transcript : {}", output.transcript_file.display());
            println!("summary    : {}", output.summary_file.display());
            println!("----------------------------------------------------------");
        }
        Err(e) => {
            eprintln!("\nFAILED: {e}");
            std::process::exit(1);
        }
    }
}

fn list() {
    println!("whisper models in {}:", platform::models_dir().display());
    for spec in whisper::MODELS {
        let mark = if whisper::is_installed(spec.id) {
            "installed"
        } else {
            "-"
        };
        println!("  {:<10} ~{:>5} MB  {mark}", spec.id, spec.approx_mb);
    }

    println!("\nollama at {}:", ollama::BASE_URL);
    match ollama::list_models() {
        Ok(models) if models.is_empty() => println!("  running, no models installed"),
        Ok(models) => {
            for model in models {
                println!("  {model}");
            }
        }
        Err(e) => println!("  {e}"),
    }

    match platform::find_ollama() {
        Some(path) => println!("\nollama binary: {}", path.display()),
        None => println!("\nollama binary: not found"),
    }
}

fn download(model: &str) {
    println!("downloading {model} to {}", whisper::model_path(model).display());
    let mut last = u8::MAX;
    match whisper::download_model(model, |percent| {
        if percent != last && percent % 5 == 0 {
            println!("  {percent}%");
            last = percent;
        }
    }) {
        Ok(path) => println!("saved to {}", path.display()),
        Err(e) => {
            eprintln!("FAILED: {e}");
            std::process::exit(1);
        }
    }
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Audio => "audio",
        Stage::Whisper => "whisper",
        Stage::Summary => "summary",
    }
}

fn state_name(state: StageState) -> &'static str {
    match state {
        StageState::Pending => "pending",
        StageState::Working => "working",
        StageState::Done => "done",
        StageState::Error => "error",
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let index = args.iter().position(|a| a == name)?;
    args.get(index + 1).cloned()
}
