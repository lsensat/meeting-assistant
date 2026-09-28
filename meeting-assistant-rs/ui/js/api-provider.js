/**
 * The remote-provider fields — provider, URL, model, key — shared by Settings
 * and the setup wizard, so the two cannot drift apart.
 *
 * The URL is asked for only under "Other". Every listed provider has one fixed
 * endpoint, and typing it was where configurations went wrong: Anthropic's
 * full `/v1/messages` URL in a field that expected a base produced a setup that
 * could never summarise anything.
 *
 * The key never reaches this page. `apiKeyHint` returns at most its last four
 * characters, which is enough to recognise which key is saved.
 */

import * as api from "./api.js";
import { fillSelect } from "./dom.js";
import { enhanceCombo } from "./dropdown.js";
import { tr } from "./i18n.js";

/** `api_provider` for a provider the user supplies the URL for. */
export const CUSTOM = "custom";

/** A starting point per provider, for the model field's placeholder only. */
const MODEL_PLACEHOLDER = {
  anthropic: "claude-opus-5",
};

/**
 * @typedef {{
 *   provider: HTMLSelectElement,
 *   baseField: HTMLElement,
 *   baseUrl: HTMLInputElement,
 *   model: HTMLInputElement,
 *   modelList: HTMLDataListElement,
 *   loadModels: HTMLButtonElement,
 *   modelStatus: HTMLElement,
 *   key: HTMLInputElement,
 *   keyStatus: HTMLElement,
 * }} ApiFields
 */

/**
 * Fill the fields from `config` and wire them up.
 *
 * @param {ApiFields} f
 * @param {Record<string, unknown>} config
 */
export async function initApiFields(f, config) {
  const presets = await api.apiProviders();
  fillSelect(
    f.provider,
    [
      ...presets.map((p) => ({ value: p.id, label: p.label })),
      { value: CUSTOM, label: tr("api_provider_custom") },
    ],
    String(config.api_provider ?? presets[0]?.id ?? CUSTOM),
  );
  f.baseUrl.value = String(config.api_base_url ?? "");
  f.model.value = String(config.api_model ?? "");
  f.key.value = "";
  enhanceCombo(f.model, f.modelList);

  const apply = () => {
    f.baseField.hidden = f.provider.value !== CUSTOM;
    f.model.placeholder = MODEL_PLACEHOLDER[f.provider.value] ?? "";
  };
  apply();

  f.provider.addEventListener("change", () => {
    apply();
    // A list loaded for another provider would offer models this one lacks.
    f.modelList.replaceChildren();
    f.modelStatus.textContent = "";
  });

  f.loadModels.addEventListener("click", () => loadModels(f));
  await showKeyStatus(f);
}

/**
 * Show whether a key is saved and, when it is, its last four characters.
 *
 * @param {ApiFields} f
 */
export async function showKeyStatus(f) {
  const hint = await api.apiKeyHint();
  if (hint === null) {
    f.keyStatus.textContent = tr("api_key_missing");
    f.key.placeholder = "";
  } else if (hint === "") {
    f.keyStatus.textContent = tr("api_key_saved");
    f.key.placeholder = "••••••••••••";
  } else {
    f.keyStatus.textContent = tr("api_key_saved_hint", { last4: hint });
    f.key.placeholder = `••••••••••••${hint}`;
  }
}

/**
 * Fetch the provider's models into the model field's suggestions.
 *
 * Also the test of the key: listing models costs nothing, and a rejected key
 * fails here with the same message a summary would fail with — but now, not
 * after the next meeting.
 *
 * @param {ApiFields} f
 */
async function loadModels(f) {
  f.loadModels.disabled = true;
  f.modelStatus.textContent = tr("api_models_loading");
  try {
    const models = await api.listApiModels(f.provider.value, f.baseUrl.value.trim(), f.key.value);
    f.modelList.replaceChildren(
      ...models.map((id) => {
        const option = document.createElement("option");
        option.value = id;
        return option;
      }),
    );

    const chosen = f.model.value.trim();
    if (chosen && !models.includes(chosen)) {
      f.modelStatus.textContent = tr("api_model_not_listed", { model: chosen, count: models.length });
    } else {
      f.modelStatus.textContent = tr("api_models_loaded", { count: models.length });
    }
    // Empty field: the list is the point, so open it.
    if (!chosen) enhanceCombo(f.model, f.modelList).open();
  } catch (error) {
    f.modelStatus.textContent = String(error);
  } finally {
    f.loadModels.disabled = false;
  }
}

/**
 * The fields' values, merged into `config`.
 *
 * @param {ApiFields} f
 * @param {Record<string, unknown>} config
 */
export function collectApiFields(f, config) {
  config.api_provider = f.provider.value;
  config.api_base_url = f.baseUrl.value.trim();
  config.api_model = f.model.value.trim();
  return config;
}
