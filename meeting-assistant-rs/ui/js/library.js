/**
 * The summaries window: meetings on the left, the selected one rendered right.
 *
 * Exists because the Summary button used to hand `summary.md` to whatever the
 * OS had registered — on Windows, Notepad showing raw `#` and `*`. For an app
 * whose pitch is that everything happens locally and needs nothing else,
 * sending people elsewhere to read its own output was the wrong shape.
 */

// First, so a failure in anything below is reported on screen rather than
// leaving a window that looks merely unfinished.
import "./errors.js";
import "./theme.js";
import * as api from "./api.js";
import { applyLanguage, loadCatalog, setLanguage, tr } from "./i18n.js";
import { initTooltips } from "./tooltip.js";
import { render } from "./markdown.js";
import { createEditor } from "./editor.js";
import { Autosave } from "./autosave.js";

const el = (id) => document.getElementById(id);

const ui = {
  list: el("library-list"),
  head: el("library-head"),
  title: el("doc-title"),
  meta: el("doc-meta"),
  body: el("doc-body"),
  empty: el("library-empty"),
  openTranscript: el("open-transcript"),
  openFolder: el("open-folder"),
  openFile: el("open-file"),
  readOnly: el("doc-readonly"),
  toggleEdit: el("toggle-edit"),
  editor: el("doc-editor"),
  editorText: el("editor-text"),
  editorStatus: el("editor-status"),
  undo: el("editor-undo"),
  redo: el("editor-redo"),
  conflict: el("editor-conflict"),
  reload: el("editor-reload"),
  keep: el("editor-keep"),
};

/** Whether the document pane shows the editor rather than the rendered page. */
let editing = false;
/**
 * The meeting whose source is in the editor. Saves go to this id, never to
 * `selectedId`: a save that lands after the selection moved must still write
 * to the document it came from.
 */
let editingId = null;

const editor = createEditor(ui.editorText, {
  onChange: (text) => autosave.changed(text),
  onHistory: (canUndo, canRedo) => {
    ui.undo.disabled = !canUndo;
    ui.redo.disabled = !canRedo;
  },
  linkPlaceholder: () => tr("editor_link_text"),
});

const autosave = new Autosave({
  save: (text, baseVersion) => api.saveSummary(editingId, text, baseVersion),
  onState: showSaveState,
});

/**
 * @param {import("./autosave.js").SaveState} state
 * @param {string} [detail]
 */
function showSaveState(state, detail) {
  ui.conflict.hidden = state !== "conflict";
  ui.editorStatus.classList.toggle("is-error", state === "error");
  ui.editorStatus.textContent =
    state === "saved" ? tr("editor_saved")
    : state === "pending" ? tr("editor_pending")
    : state === "saving" ? tr("editor_saving")
    : state === "error" ? tr("editor_save_failed", { error: detail ?? "" })
    : "";

  // The list's preview line is the summary's first prose line, which an edit
  // can change. Re-read it once the edit is on disk.
  if (state === "saved" && editingId) {
    api.listLibrary().then((latest) => {
      entries = latest;
      drawList();
    });
  }
}

/** @param {{kind?: string}|undefined} entry */
const isFile = (entry) => entry?.kind === "file";

/** The meeting on screen, so a refresh can keep it selected. */
let selectedId = null;
/** The last list drawn, so a refresh can tell whether anything changed. */
let entries = [];

/**
 * `2026-09-10_08-07-02` → `10 Sep 2026` and `08:07`.
 *
 * Parsed rather than formatted from a Date: the folder name is already local
 * time (that is the whole point of naming it in local time), and putting it
 * through `Date` would re-interpret it as UTC and shift it a second time.
 *
 * @param {string} id
 */
function whenOf(id) {
  const m = /^(\d{4})-(\d{2})-(\d{2})_(\d{2})-(\d{2})/.exec(id);
  if (!m) return { date: id, time: "" };

  const [, year, month, day, hour, minute] = m;
  const months = tr("months").split(",");
  const name = months[Number(month) - 1] ?? month;
  return { date: `${Number(day)} ${name} ${year}`, time: `${hour}:${minute}` };
}

/** Seconds as `1:49` or `1:02:30`. */
function formatDuration(seconds) {
  const total = Math.round(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

/**
 * One row.
 *
 * Shaped like a mail client's, and built only from what actually exists. The
 * obvious idea — use the summary's first heading as a title — was measured and
 * discarded: 11 of 14 first headings read "Meeting Minutes", which is the
 * summary *type*, not the meeting.
 *
 * An opened file has no date to show — its id is `file:N`, not a timestamp — so
 * it is labelled as a file and previewed like a summary.
 *
 * @param {{id: string, title: string, preview: string, duration_seconds: number|null,
 *          kind?: string, location?: string}} entry
 */
function row(entry) {
  const when = isFile(entry) ? { date: entry.title, time: tr("library_file") } : whenOf(entry.id);

  const button = document.createElement("button");
  button.className = isFile(entry) ? "library-row library-row--file" : "library-row";
  button.dataset.id = entry.id;

  const head = document.createElement("div");
  head.className = "library-row-head";

  const name = document.createElement("span");
  name.className = "library-row-title";
  // A title, when one exists, is the better identifier. On a real library not
  // one meeting had been given a name, so the date is the usual case.
  name.textContent = entry.title || when.date;

  const right = document.createElement("span");
  right.className = "library-row-when";
  right.textContent = entry.duration_seconds
    ? `${when.time} · ${formatDuration(entry.duration_seconds)}`
    : when.time;

  head.append(name, right);

  const preview = document.createElement("div");
  preview.className = "library-row-preview";
  preview.textContent = entry.preview;

  button.append(head, preview);
  button.addEventListener("click", () => select(entry.id));
  return button;
}

/** Draw the list, preserving the selection. */
function drawList() {
  ui.list.replaceChildren(...entries.map(row));
  markSelected();
}

function markSelected() {
  for (const node of ui.list.children) {
    node.classList.toggle("is-selected", node.dataset.id === selectedId);
  }
}

/**
 * Show one meeting.
 *
 * @param {string} id
 */
async function select(id) {
  // Unsaved edits are written before the selection moves. The one case where
  // that cannot happen is a conflict the user has not resolved: moving on would
  // quietly discard their version, so the selection stays and the choice is
  // put in front of them again.
  if (!(await leaveDocument())) {
    markSelected();
    return;
  }

  selectedId = id;
  markSelected();

  const entry = entries.find((e) => e.id === id);

  if (isFile(entry)) {
    // A file has no meeting behind it: its name is the title, its folder is
    // the only context there is, and there is no transcript to open.
    ui.title.textContent = entry.title;
    ui.meta.textContent = entry.location ?? "";
    ui.openTranscript.hidden = true;
    ui.openFolder.dataset.tooltip = "library_open_file_folder";
    // Only summaries are editable. A file opened from outside the app shows no
    // Edit button and no toolbar — nothing that suggests it could be changed —
    // and one quiet line saying so, so its absence is not a mystery.
    //
    // Arriving here in Edit mode drops back to Read: `leaveDocument` above has
    // already saved the summary that was being edited.
    if (editing) closeEditor();
    ui.toggleEdit.hidden = true;
    ui.readOnly.hidden = false;
  } else {
    const when = whenOf(id);
    ui.title.textContent = entry?.title || when.date;
    ui.meta.textContent = entry?.duration_seconds
      ? `${when.time} · ${formatDuration(entry.duration_seconds)}`
      : when.time;
    ui.openTranscript.hidden = false;
    ui.openFolder.dataset.tooltip = "library_open_folder";
    ui.toggleEdit.hidden = false;
    ui.readOnly.hidden = true;
  }
  ui.head.hidden = false;
  ui.empty.hidden = true;

  if (editing) {
    await loadSource(id);
    return;
  }

  let tokens;
  try {
    tokens = await api.readSummary(id);
  } catch (error) {
    // An opened file can be moved or deleted behind the app's back. Say so in
    // place rather than leaving the previous document on screen.
    if (selectedId !== id) return;
    ui.body.replaceChildren();
    ui.meta.textContent = String(error);
    return;
  }

  // Two clicks in quick succession can resolve out of order, and the header is
  // set synchronously above — without this, meeting A's body renders under
  // meeting B's title.
  if (selectedId !== id) return;

  // `onLink`: a plain <a> in a Tauri webview navigates the window, replacing
  // the app with the page and offering no way back. Rust opens it instead, and
  // re-checks the scheme there rather than trusting this call.
  render(ui.body, tokens, openLink);
  ui.body.scrollTop = 0;
}

/**
 * Save what is in the editor before showing something else.
 *
 * @returns {Promise<boolean>} false when an unresolved conflict means leaving
 *   would lose the user's text
 */
async function leaveDocument() {
  if (!editingId) return true;
  await autosave.flush();
  if (autosave.blocked && autosave.unsaved) {
    ui.conflict.hidden = false;
    ui.editorText.focus();
    return false;
  }
  editingId = null;
  return true;
}

/** Put a meeting's source in the editor, as a fresh document. */
async function loadSource(id) {
  const source = await api.readSummarySource(id);
  if (selectedId !== id) return;
  editingId = id;
  editor.load(source.text);
  autosave.reset(source.version);
  ui.editorText.focus();
}

/**
 * Switch between the rendered page and the editor.
 *
 * @param {boolean} on
 */
async function setEditing(on) {
  if (on === editing || !selectedId) return;
  // The button is hidden for opened files; this keeps any other route in too.
  if (on && isFile(entries.find((e) => e.id === selectedId))) return;

  if (on) {
    editing = true;
    ui.body.hidden = true;
    ui.editor.hidden = false;
    await loadSource(selectedId);
  } else {
    // Rendered from the editor's own text rather than re-read from disk, so
    // Read shows the document as it is this instant — including during a
    // conflict, when what is on disk is somebody else's version.
    const text = editor.text;
    if (!(await leaveDocument())) return;
    closeEditor();
    render(ui.body, await api.renderMarkdown(text), openLink);
    ui.body.scrollTop = 0;
  }

  syncToggle();
}

/**
 * Hide the editor and show the page, without rendering anything into it. The
 * caller has saved (see `leaveDocument`) and renders what comes next.
 */
function closeEditor() {
  editing = false;
  ui.editor.hidden = true;
  ui.body.hidden = false;
  syncToggle();
}

/** The Read/Edit button shows the mode it switches to. */
function syncToggle() {
  ui.toggleEdit.setAttribute("aria-pressed", String(editing));
  const label = editing ? "library_read" : "library_edit";
  ui.toggleEdit.dataset.tooltip = label;
  ui.toggleEdit.dataset.i18nLabel = label;
  ui.toggleEdit.setAttribute("aria-label", tr(label));
}

/** A link in a rendered document. See `select` for why it is not a plain <a>. */
function openLink(url) {
  api.openExternalUrl(url).catch((error) => {
    throw error;
  });
}

/** Re-read the list; keep the selection if it still exists. */
async function refresh() {
  entries = await api.listLibrary();

  const nothing = entries.length === 0;
  ui.empty.hidden = !nothing;
  ui.head.hidden = nothing || selectedId === null;
  if (nothing) ui.body.replaceChildren();

  drawList();

  // The selected meeting was deleted from disk while the window was open.
  if (selectedId && !entries.some((e) => e.id === selectedId)) {
    selectedId = null;
    editingId = null;
    editing = false;
    ui.head.hidden = true;
    ui.readOnly.hidden = true;
    closeEditor();
    ui.body.replaceChildren();
  }
}

async function main() {
  await loadCatalog();
  const config = await api.getConfig();
  setLanguage(String(config.language ?? "en"));
  applyLanguage();
  initTooltips(el("tooltip"));

  ui.openFile.addEventListener("click", async () => {
    try {
      const id = await api.pickMarkdownFile();
      if (!id) return; // cancelled
      await refresh();
      await select(id);
    } catch (error) {
      ui.empty.hidden = false;
      ui.empty.textContent = String(error);
    }
  });

  ui.toggleEdit.addEventListener("click", () => setEditing(!editing));

  // Toolbar buttons act on the text without taking focus from it: cancelling
  // mousedown keeps the textarea's selection, which is what the button needs.
  for (const button of ui.editor.querySelectorAll(".editor-tool")) {
    button.addEventListener("mousedown", (event) => event.preventDefault());
    button.addEventListener("click", () => editor.actions[button.dataset.action]?.());
  }

  // The file changed on disk. "Reload" is recorded as an edit, so Undo still
  // reaches the user's own version if they change their mind.
  ui.reload.addEventListener("click", async () => {
    if (!editingId) return;
    const source = await api.readSummarySource(editingId);
    editor.replace(source.text);
    autosave.reset(source.version);
  });
  ui.keep.addEventListener("click", () => autosave.overwrite());

  // Ctrl/Cmd+S out of habit. Autosave has usually done it already; this makes
  // "saved" true the instant it is pressed.
  document.addEventListener("keydown", (event) => {
    const modifier = /Mac/.test(navigator.platform) ? event.metaKey : event.ctrlKey;
    if (modifier && event.key.toLowerCase() === "s") {
      event.preventDefault();
      autosave.flush();
    }
  });

  // Last-chance saves. The window can be closed within the autosave delay of a
  // keystroke; losing focus is the earliest sign, and `pagehide` the last one.
  // The latter cannot await, but the command is sent before the page goes.
  window.addEventListener("blur", () => autosave.flush());
  window.addEventListener("pagehide", () => {
    if (editingId && autosave.unsaved && !autosave.blocked) {
      api.saveSummary(editingId, editor.text, autosave.version);
    }
  });

  ui.openFolder.addEventListener("click", async () => {
    if (!selectedId) return;
    const path = await api.libraryFolder(selectedId);
    api.openPath(path).catch((error) => {
      throw error;
    });
  });

  // A meeting recorded before transcripts were kept, or one whose run failed,
  // has no transcript — the command says so and the message reaches the user
  // rather than the console.
  ui.openTranscript.addEventListener("click", async () => {
    if (!selectedId) return;
    try {
      api.openPath(await api.libraryTranscript(selectedId));
    } catch (error) {
      ui.meta.textContent = String(error);
    }
  });

  await refresh();

  // A meeting finishing behind this window should appear in it. Reuses the
  // event the main window already listens to rather than adding a poll.
  api.on(api.EVENTS.queueChanged, refresh);

  // Settings can change the language while this window is open.
  api.on(api.EVENTS.configChanged, async () => {
    const latest = await api.getConfig();
    setLanguage(String(latest.language ?? "en"));
    applyLanguage();
    // The rows are built in JavaScript, so `applyLanguage` — which only touches
    // `[data-i18n]` elements — does not reach them.
    drawList();
  });

  // The Summary button was pressed while this window was already open. The
  // query string cannot carry that — the URL is fixed once the window exists.
  api.on(api.EVENTS.librarySelect, async (id) => {
    await refresh();
    if (entries.some((e) => e.id === id)) await select(id);
  });

  // Opened from the main window's Summary button, which passes the meeting.
  const wanted = new URLSearchParams(window.location.search).get("id");
  if (wanted && entries.some((e) => e.id === wanted)) {
    await select(wanted);
  } else if (entries.length > 0) {
    await select(entries[0].id);
  }
}

main().catch((error) => {
  // Surfaced by errors.js, on screen and in the terminal.
  throw error;
});
