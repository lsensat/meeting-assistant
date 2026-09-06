/**
 * Settings window controller.
 *
 * The dropdowns are populated from live data (devices, installed Whisper
 * weights, Ollama models) rather than from hardcoded lists, so what the user
 * can pick always reflects what the machine actually has.
 */

import * as api from "./api.js";
import { applyLanguage, loadCatalog, setLanguage, tr } from "./i18n.js";
import { initTooltips } from "./tooltip.js";

const el = (id) => document.getElementById(id);

/** Working copy; only written back to Rust on Save. */
let config = {};

/**
 * @param {HTMLSelectElement} select
 * @param {{value: string, label: string}[]} options
 * @param {string} selected
 */
function fill(select, options, selected) {
  select.replaceChildren();
  for (const option of options) {
    const node = document.createElement("option");
    node.value = option.value;
    node.textContent = option.label;
    node.selected = option.value === selected;
    select.append(node);
  }
}

async function populateLanguages() {
  fill(
    el("app-language"),
    [
      { value: "en", label: tr("app_language.en") },
      { value: "es", label: tr("app_language.es") },
    ],
    String(config.language ?? "en"),
  );

  fill(
    el("transcription-language"),
    [
      { value: "auto", label: tr("transcription_language.auto") },
      { value: "en", label: tr("transcription_language.en") },
      { value: "es", label: tr("transcription_language.es") },
    ],
    String(config.transcription_language ?? "auto"),
  );

  fill(
    el("summary-type"),
    [
      { value: "meeting_minutes", label: tr("summary_type.meeting_minutes") },
      { value: "executive", label: tr("summary_type.executive") },
      { value: "actions", label: tr("summary_type.actions") },
      { value: "brief", label: tr("summary_type.brief") },
      { value: "custom", label: tr("summary_type.custom") },
    ],
    String(config.summary_type ?? "meeting_minutes"),
  );
}

async function populateWhisper() {
  const models = await api.listWhisperModels();

  // The id and the display label are kept apart on purpose. The Python stored
  // the display string and re-parsed it with `parse_whisper_value`, splitting
  // on whitespace to strip the suffix (`app.py:731`) — which breaks the moment
  // a label contains a space.
  fill(
    el("whisper-model"),
    models.map((model) => ({
      value: model.id,
      label: model.installed
        ? `${model.id} — ${tr("whisper_installed")}`
        : `${model.id} — ${tr("whisper_download")} (~${model.approx_mb} MB)`,
    })),
    String(config.whisper_model ?? "small"),
  );

  const chosen = models.find((model) => model.id === config.whisper_model);
  el("whisper-note").textContent = chosen?.installed
    ? tr("whisper_available")
    : tr("whisper_first_use");
}

/** Show the fields for the chosen engine, and the privacy note with them. */
function applyProviderVisibility() {
  const remote = el("summary-provider").value === "openai_compatible";
  el("ollama-field").hidden = remote;
  el("api-fields").hidden = !remote;
  el("api-privacy").hidden = !remote;
}

async function populateProvider() {
  fill(
    el("summary-provider"),
    [
      { value: "ollama", label: tr("summary_provider.ollama") },
      { value: "openai_compatible", label: tr("summary_provider.openai_compatible") },
    ],
    String(config.summary_provider ?? "ollama"),
  );

  el("api-base-url").value = String(config.api_base_url ?? "");
  el("api-model").value = String(config.api_model ?? "");
  el("api-key").value = "";
  el("api-key-status").textContent = (await api.hasApiKey())
    ? tr("api_key_saved")
    : tr("api_key_missing");

  applyProviderVisibility();
}

async function populateOllama() {
  const status = await api.listOllamaModels();
  const note = el("ollama-status");

  if (!status.running) {
    note.textContent = tr("ollama_not_responding");
    // No sentinel option. The Python put "No models installed" into the select
    // and `write_config` then persisted that string verbatim as the model name
    // (deferred fix #7); an empty select cannot do that.
    fill(el("ollama-model"), [], "");
    return;
  }

  if (status.models.length === 0) {
    note.textContent = tr("ollama_no_models");
    fill(el("ollama-model"), [], "");
    return;
  }

  note.textContent = tr("ollama_active_models", { count: status.models.length });
  fill(
    el("ollama-model"),
    status.models.map((model) => ({ value: model, label: model })),
    String(config.ollama_model ?? ""),
  );
}

async function populateDevices() {
  const devices = await api.listDevices();

  fill(
    el("microphone"),
    devices.microphones.map((device) => ({
      value: device.name,
      label: device.is_default ? `${device.name} (${tr("automatic")})` : device.name,
    })),
    String(config.microphone_name ?? ""),
  );

  fill(
    el("system-audio"),
    devices.system.map((device) => ({
      value: device.name,
      label: device.is_default ? `${device.name} (${tr("automatic")})` : device.name,
    })),
    String(config.system_audio_name ?? ""),
  );
}

function populateFiles() {
  el("output-folder").value = String(config.output_folder ?? "");
  el("keep-audio").checked = config.keep_audio !== false;
  el("custom-prompt").value = String(config.custom_summary_prompt ?? "");
}

async function populateAll() {
  await populateLanguages();
  await populateWhisper();
  await populateProvider();
  await populateOllama();
  await populateDevices();
  populateFiles();
}

function collect() {
  return {
    ...config,
    language: el("app-language").value,
    transcription_language: el("transcription-language").value,
    whisper_model: el("whisper-model").value,
    ollama_model: el("ollama-model").value,
    summary_type: el("summary-type").value,
    summary_provider: el("summary-provider").value,
    api_base_url: el("api-base-url").value.trim(),
    api_model: el("api-model").value.trim(),
    microphone_name: el("microphone").value,
    system_audio_name: el("system-audio").value,
    output_folder: el("output-folder").value,
    keep_audio: el("keep-audio").checked,
    custom_summary_prompt: el("custom-prompt").value,
  };
}

function wire() {
  // A live language switch re-renders every label and every option list. In the
  // Python this was 154 hand-written lines; here the option lists are the only
  // part that is not covered by the `[data-i18n]` walk.
  el("app-language").addEventListener("change", async () => {
    setLanguage(el("app-language").value);
    config = collect();
    applyLanguage();
    await populateAll();
  });

  el("whisper-model").addEventListener("change", async () => {
    config = collect();
    await populateWhisper();
  });

  el("summary-provider").addEventListener("change", applyProviderVisibility);

  el("refresh-models").addEventListener("click", async () => {
    config = collect();
    await populateOllama();
  });

  el("refresh-devices").addEventListener("click", async () => {
    config = collect();
    await api.refreshDevices();
    await populateDevices();
  });

  el("browse-folder").addEventListener("click", async () => {
    const chosen = await api.browseFolder();
    if (typeof chosen === "string" && chosen) el("output-folder").value = chosen;
  });

  el("save-settings").addEventListener("click", async () => {
    const next = collect();

    // The custom summary type is the only one that needs its prompt; the
    // original refused to save an empty one (`custom_prompt_required`).
    if (next.summary_type === "custom" && !next.custom_summary_prompt.trim()) {
      el("custom-prompt-note").textContent = tr("custom_prompt_required");
      return;
    }

    // The key is stored separately, in the keychain, and only when the user
    // typed one — an empty box means "keep what is already saved".
    const typedKey = el("api-key").value;
    if (typedKey) await api.setApiKey(typedKey);

    if (next.summary_provider === "openai_compatible") {
      const haveKey = typedKey || (await api.hasApiKey());
      if (!next.api_base_url || !next.api_model || !haveKey) {
        el("api-key-status").textContent = tr("api_incomplete");
        return;
      }
    }

    await api.saveConfig(next);
    config = next;
    await api.closeSettings();
  });
}

async function main() {
  await loadCatalog();
  config = await api.getConfig();
  setLanguage(String(config.language ?? "en"));

  applyLanguage();
  await populateAll();
  initTooltips(el("tooltip"));
  wire();
}

main();
