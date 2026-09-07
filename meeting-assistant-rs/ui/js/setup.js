/**
 * First-run wizard.
 *
 * Two rules shape this file:
 *
 * 1. **Nothing is installed until asked.** The model step can be skipped
 *    entirely; recording never blocks on a download, and `pipeline::run`
 *    fetches a missing model when transcription starts.
 * 2. **The config is written once, at the end, as a complete object.**
 *    `save_config` replaces the whole file, so any key omitted from the payload
 *    reverts to its default. Spreading the loaded config is what prevents the
 *    wizard from silently resetting settings it never showed.
 */

// Installed before anything that can throw, so a failure in the modules
// below is reported on screen instead of leaving a blank or half-built window.
import "./errors.js";
import * as api from "./api.js";
import { applyLanguage, loadCatalog, setLanguage, tr } from "./i18n.js";

const el = (id) => document.getElementById(id);

const STEPS = 5;
const RECOMMENDED_MODEL = "small";

let step = 0;
/** The loaded config, mutated as the user moves through the wizard. */
let config = {};

// ---------------------------------------------------------------- steps

function render() {
  for (const section of document.querySelectorAll(".step")) {
    section.hidden = Number(section.dataset.step) !== step;
  }

  el("wizard-back").hidden = step === 0;
  el("wizard-next").textContent = step === STEPS - 1 ? tr("setup_finish") : tr("setup_next");
  el("step-counter").textContent = tr("setup_step", { index: step + 1, total: STEPS });

  el("wizard-progress").replaceChildren(
    ...Array.from({ length: STEPS }, (_, i) => {
      const dot = document.createElement("span");
      dot.className = i <= step ? "dot filled" : "dot";
      return dot;
    }),
  );
}

// ------------------------------------------------------- transcription

async function renderModels() {
  const models = await api.listWhisperModels();
  const list = el("model-list");
  list.replaceChildren();

  for (const model of models) {
    const row = document.createElement("div");
    row.className = "model-row";

    const label = document.createElement("span");
    label.textContent = `${model.id} · ${model.approx_mb} MB`;
    if (model.id === RECOMMENDED_MODEL) {
      const badge = document.createElement("span");
      badge.className = "badge";
      badge.textContent = tr("setup_recommended");
      label.append(" ", badge);
    }

    const action = document.createElement("button");
    action.className = "btn btn--secondary";
    if (model.installed) {
      action.textContent = tr("setup_installed");
      action.disabled = true;
    } else {
      action.textContent = tr("setup_install_now");
      action.addEventListener("click", () => downloadModel(model.id));
    }

    row.append(label, action);
    list.append(row);
  }
}

async function downloadModel(id) {
  // Selecting the model is what matters even if the download fails — the
  // pipeline will retry it at transcription time.
  config.whisper_model = id;
  for (const button of el("model-list").querySelectorAll("button")) button.disabled = true;

  try {
    await api.downloadWhisperModel(id);
    el("model-status").textContent = "";
    await renderModels();
  } catch (error) {
    el("model-status").textContent = String(error);
    await renderModels();
  }
}

// ------------------------------------------------------------- summary

function applyProviderVisibility() {
  const remote = el("setup-provider").value === "openai_compatible";
  el("setup-ollama").hidden = remote;
  el("setup-api").hidden = !remote;
}

function fillSelect(select, options, selected) {
  select.replaceChildren();
  for (const option of options) {
    const node = document.createElement("option");
    node.value = option.value;
    node.textContent = option.label;
    node.selected = option.value === selected;
    select.append(node);
  }
}

async function renderSummaryStep() {
  fillSelect(
    el("setup-provider"),
    [
      { value: "ollama", label: tr("summary_provider.ollama") },
      { value: "openai_compatible", label: tr("summary_provider.openai_compatible") },
    ],
    String(config.summary_provider ?? "ollama"),
  );

  const status = await api.listOllamaModels();
  el("setup-ollama-status").textContent = status.running
    ? tr("setup_ollama_found", { count: status.models.length })
    : tr("setup_ollama_not_running");

  fillSelect(
    el("setup-ollama-model"),
    status.models.map((m) => ({ value: m, label: m })),
    String(config.ollama_model ?? ""),
  );

  applyProviderVisibility();
}

// ------------------------------------------------------------- storage

function renderStorageStep() {
  fillSelect(
    el("setup-language"),
    [
      { value: "en", label: tr("app_language.en") },
      { value: "es", label: tr("app_language.es") },
    ],
    String(config.language ?? "en"),
  );
  el("setup-folder").value = String(config.output_folder ?? "");
}

// ------------------------------------------------------------ advancing

/**
 * Collect the current step into `config`, and refuse to advance if it is
 * incomplete. Returns false to stay put.
 */
async function commitStep() {
  if (step === 2) {
    config.summary_provider = el("setup-provider").value;

    if (config.summary_provider === "openai_compatible") {
      config.api_base_url = el("setup-api-base").value.trim();
      config.api_model = el("setup-api-model").value.trim();
      const key = el("setup-api-key").value;

      if (key) await api.setApiKey(key);
      const haveKey = key || (await api.hasApiKey());

      if (!config.api_base_url || !config.api_model || !haveKey) {
        el("setup-api-status").textContent = tr("api_incomplete");
        return false;
      }
    } else {
      config.ollama_model = el("setup-ollama-model").value;
    }
  }

  if (step === 3) {
    config.language = el("setup-language").value;
    config.output_folder = el("setup-folder").value;
  }

  return true;
}

async function enterStep() {
  if (step === 1) await renderModels();
  if (step === 2) await renderSummaryStep();
  if (step === 3) renderStorageStep();
  render();
}

async function finish() {
  config.setup_completed = true;
  await api.saveConfig(config);
  await api.closeSetup();
}

// --------------------------------------------------------------- wiring

function wire() {
  el("wizard-next").addEventListener("click", async () => {
    if (!(await commitStep())) return;
    if (step === STEPS - 1) {
      await finish();
      return;
    }
    step += 1;
    await enterStep();
  });

  el("wizard-back").addEventListener("click", async () => {
    if (step === 0) return;
    step -= 1;
    await enterStep();
  });

  el("setup-provider").addEventListener("change", applyProviderVisibility);

  el("setup-start-ollama").addEventListener("click", async () => {
    try {
      await api.startOllama();
    } catch (error) {
      el("setup-ollama-status").textContent = String(error);
      return;
    }
    // Give it a moment to bind the port before re-probing.
    setTimeout(renderSummaryStep, 3000);
  });

  el("setup-get-ollama").addEventListener("click", () => api.openUrl("https://ollama.com/download"));

  el("setup-browse").addEventListener("click", async () => {
    const chosen = await api.browseFolder();
    if (typeof chosen === "string" && chosen) el("setup-folder").value = chosen;
  });

  // Structured payload, localized here rather than in Rust, so progress shows
  // in the user's language and only in the window that started the download.
  api.on(api.EVENTS.whisperProgress, ({ model, percent }) => {
    el("model-status").textContent = tr("setup_downloading", { model, percent });
  });
}

async function main() {
  await loadCatalog();
  config = await api.getConfig();
  setLanguage(String(config.language ?? "en"));
  applyLanguage();
  wire();
  await enterStep();
}

main().catch((error) => {
  // Surfaced by errors.js as an on-screen banner.
  throw error;
});
