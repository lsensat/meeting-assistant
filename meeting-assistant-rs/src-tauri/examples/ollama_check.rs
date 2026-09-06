//! Manual check against a live Ollama. Not a test: it needs a running server.
//!   cargo run --example ollama_check
use meeting_assistant::ollama;

fn main() {
    println!("running: {}", ollama::is_running());
    match ollama::list_models() {
        Ok(models) => {
            println!("models ({}):", models.len());
            for m in &models {
                println!("  {m}");
            }
            if let Some(model) = models.first() {
                println!("\nchat round-trip with {model}...");
                match ollama::chat(
                    model,
                    "You are terse. Reply with exactly one word.",
                    "Say the word: ready",
                ) {
                    Ok(reply) => println!("  reply: {:?}", reply.trim()),
                    Err(e) => println!("  chat failed: {e}"),
                }
            }
        }
        Err(e) => println!("list_models failed: {e}"),
    }
}
