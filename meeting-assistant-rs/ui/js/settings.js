/**
 * Settings window controller.
 *
 * The dropdowns are populated from live data (devices, installed Whisper
 * weights, Ollama models) rather than from hardcoded lists, so what the user
 * can pick always reflects what the machine actually has.
 */

// Installed before anything that can throw, so a failure in the modules
// below is reported on screen instead of leaving a blank or half-built window.
import "./errors.js";
import * as api from "./api.js";
import { fillSelect, isRemoteProviderConfigured } from "./dom.js";
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
async function populateLanguages() {
  fillSelect(
    el("app-language"),
    [
      { value: "en", label: tr("app_language.en") },
      { value: "es", label: tr("app_language.es") },
    ],
    String(config.language ?? "en"),
  );

  fillSelect(
    el("transcription-language"),
    [
      { value: "auto", label: tr("transcription_language.auto") },
      { value: "en", label: tr("transcription_language.en") },
      { value: "es", label: tr("transcription_language.es") },
    ],
    String(config.transcription_language ?? "auto"),
  );

  fillSelect(
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
  fillSelect(
    el("whisper-model"),
    models.map((model) => ({
      value: model.id,
      // Both catalog strings already contain `{model}`; calling `tr` without
      // the argument left the placeholder on screen as a literal —
      // "small — {model} installed". The id must be passed, not prefixed.
      label: model.installed
        ? tr("whisper_installed", { model: model.id })
        : `${tr("whisper_download", { model: model.id })} (~${model.approx_mb} MB)`,
    })),
    String(config.whisper_model ?? "small"),
  );

  const chosen = models.find((model) => model.id === config.whisper_model);
  el("whisper-note").textContent = chosen?.installed
    ? tr("whisper_available")
    : tr("whisper_first_use");

  renderDownloaded(models);
}

/** Bytes as the user's file manager would show them. */
function formatSize(bytes) {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
  return `${Math.round(bytes / 1024 ** 2)} MB`;
}

/**
 * The installed models, with what each is costing on disk.
 *
 * Only the installed ones: the dropdown above already lists all five, and this
 * exists to answer "what can I delete to get space back".
 *
 * @param {{id: string, installed: boolean, size_bytes: number}[]} models
 */
function renderDownloaded(models) {
  const list = el("downloaded-list");
  list.replaceChildren();

  const installed = models.filter((model) => model.installed);
  const total = installed.reduce((sum, model) => sum + model.size_bytes, 0);
  el("downloaded-total").textContent = total > 0 ? formatSize(total) : "";

  if (installed.length === 0) {
    const empty = document.createElement("div");
    empty.className = "helper";
    empty.textContent = tr("no_models_downloaded");
    list.append(empty);
    return;
  }

  for (const model of installed) {
    const row = document.createElement("div");
    row.className = "model-row";

    const label = document.createElement("span");
    label.textContent = `${model.id} · ${formatSize(model.size_bytes)}`;

    const remove = document.createElement("button");
    remove.className = "trash-button";
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    // Heroicons outline, trash.
    path.setAttribute(
      "d",
      "m14.74 9-.346 9m-4.788 0L9.26 9m9.968-3.21c.342.052.682.107 1.022.166m-1.022-.165L18.16 " +
        "19.673a2.25 2.25 0 0 1-2.244 2.077H8.084a2.25 2.25 0 0 1-2.244-2.077L4.772 5.79m14.456 " +
        "0a48.108 48.108 0 0 0-3.478-.397m-12 .562c.34-.059.68-.114 1.022-.165m0 0a48.11 48.11 0 " +
        "0 1 3.478-.397m7.5 0v-.916c0-1.18-.91-2.164-2.09-2.201a51.964 51.964 0 0 0-3.32 0c-1.18 " +
        ".037-2.09 1.022-2.09 2.201v.916m7.5 0a48.667 48.667 0 0 0-7.5 0",
    );
    svg.append(path);
    remove.append(svg);

    // The one guard the user meets constantly, so it is explained in place
    // rather than only as a rejected command. `delete_whisper_model` refuses
    // this too — the UI is not trusted to be the only gate.
    const isSelected = model.id === config.whisper_model;
    remove.disabled = isSelected;
    remove.setAttribute(
      "data-tooltip",
      isSelected ? "delete_model_selected" : "tooltip_delete_model",
    );
    remove.setAttribute("aria-label", tr(isSelected ? "delete_model_selected" : "tooltip_delete_model"));
    if (!isSelected) remove.addEventListener("click", () => confirmDelete(model));

    row.append(label, remove);
    list.append(row);
  }
}

/**
 * Ask before removing gigabytes.
 *
 * @param {{id: string, size_bytes: number}} model
 */
function confirmDelete(model) {
  const modal = el("delete-modal");
  el("delete-title").textContent = tr("delete_model_title", { model: model.id });
  el("delete-message").textContent = tr("delete_model_body", {
    size: formatSize(model.size_bytes),
  });

  const close = () => {
    modal.hidden = true;
    document.removeEventListener("keydown", onKey);
  };
  const onKey = (event) => {
    if (event.key === "Escape") close();
  };

  el("delete-no").onclick = close;
  el("delete-yes").onclick = async () => {
    close();
    try {
      await api.deleteWhisperModel(model.id);
    } catch (error) {
      el("whisper-note").textContent = String(error);
    }
    // Re-read rather than mutating the list in place: the command may have
    // refused, and the disk is the only thing that knows what is really there.
    await populateWhisper();
  };

  document.addEventListener("keydown", onKey);
  modal.hidden = false;
  el("delete-no").focus();
}

/** Show the fields for the chosen engine, and the privacy note with them. */
function applyProviderVisibility() {
  const remote = el("summary-provider").value === "openai_compatible";
  el("ollama-field").hidden = remote;
  el("api-fields").hidden = !remote;
  el("api-privacy").hidden = !remote;
}

async function populateProvider() {
  fillSelect(
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
    fillSelect(el("ollama-model"), [], "");
    return;
  }

  if (status.models.length === 0) {
    note.textContent = tr("ollama_no_models");
    fillSelect(el("ollama-model"), [], "");
    return;
  }

  note.textContent = tr("ollama_active_models", { count: status.models.length });
  fillSelect(
    el("ollama-model"),
    status.models.map((model) => ({ value: model, label: model })),
    String(config.ollama_model ?? ""),
  );
}

async function populateDevices() {
  const devices = await api.listDevices();

  fillSelect(
    el("microphone"),
    // `label` on screen, `name` as the value. The raw name is the identity the
    // recorder resolves against and must stay the stored value, but showing it
    // put "Micrófono de los auriculares con micrófono (Plantronics Blackwire
    // 3225 Series)" in a 375px window while the main window showed the short
    // form for the same device. One rule, everywhere.
    devices.microphones.map((device) => ({
      value: device.name,
      label: device.is_default
        ? `${device.label ?? device.name} (${tr("automatic")})`
        : (device.label ?? device.name),
    })),
    String(config.microphone_name ?? ""),
  );

  fillSelect(
    el("system-audio"),
    devices.system.map((device) => ({
      value: device.name,
      label: device.is_default
        ? `${device.label ?? device.name} (${tr("automatic")})`
        : (device.label ?? device.name),
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
      const complete = isRemoteProviderConfigured(next, {
        typedKey,
        hasStoredKey: await api.hasApiKey(),
      });
      if (!complete) {
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

main().catch((error) => {
  // Surfaced by errors.js as an on-screen banner.
  throw error;
});
