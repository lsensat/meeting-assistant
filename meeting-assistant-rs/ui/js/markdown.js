/**
 * Build a document from Rust's Markdown token stream.
 *
 * # The one rule
 *
 * Element names come from `TAGS` below and nowhere else, and text is assigned
 * with `textContent`. There is no path in this file from document content to
 * markup: a `<script>` in a file becomes the nine literal characters
 * `<script>`, and a `<div class="panel">` cannot be produced at all because
 * `div` is not in the map.
 *
 * That is the point of the whole design. The app's CSP already blocks script
 * execution, so this is not about scripts — it is about limiting how far a
 * document can *impersonate the app*.
 *
 * Be precise about how far that goes. What is genuinely unreachable is anything
 * that **takes input**: no `input`, no `button`, no `form`, no `select`. A
 * document cannot construct something a user can type a password into.
 *
 * What remains reachable is *appearance*. `pre` and `blockquote` are styled
 * boxes, and a heading is a heading. A hostile file can look official. The
 * defences for that are elsewhere and deliberate: `.library-body` is visually
 * separated from the window's own chrome in `app.css`, and every link is handed
 * to `open_external_url`, which refuses every scheme but http, https and
 * mailto. Do not let this comment grow back into "impersonation is impossible".
 *
 * See `markdown.rs` for the other half.
 */

/**
 * Token tag → element. A tag absent from this map is skipped, so an unknown
 * token degrades to missing formatting rather than to unchecked markup.
 */
const TAGS = {
  paragraph: "p",
  heading: "h2", // overridden per level below
  list: "ul", // overridden when ordered
  item: "li",
  code: "pre",
  code_inline: "code",
  quote: "blockquote",
  emphasis: "em",
  strong: "strong",
  strike: "s",
  link: "a",
  rule: "hr",
  table: "table",
  row: "tr",
  cell: "td",
};

/** Elements that never contain anything, so their `End` closes nothing. */
const VOID = new Set(["rule"]);

/**
 * Render tokens into `root`, replacing whatever was there.
 *
 * @param {HTMLElement} root
 * @param {{t: string, tag?: string, level?: number, href?: string,
 *          ordered?: boolean, v?: string}[]} tokens
 * @param {(url: string) => void} onLink called instead of navigating
 */
export function render(root, tokens, onLink) {
  root.replaceChildren();

  // The element currently being filled. `root` is the floor: a malformed
  // stream that closes more than it opens cannot pop past it.
  const stack = [root];
  const top = () => stack[stack.length - 1];

  for (const token of tokens) {
    switch (token.t) {
      case "start": {
        const name = elementFor(token);
        if (!name) {
          // Unsupported tag. Push the current element again so the matching
          // `end` pops something harmless and the tree stays balanced.
          stack.push(top());
          break;
        }

        const el = document.createElement(name);

        if (token.tag === "link" && token.href) {
          // The href is set so the URL shows in the status bar and on hover,
          // but the click never navigates — see `onLink`.
          el.href = token.href;
          el.addEventListener("click", (event) => {
            event.preventDefault();
            onLink(token.href);
          });
        } else if (token.tag === "link") {
          // A destination Rust refused, or a relative one. The text stays;
          // there is simply nothing to click.
          el.classList.add("inert");
        }

        top().append(el);

        if (VOID.has(token.tag)) {
          // Nothing goes inside, but the stream still sends an `end`.
          stack.push(top());
        } else {
          stack.push(el);
        }
        break;
      }

      case "end":
        if (stack.length > 1) stack.pop();
        break;

      case "text":
        // The only place document content reaches the DOM, and it is text.
        top().append(document.createTextNode(token.v ?? ""));
        break;

      case "break":
        top().append(document.createElement("br"));
        break;

      default:
        // An unknown token type from a future Rust version. Ignored rather
        // than guessed at.
        break;
    }
  }
}

/**
 * The element name for a start token, or null to skip it.
 *
 * @param {{tag?: string, level?: number, ordered?: boolean}} token
 */
function elementFor(token) {
  if (token.tag === "heading") {
    // Clamped: a level outside 1-6 is not a heading element that exists.
    const level = Math.min(Math.max(Number(token.level) || 2, 1), 6);
    return `h${level}`;
  }
  if (token.tag === "list") return token.ordered ? "ol" : "ul";
  return Object.prototype.hasOwnProperty.call(TAGS, token.tag) ? TAGS[token.tag] : null;
}
