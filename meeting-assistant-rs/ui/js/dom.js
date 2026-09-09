/**
 * Small helpers shared by more than one window.
 *
 * Exists because three separate pages — the main window, Settings and the setup
 * wizard — are plain ES modules with no bundler, and each was growing its own
 * copy of the same few functions. `fillSelect` was duplicated character for
 * character under two names; the remote-provider check below was written twice
 * and could drift, leaving the wizard and Settings disagreeing about whether the
 * same configuration was usable.
 *
 * Keep this small. It is a place for things genuinely used twice, not a
 * utilities drawer.
 */

/**
 * Replace a `<select>`'s options.
 *
 * @param {HTMLSelectElement} select
 * @param {{value: string, label: string}[]} options
 * @param {string} selected value to mark selected, if it is present
 */
export function fillSelect(select, options, selected) {
  select.replaceChildren();
  for (const option of options) {
    const node = document.createElement("option");
    node.value = option.value;
    node.textContent = option.label;
    node.selected = option.value === selected;
    select.append(node);
  }
}

/**
 * Whether a remote summary provider has everything it needs.
 *
 * One definition, because Settings and the wizard both gate on it. The key is
 * deliberately two arguments: an empty box means "keep the one already in the
 * keychain", so a blank field with a stored key is complete, while a blank field
 * with no stored key is not.
 *
 * @param {{api_base_url?: string, api_model?: string}} config
 * @param {{typedKey: string, hasStoredKey: boolean}} key
 */
export function isRemoteProviderConfigured(config, { typedKey, hasStoredKey }) {
  const haveKey = Boolean(typedKey) || hasStoredKey;
  return Boolean(config.api_base_url) && Boolean(config.api_model) && haveKey;
}
