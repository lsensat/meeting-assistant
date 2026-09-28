/**
 * The app's own dropdown list, for every `<select>` and for the model field's
 * suggestions.
 *
 * The system draws both lists itself, and neither can be styled: the
 * suggestion list of a text field takes no CSS at all, and a select's popup
 * keeps the platform's border and shape. On the dark window the suggestion
 * list had no edge and ran into everything behind it, and the two kinds of
 * list looked unrelated.
 *
 * The native `<select>` stays in the page, hidden, and remains the source of
 * truth: code keeps reading `.value`, filling options with `fillSelect` and
 * listening for `change`. This file only draws it.
 */

const LIST_MAX_HEIGHT = 220;
const GAP = 4;

let listCount = 0;

/** The one list that is open, if any. Opening another closes it. */
let openList = null;

/**
 * @typedef {{ value: string, label: string, disabled?: boolean }} Item
 * @typedef {{
 *   anchor: HTMLElement,
 *   owner: HTMLElement,
 *   items: Item[],
 *   selected: string | null,
 *   onPick: (value: string) => void,
 *   onClose?: () => void,
 * }} ListRequest
 */

/**
 * Draw the list under `anchor`, or above it when there is no room below.
 *
 * Fixed to the viewport rather than placed inside the form: the Settings page
 * scrolls, and a list inside it would be cut off by the scroll container or
 * push the page longer.
 *
 * @param {ListRequest} request
 */
function showList(request) {
  closeList();
  const { anchor, owner, items, selected, onPick } = request;

  const list = document.createElement("ul");
  list.className = "dd-list";
  list.id = `dd-list-${++listCount}`;
  list.setAttribute("role", "listbox");

  const options = items.map((item, index) => {
    const li = document.createElement("li");
    li.id = `${list.id}-${index}`;
    li.className = "dd-option";
    li.setAttribute("role", "option");
    li.setAttribute("aria-selected", String(item.value === selected));
    if (item.disabled) li.setAttribute("aria-disabled", "true");

    const lamp = document.createElement("span");
    lamp.className = "dd-lamp";
    lamp.setAttribute("aria-hidden", "true");
    const text = document.createElement("span");
    text.className = "dd-text";
    text.textContent = item.label;
    li.append(lamp, text);

    // mousedown, not click: the field keeps focus, so the list is not closed
    // by the field's blur before the choice lands.
    li.addEventListener("mousedown", (event) => {
      event.preventDefault();
      if (item.disabled) return;
      onPick(item.value);
      closeList();
    });
    li.addEventListener("mousemove", () => setActive(index));
    list.append(li);
    return li;
  });

  document.body.append(list);
  place(list, anchor);

  const state = {
    list,
    owner,
    items,
    options,
    active: -1,
    onPick,
    onClose: request.onClose,
  };
  openList = state;
  owner.setAttribute("aria-expanded", "true");
  owner.setAttribute("aria-controls", list.id);

  const start = items.findIndex((item) => item.value === selected);
  setActive(start >= 0 ? start : -1);
  return state;
}

/** @param {HTMLElement} list @param {HTMLElement} anchor */
function place(list, anchor) {
  const rect = anchor.getBoundingClientRect();
  const below = window.innerHeight - rect.bottom - GAP * 2;
  const above = rect.top - GAP * 2;
  const wanted = Math.min(list.scrollHeight, LIST_MAX_HEIGHT);
  const up = below < wanted && above > below;

  list.style.left = `${Math.round(rect.left)}px`;
  list.style.minWidth = `${Math.round(rect.width)}px`;
  list.style.maxWidth = `${Math.max(rect.width, window.innerWidth - rect.left - GAP * 2)}px`;
  list.style.maxHeight = `${Math.max(80, Math.min(LIST_MAX_HEIGHT, up ? above : below))}px`;
  if (up) {
    list.style.top = "";
    list.style.bottom = `${Math.round(window.innerHeight - rect.top + GAP)}px`;
  } else {
    list.style.bottom = "";
    list.style.top = `${Math.round(rect.bottom + GAP)}px`;
  }
}

/** @param {number} index */
function setActive(index) {
  if (!openList) return;
  const { options, owner } = openList;
  options[openList.active]?.classList.remove("is-active");
  openList.active = index;
  const option = options[index];
  if (option) {
    option.classList.add("is-active");
    option.scrollIntoView({ block: "nearest" });
    owner.setAttribute("aria-activedescendant", option.id);
  } else {
    owner.removeAttribute("aria-activedescendant");
  }
}

/** @param {number} step */
function moveActive(step) {
  if (!openList) return;
  const { items } = openList;
  const n = items.length;
  let next = openList.active;
  for (let k = 0; k < n; k++) {
    next = next < 0 ? (step > 0 ? 0 : n - 1) : (next + step + n) % n;
    if (!items[next].disabled) break;
  }
  setActive(next);
}

function pickActive() {
  if (!openList) return false;
  const item = openList.items[openList.active];
  if (!item || item.disabled) return false;
  openList.onPick(item.value);
  closeList();
  return true;
}

function closeList() {
  if (!openList) return;
  const { list, owner, onClose } = openList;
  openList = null;
  list.remove();
  owner.setAttribute("aria-expanded", "false");
  owner.removeAttribute("aria-controls");
  owner.removeAttribute("aria-activedescendant");
  onClose?.();
}

/** @param {HTMLElement} owner */
const isOpenFor = (owner) => openList?.owner === owner;

// A fixed list would stay put while the page scrolls away under it.
window.addEventListener(
  "scroll",
  (event) => {
    if (openList && event.target !== openList.list) closeList();
  },
  true,
);
window.addEventListener("resize", closeList);
window.addEventListener("blur", closeList);

/** An id for `label[for=id]`, so the drawn control can be named by it. */
function labelId(select) {
  const label = select.id ? document.querySelector(`label[for="${select.id}"]`) : null;
  if (!label) return null;
  if (!label.id) label.id = `${select.id}-label`;
  return label.id;
}

const CARET =
  '<svg class="dd-caret" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 4.5 6 8l3.5-3.5"/></svg>';

/**
 * Draw one `<select>` with the app's list. Safe to call twice.
 *
 * @param {HTMLSelectElement} select
 */
export function enhanceSelect(select) {
  if (select.dataset.dd) return;
  select.dataset.dd = "1";

  const trigger = document.createElement("button");
  trigger.type = "button";
  trigger.className = "dd-trigger";
  trigger.setAttribute("role", "combobox");
  trigger.setAttribute("aria-haspopup", "listbox");
  trigger.setAttribute("aria-expanded", "false");
  const named = labelId(select);
  if (named) trigger.setAttribute("aria-labelledby", named);

  const text = document.createElement("span");
  text.className = "dd-value";
  trigger.append(text);
  trigger.insertAdjacentHTML("beforeend", CARET);

  select.classList.add("dd-native");
  select.tabIndex = -1;
  select.setAttribute("aria-hidden", "true");
  select.after(trigger);

  const sync = () => {
    text.textContent = select.selectedOptions[0]?.textContent ?? "";
    trigger.disabled = select.disabled;
    trigger.hidden = select.hidden;
    if (isOpenFor(trigger) && (select.disabled || select.hidden)) closeList();
  };
  sync();

  // Options are replaced wholesale by `fillSelect`, and a value set from code
  // fires no event, so both are watched rather than trusted to announce
  // themselves.
  new MutationObserver(sync).observe(select, {
    childList: true,
    subtree: true,
    characterData: true,
    attributes: true,
    attributeFilter: ["disabled", "hidden", "selected"],
  });
  select.addEventListener("change", sync);
  for (const prop of ["value", "selectedIndex"]) {
    const native = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, prop);
    Object.defineProperty(select, prop, {
      configurable: true,
      get() {
        return native.get.call(this);
      },
      set(v) {
        native.set.call(this, v);
        sync();
      },
    });
  }

  const items = () =>
    Array.from(select.options, (option) => ({
      value: option.value,
      label: option.textContent ?? "",
      disabled: option.disabled,
    }));

  const pick = (value) => {
    trigger.focus();
    if (value === select.value) return;
    select.value = value;
    select.dispatchEvent(new Event("input", { bubbles: true }));
    select.dispatchEvent(new Event("change", { bubbles: true }));
  };

  const open = () => {
    if (select.options.length === 0) return;
    showList({ anchor: trigger, owner: trigger, items: items(), selected: select.value, onPick: pick });
  };

  trigger.addEventListener("click", () => (isOpenFor(trigger) ? closeList() : open()));
  trigger.addEventListener("blur", () => {
    if (isOpenFor(trigger)) closeList();
  });

  // A click on the <label> focuses the hidden select; hand it on.
  select.addEventListener("focus", () => trigger.focus());

  let typed = "";
  let typedAt = 0;
  trigger.addEventListener("keydown", (event) => {
    const isOpen = isOpenFor(trigger);
    if (event.key.length !== 1 || event.key === " ") typed = "";
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp":
        event.preventDefault();
        if (!isOpen) open();
        else moveActive(event.key === "ArrowDown" ? 1 : -1);
        return;
      case "Home":
      case "End":
        if (!isOpen) return;
        event.preventDefault();
        setActive(event.key === "Home" ? 0 : openList.items.length - 1);
        return;
      case "Enter":
      case " ":
        event.preventDefault();
        if (!isOpen) open();
        else if (!pickActive()) closeList();
        return;
      case "Escape":
        if (!isOpen) return;
        event.preventDefault();
        event.stopPropagation();
        closeList();
        return;
      case "Tab":
        if (isOpen) closeList();
        return;
    }

    // Type-ahead, as a native select has: jump to the option starting with
    // what was typed in the last half second.
    if (event.key.length !== 1 || event.ctrlKey || event.metaKey || event.altKey) return;
    const now = Date.now();
    typed = now - typedAt > 500 ? event.key : typed + event.key;
    typedAt = now;
    const all = items();
    const index = all.findIndex((item) => !item.disabled && item.label.toLowerCase().startsWith(typed.toLowerCase()));
    if (index < 0) return;
    if (!isOpen) open();
    setActive(index);
  });
}

/**
 * Draw every `<select>` under `root`.
 *
 * @param {ParentNode} [root]
 */
export function enhanceSelects(root = document) {
  for (const select of root.querySelectorAll("select")) enhanceSelect(select);
}

/** @type {WeakMap<HTMLInputElement, { open: () => void }>} */
const combos = new WeakMap();

/**
 * Suggestions for a text field, from a `<datalist>`'s options, in the app's
 * list. The field stays free text: a model the provider has not listed can
 * still be typed. Safe to call twice.
 *
 * @param {HTMLInputElement} input
 * @param {HTMLDataListElement} source
 * @returns {{ open: () => void }} `open` shows the list, when it has anything
 */
export function enhanceCombo(input, source) {
  const existing = combos.get(input);
  if (existing) return existing;

  // The system list would open as well as this one.
  input.removeAttribute("list");
  input.setAttribute("role", "combobox");
  input.setAttribute("aria-autocomplete", "list");
  input.setAttribute("aria-expanded", "false");

  const wrap = document.createElement("span");
  wrap.className = "dd-combo";
  input.before(wrap);
  wrap.append(input);

  const toggle = document.createElement("button");
  toggle.type = "button";
  toggle.className = "dd-combo-toggle";
  toggle.tabIndex = -1;
  toggle.setAttribute("aria-hidden", "true");
  toggle.innerHTML = CARET;
  wrap.append(toggle);

  const all = () => Array.from(source.options, (option) => option.value).filter(Boolean);

  const sync = () => {
    toggle.hidden = all().length === 0;
  };
  sync();

  /** @param {boolean} everything ignore what is typed and show the full list */
  const show = (everything) => {
    const values = all();
    const typed = input.value.trim().toLowerCase();
    // What is typed narrows the list, until it names a model exactly: then
    // the whole list again, so the choice can be changed.
    const exact = values.some((v) => v.toLowerCase() === typed);
    const shown = everything || !typed || exact ? values : values.filter((v) => v.toLowerCase().includes(typed));
    if (shown.length === 0) {
      if (isOpenFor(input)) closeList();
      return;
    }
    showList({
      anchor: input,
      owner: input,
      items: shown.map((value) => ({ value, label: value })),
      selected: input.value.trim(),
      // No `input` event: that would narrow and reopen the list just chosen
      // from. Nothing listens for typing in this field; `change` is the
      // event a finished choice fires.
      onPick: (value) => {
        input.value = value;
        input.dispatchEvent(new Event("change", { bubbles: true }));
      },
    });
  };

  // A fresh list from "Load models" replaces the one on screen.
  new MutationObserver(() => {
    sync();
    if (isOpenFor(input)) show(false);
  }).observe(source, { childList: true, subtree: true, attributes: true });

  input.addEventListener("input", () => show(false));
  input.addEventListener("mousedown", () => {
    if (document.activeElement === input && !isOpenFor(input)) show(true);
  });
  input.addEventListener("click", () => {
    if (!isOpenFor(input)) show(true);
  });
  input.addEventListener("blur", () => {
    if (isOpenFor(input)) closeList();
  });
  input.addEventListener("keydown", (event) => {
    const isOpen = isOpenFor(input);
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp":
        event.preventDefault();
        if (!isOpen) show(true);
        else moveActive(event.key === "ArrowDown" ? 1 : -1);
        return;
      case "Enter":
        if (!isOpen) return;
        event.preventDefault();
        if (!pickActive()) closeList();
        return;
      case "Escape":
        if (!isOpen) return;
        event.preventDefault();
        event.stopPropagation();
        closeList();
        return;
      case "Tab":
        if (isOpen) closeList();
        return;
    }
  });

  toggle.addEventListener("mousedown", (event) => {
    event.preventDefault();
    if (isOpenFor(input)) {
      closeList();
    } else {
      input.focus();
      show(true);
    }
  });

  const controller = {
    open() {
      input.focus();
      show(true);
    },
  };
  combos.set(input, controller);
  return controller;
}
