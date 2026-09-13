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
import * as api from "./api.js";
import { applyLanguage, loadCatalog, setLanguage, tr } from "./i18n.js";
import { initTooltips } from "./tooltip.js";
import { render } from "./markdown.js";

const el = (id) => document.getElementById(id);

const ui = {
  list: el("library-list"),
  head: el("library-head"),
  title: el("doc-title"),
  meta: el("doc-meta"),
  body: el("doc-body"),
  empty: el("library-empty"),
  openFolder: el("open-folder"),
};

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
 * @param {{id: string, title: string, preview: string, duration_seconds: number|null}} entry
 */
function row(entry) {
  const when = whenOf(entry.id);

  const button = document.createElement("button");
  button.className = "library-row";
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
  selectedId = id;
  markSelected();

  const entry = entries.find((e) => e.id === id);
  const when = whenOf(id);

  ui.title.textContent = entry?.title || when.date;
  ui.meta.textContent = entry?.duration_seconds
    ? `${when.time} · ${formatDuration(entry.duration_seconds)}`
    : when.time;
  ui.head.hidden = false;
  ui.empty.hidden = true;

  const tokens = await api.readSummary(id);

  // Two clicks in quick succession can resolve out of order, and the header is
  // set synchronously above — without this, meeting A's body renders under
  // meeting B's title.
  if (selectedId !== id) return;

  // `onLink`: a plain <a> in a Tauri webview navigates the window, replacing
  // the app with the page and offering no way back. Rust opens it instead, and
  // re-checks the scheme there rather than trusting this call.
  render(ui.body, tokens, (url) => {
    api.openExternalUrl(url).catch((error) => {
      throw error;
    });
  });
  ui.body.scrollTop = 0;
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
    ui.head.hidden = true;
    ui.body.replaceChildren();
  }
}

async function main() {
  await loadCatalog();
  const config = await api.getConfig();
  setLanguage(String(config.language ?? "en"));
  applyLanguage();
  initTooltips(el("tooltip"));

  ui.openFolder.addEventListener("click", async () => {
    if (!selectedId) return;
    const path = await api.libraryFolder(selectedId);
    api.openPath(path).catch((error) => {
      throw error;
    });
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
