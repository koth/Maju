// Inline-math typography for the phone.
//
// React Native has no inline-block: a `Svg`/`WebView`/custom native view cannot
// sit inside a `<Text>` run, only a nested `<Text>` or an `<Image>` can. So an
// inline formula cannot be handed to KaTeX the way a display formula is. This
// module is the fallback the phone uses instead: a LaTeX → Unicode pass that
// keeps the formula *inline and readable* — `$H_0$` → H₀, `$O(n)$` → O(n),
// `$\hat{p} = 0.65$` → p̂ = 0.65 — with variables set in italic and numbers and
// operators upright, the way TeX would.
//
// It is intentionally lossy. Anything it cannot express degrades to a readable
// form (`\frac{a}{b}` → a/b, an unknown macro keeps its name) rather than
// disappearing, and display formulas never come here at all — those are
// rendered by KaTeX in MathFormula.tsx.
//
// Pure string work, so it is covered by tests instead of a device.

export interface InlineMathRun {
  text: string;
  italic: boolean;
}

/// Greek letters, operators and relations. `italic: false` — these glyphs are
/// already the mathematical form in a normal font, so slanting them again just
/// looks broken.
const SYMBOLS: Record<string, string> = {
  alpha: "α", beta: "β", gamma: "γ", delta: "δ", epsilon: "ε", varepsilon: "ε",
  zeta: "ζ", eta: "η", theta: "θ", vartheta: "ϑ", iota: "ι", kappa: "κ",
  lambda: "λ", mu: "μ", nu: "ν", xi: "ξ", pi: "π", varpi: "ϖ", rho: "ρ",
  sigma: "σ", varsigma: "ς", tau: "τ", upsilon: "υ", phi: "φ", varphi: "φ",
  chi: "χ", psi: "ψ", omega: "ω",
  Gamma: "Γ", Delta: "Δ", Theta: "Θ", Lambda: "Λ", Xi: "Ξ", Pi: "Π",
  Sigma: "Σ", Upsilon: "Υ", Phi: "Φ", Psi: "Ψ", Omega: "Ω",
  times: "×", cdot: "·", div: "÷", pm: "±", mp: "∓", ast: "∗", star: "⋆",
  le: "≤", leq: "≤", ge: "≥", geq: "≥", ne: "≠", neq: "≠", approx: "≈",
  equiv: "≡", propto: "∝", sim: "∼", simeq: "≃", cong: "≅", ll: "≪", gg: "≫",
  in: "∈", notin: "∉", ni: "∋", subset: "⊂", subseteq: "⊆", supset: "⊃",
  supseteq: "⊇", cup: "∪", cap: "∩", setminus: "∖", emptyset: "∅",
  forall: "∀", exists: "∃", nexists: "∄", neg: "¬", land: "∧", lor: "∨",
  to: "→", rightarrow: "→", leftarrow: "←", leftrightarrow: "↔", implies: "⟹",
  Rightarrow: "⇒", Leftarrow: "⇐", Leftrightarrow: "⇔", mapsto: "↦",
  infty: "∞", partial: "∂", nabla: "∇", sum: "∑", prod: "∏", coprod: "∐",
  int: "∫", iint: "∬", oint: "∮", sqrt: "√", angle: "∠", perp: "⊥",
  parallel: "∥", therefore: "∴", because: "∵", degree: "°",
  ldots: "…", cdots: "⋯", dots: "…", vdots: "⋮", ddots: "⋱",
  quad: "  ", qquad: "    ", ",": " ", ";": " ", ":": " ", "!": "", " ": " ",
  "{": "{", "}": "}", _: "_", "#": "#", "&": "&", "%": "%", $: "$",
  // Layout-only macros with no ink of their own.
  left: "", right: "", big: "", Big: "", bigg: "", Bigg: "",
  displaystyle: "", textstyle: "", scriptstyle: "", limits: "", nolimits: "",
  mathbb: "", mathcal: "", mathfrak: "", mathsf: "", mathtt: "", mathnormal: "",
  bmod: "mod", pmod: "mod", operatorname: "", text: "", textrm: "",
};

/// Values that can be raised or lowered. Anything missing from these tables
/// degrades to a caret/underscore form, which still reads correctly.
const SUPERSCRIPTS: Record<string, string> = {
  "0": "⁰", "1": "¹", "2": "²", "3": "³", "4": "⁴", "5": "⁵", "6": "⁶",
  "7": "⁷", "8": "⁸", "9": "⁹", "+": "⁺", "-": "⁻", "−": "⁻", "=": "⁼",
  "(": "⁽", ")": "⁾", n: "ⁿ", i: "ⁱ", a: "ᵃ", b: "ᵇ", c: "ᶜ", d: "ᵈ",
  e: "ᵉ", k: "ᵏ", m: "ᵐ", o: "ᵒ", p: "ᵖ", t: "ᵗ", x: "ˣ", T: "ᵀ",
};

const SUBSCRIPTS: Record<string, string> = {
  "0": "₀", "1": "₁", "2": "₂", "3": "₃", "4": "₄", "5": "₅", "6": "₆",
  "7": "₇", "8": "₈", "9": "₉", "+": "₊", "-": "₋", "−": "₋", "=": "₌",
  "(": "₍", ")": "₎", a: "ₐ", e: "ₑ", h: "ₕ", i: "ᵢ", j: "ⱼ", k: "ₖ",
  l: "ₗ", m: "ₘ", n: "ₙ", o: "ₒ", p: "ₚ", r: "ᵣ", s: "ₛ", t: "ₜ", u: "ᵤ",
  v: "ᵥ", x: "ₓ",
};

/// Combining marks applied to the preceding character.
const ACCENTS: Record<string, string> = {
  hat: "\u0302", widehat: "\u0302", bar: "\u0304", overline: "\u0304",
  tilde: "\u0303", widetilde: "\u0303", vec: "\u20D7", dot: "\u0307",
  ddot: "\u0308", check: "\u030C", breve: "\u0306",
};

/// Macros that take one brace group and print it in an upright face.
const UPRIGHT_ONE_ARG = new Set(["text", "textrm", "mathrm", "operatorname", "mathbf", "mathsf", "mathtt"]);

/// Binary operators and relations. TeX spaces these by *class*, not by what the
/// source happened to contain, so `a+b` and `a + b` both read as "a + b" here.
const SPACED = new Set([
  "+", "-", "±", "∓", "×", "·", "÷", "∗", "⋆", "∘", "∧", "∨", "∪", "∩", "∖",
  "=", "≠", "≤", "≥", "≈", "≡", "∼", "≃", "≅", "∝", "≪", "≫",
  "∈", "∉", "∋", "⊂", "⊆", "⊃", "⊇", "→", "←", "↔", "⇒", "⇐", "⇔", "↦",
  "∥", "⊥", "∣", "<", ">", "∴", "∵",
]);

interface Piece {
  text: string;
  italic: boolean;
}

/**
 * Rendering runs for one inline formula. Adjacent characters with the same
 * slant are merged so the renderer emits as few nested `<Text>` nodes as
 * possible.
 */
export function inlineMathRuns(tex: string): InlineMathRun[] {
  const runs = render(tex, false);
  // A trailing space inside a group is meaningful (`\text{其中 }`), but the
  // formula as a whole is set inline and must not push the text after it away.
  const first = runs[0];
  if (first) first.text = first.text.replace(/^ +/, "");
  const last = runs[runs.length - 1];
  if (last) last.text = last.text.replace(/ +$/, "");
  return runs.filter((run) => run.text.length > 0);
}

/** Convenience for tests and for callers that only want the plain text. */
export function inlineMathText(tex: string): string {
  return inlineMathRuns(tex)
    .map((run) => run.text)
    .join("");
}

/**
 * @param tight script/radical content: TeX sets those with no operator
 *   spacing and no inter-atom spaces, so `e^{-\lambda t}` is `e^(-λt)` and not
 *   `e^(- λ t)`.
 */
function render(tex: string, tight: boolean): InlineMathRun[] {
  const pieces: Piece[] = [];
  let pendingSpace = false;
  let eatNextSpace = false;

  const endsWithSpace = () => {
    const last = pieces[pieces.length - 1];
    return last === undefined ? true : last.text.endsWith(" ");
  };
  /// Append to the last run when the slant matches, so a formula produces as
  /// few nested `<Text>` nodes as possible.
  const append = (text: string, italic: boolean) => {
    const last = pieces[pieces.length - 1];
    if (last && last.italic === italic) last.text += text;
    else pieces.push({ text, italic });
  };
  const push = (text: string, italic = false) => {
    if (text.length === 0) return;
    if (pendingSpace) {
      pendingSpace = false;
      if (pieces.length > 0 && !endsWithSpace()) append(" ", false);
    }
    append(text, italic);
  };
  /// An operator: spaced on both sides, whatever the source contained.
  const pushOperator = (glyph: string) => {
    if (tight) {
      push(glyph);
      return;
    }
    pendingSpace = pieces.length > 0;
    push(glyph);
    pendingSpace = true;
  };
  const pushAtom = (text: string, italic = false) => {
    if (SPACED.has(text) && text.length >= 1) pushOperator(text);
    else push(text, italic);
  };
  const pushScript = (text: string, table: Record<string, string>, marker: string) => {
    // Map the whole script at once: glyph availability is a property of the
    // script as a whole, not of each italic/upright run inside it.
    const flat = render(text, true)
      .map((run) => run.text)
      .join("");
    push(toScript(flat, table, marker));
  };

  let index = 0;
  while (index < tex.length) {
    const eatSpace = eatNextSpace;
    eatNextSpace = false;
    const char = tex[index];

    if (char === "\\") {
      const command = readCommand(tex, index);
      index = command.next;
      const name = command.name;
      if (name.length === 0) continue;
      // TeX eats the single space that terminates a control WORD, so
      // `\nabla f` is "∇f" and `a \quad b` keeps only the quad's own width.
      eatNextSpace = command.isWord;

      if (name in ACCENTS) {
        const [argument, next] = readArgument(tex, index);
        index = next;
        pushArgument(argument, ACCENTS[name]);
        continue;
      }

      if (name === "frac" || name === "dfrac" || name === "tfrac") {
        const [numerator, afterNumerator] = readArgument(tex, index);
        const [denominator, afterDenominator] = readArgument(tex, afterNumerator);
        index = afterDenominator;
        pushArgument(numerator);
        push("/");
        pushArgument(denominator);
        continue;
      }

      if (name === "binom" || name === "dbinom" || name === "tbinom") {
        const [top, afterTop] = readArgument(tex, index);
        const [bottom, afterBottom] = readArgument(tex, afterTop);
        index = afterBottom;
        push("C(");
        pushArgument(top);
        push(", ");
        pushArgument(bottom);
        push(")");
        continue;
      }

      if (name === "sqrt") {
        const [degree, afterDegree] = readOptionalArgument(tex, index);
        const [radicand, afterRadicand] = readArgument(tex, afterDegree);
        index = afterRadicand;
        if (degree.length > 0) pushScript(degree, SUPERSCRIPTS, "^");
        push("√");
        pushArgument(radicand);
        continue;
      }

      if (UPRIGHT_ONE_ARG.has(name)) {
        const [argument, next] = readArgument(tex, index);
        index = next;
        // Text arguments keep their spaces (`\text{其中 }` is a real space).
        for (const piece of render(argument, false)) {
          push(piece.text, false);
        }
        continue;
      }

      if (name in SYMBOLS) {
        pushAtom(SYMBOLS[name]);
        continue;
      }

      // Unknown macro: keep its name so the reader is not left with a gap.
      push(name);
      continue;
    }

    if (char === "^" || char === "_") {
      index += 1;
      const [argument, next] = readArgument(tex, index);
      index = next;
      pushScript(argument, char === "^" ? SUPERSCRIPTS : SUBSCRIPTS, char === "^" ? "^" : "_");
      continue;
    }

    if (char === "{") {
      const close = matchingBrace(tex, index);
      if (close < 0) {
        index += 1;
        continue;
      }
      pushArgument(tex.slice(index + 1, close));
      index = close + 1;
      continue;
    }

    if (char === "}") {
      index += 1;
      continue;
    }

    if (char === "~" || char === "&") {
      push(" ");
      index += 1;
      continue;
    }

    if (char === "%") {
      // TeX comment: drop the rest of the line.
      const newline = tex.indexOf("\n", index);
      index = newline < 0 ? tex.length : newline + 1;
      continue;
    }

    if (char === " " || char === "\n" || char === "\t" || char === "\r") {
      // The space that terminates a control word is a delimiter, not a space.
      if (!eatSpace && !tight) pendingSpace = true;
      index += 1;
      continue;
    }

    if (char === "," || char === ";") {
      push(char);
      pendingSpace = !tight;
      index += 1;
      continue;
    }

    // Ordinary character: variables are italic, everything else upright.
    pushAtom(char, /[A-Za-z]/.test(char));
    index += 1;
  }

  // Preserve a trailing space so a group's own spacing survives; the public
  // entry point trims it back off at the formula's edges.
  if (pendingSpace) append(" ", false);
  return pieces;

  /// Group content of a macro argument (fraction, radical, accent base).
  function pushArgument(argument: string, suffix = "") {
    for (const piece of render(argument, false)) push(piece.text, piece.italic);
    if (suffix.length > 0) {
      const last = pieces[pieces.length - 1];
      if (last) last.text += suffix;
    }
  }
}

function readCommand(tex: string, start: number): { name: string; next: number; isWord: boolean } {
  // `start` points at the backslash.
  let index = start + 1;
  const single = tex[index];
  if (single === undefined) return { name: "", next: index, isWord: false };
  if (!/[A-Za-z]/.test(single)) {
    // Escaped punctuation (`\{`, `\,`, `\%`, …) — the name IS the character.
    return { name: single, next: index + 1, isWord: false };
  }
  let name = "";
  while (index < tex.length && /[A-Za-z]/.test(tex[index])) {
    name += tex[index];
    index += 1;
  }
  return { name, next: index, isWord: true };
}

/** One argument: a `{…}` group or the single token after the macro. */
function readArgument(tex: string, start: number): [string, number] {
  let index = start;
  while (tex[index] === " ") index += 1;
  if (tex[index] === "{") {
    const close = matchingBrace(tex, index);
    if (close < 0) return [tex.slice(index + 1), tex.length];
    return [tex.slice(index + 1, close), close + 1];
  }
  if (tex[index] === undefined) return ["", index];
  if (tex[index] === "\\") {
    // A macro used as a single-token argument (`\sqrt\alpha`) keeps its slash
    // so the recursive render resolves it.
    const command = readCommand(tex, index);
    return [tex.slice(index, command.next), command.next];
  }
  return [tex[index], index + 1];
}

/** Optional `[…]` argument (`\sqrt[3]{x}`). */
function readOptionalArgument(tex: string, start: number): [string, number] {
  let index = start;
  while (tex[index] === " ") index += 1;
  if (tex[index] !== "[") return ["", start];
  const close = tex.indexOf("]", index + 1);
  if (close < 0) return ["", start];
  return [tex.slice(index + 1, close), close + 1];
}

function matchingBrace(tex: string, open: number): number {
  let depth = 0;
  for (let index = open; index < tex.length; index += 1) {
    if (tex[index] === "\\") {
      index += 1;
      continue;
    }
    if (tex[index] === "{") depth += 1;
    else if (tex[index] === "}") {
      depth -= 1;
      if (depth === 0) return index;
    }
  }
  return -1;
}

/** Raised/lowered text, or a caret fallback when a glyph is missing. */
function toScript(text: string, table: Record<string, string>, marker: string): string {
  const mapped = [...text].map((char) => table[char]);
  if (mapped.every((char) => char !== undefined)) return mapped.join("");
  return `${marker}${text.length === 1 ? text : `(${text})`}`;
}
// end of file
