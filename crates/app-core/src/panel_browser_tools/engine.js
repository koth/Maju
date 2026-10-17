// Kodex 面板浏览器引擎：注入右侧面板的每一个页面，把 agent 的浏览器工具
// （导航之外的全部动作）落到用户看得见的这个页面上。
//
// 外界（Rust 侧）通过 `window.__kodexBrowser.exec(request)` 调用；request 是
// 纯 JSON。返回值也必须是纯 JSON（会被序列化回 Rust）。抛出的错误由调用方
// 包装上报。
//
// 快照是 playwright-mcp 的 aria 快照的一个务实近似：一棵带 role/name/状态
// 的树，可操作元素带 ref；ref 只在最近一次快照内有效——页面一旦变化就
// 重新 snapshot，这正是 agent 使用 playwright 快照的方式。
(() => {
  if (window.__kodexBrowser) return;

  const MAX_NODES = 400;
  const MAX_NAME = 100;
  const MAX_VALUE = 60;
  const MAX_TEXT = 20000;
  // 遍历预算:快照只收 MAX_NODES 行,但走到它们之前经过的每个节点都有成本
  // (一次可见性判断就是一次样式解析;巨型页面上这段遍历本身足以让页面
  // 看起来「不响应」)。给树的深度和访问总量都加上界,快照的耗时因此是
  // 有界的,与页面大小无关。
  const MAX_DEPTH = 40;
  const MAX_VISITED = 6000;

  const refs = new Map();
  let refSeq = 0;

  // ---------- 元素分类 ----------

  const SEMANTIC_ROLES = new Set([
    "navigation", "main", "banner", "contentinfo", "complementary",
    "list", "listitem", "table", "row", "cell", "columnheader", "rowheader",
    "form", "dialog", "alert", "img", "figure", "heading", "group",
    "region", "search", "menu", "menubar", "toolbar", "tablist", "tabpanel",
  ]);

  const ACTIONABLE_ROLES = new Set([
    "link", "button", "textbox", "searchbox", "checkbox", "radio",
    "combobox", "listbox", "option", "menuitem", "menuitemcheckbox",
    "menuitemradio", "tab", "switch", "slider", "spinbutton",
  ]);

  function tagRole(el) {
    const tag = el.tagName.toLowerCase();
    switch (tag) {
      case "a":
      case "area":
        return el.hasAttribute("href") ? "link" : null;
      case "button":
        return "button";
      case "summary":
        return "button";
      case "input": {
        const type = (el.getAttribute("type") || "text").toLowerCase();
        if (type === "hidden") return null;
        if (["button", "submit", "reset", "image"].includes(type)) return "button";
        if (type === "checkbox") return "checkbox";
        if (type === "radio") return "radio";
        if (type === "range") return "slider";
        if (type === "search") return "searchbox";
        return "textbox";
      }
      case "textarea":
        return "textbox";
      case "select":
        return "combobox";
      case "option":
        return "option";
      case "img":
        return "img";
      case "h1": case "h2": case "h3": case "h4": case "h5": case "h6":
        return "heading";
      case "ul": case "ol":
        return "list";
      case "li":
        return "listitem";
      case "nav":
        return "navigation";
      case "main":
        return "main";
      case "header":
        return "banner";
      case "footer":
        return "contentinfo";
      case "aside":
        return "complementary";
      case "form":
        return "form";
      case "table":
        return "table";
      case "tr":
        return "row";
      case "td":
        return "cell";
      case "th":
        return "columnheader";
      case "dialog":
        return "dialog";
      case "label":
        return null; // 名字折叠进它标注的控件里
      default:
        return null;
    }
  }

  function roleOf(el) {
    const explicit = (el.getAttribute("role") || "").trim().toLowerCase();
    if (explicit && !explicit.includes(" ")) return explicit;
    return tagRole(el);
  }

  // ---------- 可见性与文本 ----------

  function isSkipped(el) {
    const tag = el.tagName.toLowerCase();
    if (["script", "style", "noscript", "template", "meta", "link", "title", "br", "hr", "svg", "path"].includes(tag)) {
      return true;
    }
    if (el.hidden) return true;
    let style;
    try {
      style = window.getComputedStyle(el);
    } catch {
      return false;
    }
    return style.display === "none" || style.visibility === "hidden";
  }

  function ownText(el) {
    let text = "";
    for (const node of el.childNodes) {
      if (node.nodeType === Node.TEXT_NODE) text += node.textContent;
    }
    return text.replace(/\s+/g, " ").trim();
  }

  function textOf(el) {
    return (el.textContent || "").replace(/\s+/g, " ").trim();
  }

  function cap(text, max) {
    if (!text) return "";
    const clean = String(text).replace(/\s+/g, " ").trim();
    return clean.length > max ? clean.slice(0, max - 1) + "…" : clean;
  }

  // ---------- 可访问名 ----------

  function labelTextFor(el) {
    if (el.id) {
      const label = el.ownerDocument.querySelector(`label[for="${CSS.escape(el.id)}"]`);
      if (label) return textOf(label);
    }
    const wrapping = el.closest("label");
    return wrapping ? textOf(wrapping) : "";
  }

  function nameOf(el, role) {
    const aria = (el.getAttribute("aria-label") || "").trim();
    if (aria) return cap(aria, MAX_NAME);
    const labelledby = (el.getAttribute("aria-labelledby") || "").trim();
    if (labelledby) {
      const text = labelledby
        .split(/\s+/)
        .map((id) => el.ownerDocument.getElementById(id))
        .filter(Boolean)
        .map(textOf)
        .join(" ");
      if (text.trim()) return cap(text, MAX_NAME);
    }
    switch (role) {
      case "img":
        return cap(el.getAttribute("alt") || el.getAttribute("title") || "", MAX_NAME);
      case "textbox":
      case "searchbox":
      case "combobox":
      case "slider":
      case "spinbutton":
        return cap(
          labelTextFor(el) || el.getAttribute("placeholder") || el.getAttribute("title") || el.getAttribute("name") || "",
          MAX_NAME,
        );
      case "button":
        return cap(ownText(el) || textOf(el) || el.getAttribute("value") || el.getAttribute("title") || "", MAX_NAME);
      case "link":
      case "heading":
      case "option":
      case "tab":
      case "menuitem":
      case "checkbox":
      case "radio":
      case "switch":
        return cap(ownText(el) || textOf(el) || labelTextFor(el) || el.getAttribute("title") || "", MAX_NAME);
      default:
        return cap(el.getAttribute("aria-label") || el.getAttribute("title") || "", MAX_NAME);
    }
  }

  // ---------- 状态标注 ----------

  function stateOf(el, role) {
    const states = [];
    const aria = (name) => el.getAttribute(name);
    if (el.disabled || aria("aria-disabled") === "true") states.push("disabled");
    if (role === "checkbox" || role === "radio" || role === "switch" || role === "menuitemcheckbox" || role === "menuitemradio") {
      const checked = el.checked === true || aria("aria-checked") === "true";
      if (checked) states.push("checked");
    }
    if (el.selected) states.push("selected");
    if (aria("aria-expanded") === "true") states.push("expanded");
    if (aria("aria-expanded") === "false") states.push("collapsed");
    if (aria("aria-pressed") === "true") states.push("pressed");
    if (el.required || aria("aria-required") === "true") states.push("required");
    if (el.readOnly || aria("aria-readonly") === "true") states.push("readonly");
    if (role === "heading") {
      const level = el.tagName.length === 2 && el.tagName[0].toLowerCase() === "h"
        ? el.tagName[1]
        : aria("aria-level") || "2";
      states.push(`level=${level}`);
    }
    if ((role === "textbox" || role === "searchbox" || role === "spinbutton") && "value" in el && el.value) {
      states.push(`value=${JSON.stringify(cap(el.value, MAX_VALUE))}`);
    }
    if (role === "slider" && "value" in el && el.value) {
      states.push(`value=${JSON.stringify(cap(el.value, MAX_VALUE))}`);
    }
    if (role === "combobox" && el.tagName.toLowerCase() === "select") {
      const selected = el.selectedOptions && el.selectedOptions[0];
      if (selected) states.push(`value=${JSON.stringify(cap(textOf(selected), MAX_VALUE))}`);
    }
    return states;
  }

  // ---------- 快照 ----------

  function isActionable(el, role) {
    if (role && ACTIONABLE_ROLES.has(role)) return true;
    if (el.hasAttribute("onclick")) return true;
    try {
      if (window.getComputedStyle(el).cursor === "pointer" && (ownText(el) || el.getAttribute("aria-label"))) {
        return true;
      }
    } catch {
      // getComputedStyle 在异常文档上可能失败：宁可漏，不可断。
    }
    return false;
  }

  function newRef(el) {
    const ref = `e${++refSeq}`;
    refs.set(ref, el);
    return ref;
  }

  function formatLine(depth, parts) {
    return `${"  ".repeat(depth)}- ${parts.filter(Boolean).join(" ")}`;
  }

  function describe(el, role, name, states, ref) {
    const parts = [role];
    if (name) parts.push(JSON.stringify(name));
    if (ref) parts.push(`[ref=${ref}]`);
    for (const state of states) parts.push(`[${state}]`);
    return parts.join(" ");
  }

  function walkChildren(node, depth, out, budget) {
    if (depth > MAX_DEPTH) return;
    for (const child of node.childNodes) {
      if (budget.count >= MAX_NODES || budget.visited >= MAX_VISITED) return;
      if (child.nodeType !== Node.ELEMENT_NODE) continue;
      budget.visited += 1;
      walk(child, depth, out, budget);
    }
  }

  function walk(el, depth, out, budget) {
    if (budget.count >= MAX_NODES || budget.visited >= MAX_VISITED) return;
    if (isSkipped(el)) return;

    const tag = el.tagName.toLowerCase();

    if (tag === "iframe" || tag === "frame") {
      budget.count += 1;
      let doc = null;
      try {
        doc = el.contentDocument;
      } catch {
        doc = null;
      }
      if (doc && doc.body) {
        out.push(formatLine(depth, ["iframe"]));
        walkChildren(doc.body, depth + 1, out, budget);
      } else {
        const name = cap(el.getAttribute("title") || el.getAttribute("name") || el.getAttribute("src") || "", MAX_NAME);
        out.push(formatLine(depth, ["iframe", name && JSON.stringify(name), "(跨域,无法读取)"].filter(Boolean).join(" ")));
      }
      return;
    }

    const role = roleOf(el);
    const text = ownText(el);

    if (role && (ACTIONABLE_ROLES.has(role) || SEMANTIC_ROLES.has(role) || isActionable(el, role))) {
      budget.count += 1;
      const effectiveRole = role || "button";
      const name = nameOf(el, effectiveRole);
      const states = stateOf(el, effectiveRole);
      const actionable = ACTIONABLE_ROLES.has(effectiveRole) || isActionable(el, role);
      const ref = actionable ? newRef(el) : null;
      const line = describe(el, effectiveRole, name, states, ref);
      out.push(formatLine(depth, [line]));
      if (el.shadowRoot) walkChildren(el.shadowRoot, depth + 1, out, budget);
      walkChildren(el, depth + 1, out, budget);
      return;
    }

    if (text) {
      budget.count += 1;
      out.push(formatLine(depth, [`text: ${JSON.stringify(cap(text, MAX_NAME))}`]));
      // 有正文的容器仍然继续向下走:正文里可能包着链接。
    }

    if (el.shadowRoot) walkChildren(el.shadowRoot, depth, out, budget);
    walkChildren(el, depth, out, budget);
  }

  function snapshot() {
    // 表清空但序号不重置:旧快照的 ref 在新快照里查无此元素,而不是恰好指
    // 到某个新元素上 —— agent 拿旧 ref 操作时,失败必须响亮,不能点错。
    refs.clear();
    const out = [];
    const budget = { count: 0, visited: 0 };
    if (document.body) walkChildren(document.body, 0, out, budget);
    const yaml = out.join("\n");
    return {
      url: location.href,
      title: document.title,
      yaml,
      truncated: budget.count >= MAX_NODES || budget.visited >= MAX_VISITED,
    };
  }

  // ---------- ref 解析 ----------

  function resolve(ref) {
    const el = refs.get(ref);
    if (!el || !el.isConnected) {
      throw new Error(`元素 ${ref} 不存在或已过期(页面已变化)。请重新调用 browser_snapshot 获取最新 ref。`);
    }
    return el;
  }

  // ---------- 事件合成 ----------

  function center(el) {
    try {
      if (el.scrollIntoView) el.scrollIntoView({ block: "center", inline: "center" });
    } catch {
      // 老页面没有平滑滚动参数:位置不影响事件本身。
    }
    const rect = el.getBoundingClientRect();
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  }

  function mouseEvent(type, point, extra) {
    const Ctor = type.startsWith("pointer") && typeof PointerEvent === "function" ? PointerEvent : MouseEvent;
    return new Ctor(type, {
      bubbles: true,
      cancelable: true,
      composed: true,
      clientX: point.x,
      clientY: point.y,
      button: 0,
      buttons: type.endsWith("down") || type.endsWith("move") ? 1 : 0,
      ...extra,
    });
  }

  function click(ref) {
    const el = resolve(ref);
    const point = center(el);
    for (const type of ["pointerover", "mouseover", "pointermove", "mousemove", "pointerdown", "mousedown"]) {
      el.dispatchEvent(mouseEvent(type, point));
    }
    try {
      el.focus({ preventScroll: true });
    } catch {
      // 不可聚焦的元素(如纯文本 div)没有 focus:直接跳过。
    }
    for (const type of ["pointerup", "mouseup"]) {
      el.dispatchEvent(mouseEvent(type, point));
    }
    el.click();
    return { clicked: describeTarget(el) };
  }

  function hover(ref) {
    const el = resolve(ref);
    const point = center(el);
    for (const type of ["pointerover", "mouseover", "pointermove", "mousemove"]) {
      el.dispatchEvent(mouseEvent(type, point));
    }
    return { hovered: describeTarget(el) };
  }

  function describeTarget(el) {
    const role = roleOf(el) || el.tagName.toLowerCase();
    const name = nameOf(el, role);
    return name ? `${role} "${cap(name, 60)}"` : role;
  }

  function setNativeValue(el, text) {
    const proto =
      el.tagName.toLowerCase() === "textarea"
        ? window.HTMLTextAreaElement && HTMLTextAreaElement.prototype
        : window.HTMLInputElement && HTMLInputElement.prototype;
    const descriptor = proto && Object.getOwnPropertyDescriptor(proto, "value");
    if (descriptor && descriptor.set) descriptor.set.call(el, text);
    else el.value = text;
  }

  function fireInput(el, data) {
    let event;
    try {
      event = new InputEvent("input", { bubbles: true, inputType: "insertText", data });
    } catch {
      event = new Event("input", { bubbles: true });
    }
    el.dispatchEvent(event);
    el.dispatchEvent(new Event("change", { bubbles: true }));
  }

  function type(ref, text, submit) {
    const el = resolve(ref);
    center(el);
    try {
      el.focus({ preventScroll: true });
    } catch {
      // 同上。
    }
    if (el.isContentEditable) {
      let inserted = false;
      try {
        document.execCommand("selectAll", false, null);
        inserted = document.execCommand("insertText", false, text);
      } catch {
        inserted = false;
      }
      if (!inserted) {
        el.textContent = text;
        fireInput(el, text);
      }
    } else if ("value" in el) {
      try {
        el.select();
      } catch {
        // number/color 等类型不支持 select:直接覆盖。
      }
      setNativeValue(el, text);
      fireInput(el, text);
    } else {
      throw new Error(`${describeTarget(el)} 不是可输入元素`);
    }
    if (submit) press("Enter");
    return { typed: text.length, into: describeTarget(el) };
  }

  const KEY_DEFS = {
    Enter: { key: "Enter", code: "Enter", keyCode: 13 },
    Tab: { key: "Tab", code: "Tab", keyCode: 9 },
    Escape: { key: "Escape", code: "Escape", keyCode: 27 },
    Backspace: { key: "Backspace", code: "Backspace", keyCode: 8 },
    Delete: { key: "Delete", code: "Delete", keyCode: 46 },
    ArrowUp: { key: "ArrowUp", code: "ArrowUp", keyCode: 38 },
    ArrowDown: { key: "ArrowDown", code: "ArrowDown", keyCode: 40 },
    ArrowLeft: { key: "ArrowLeft", code: "ArrowLeft", keyCode: 37 },
    ArrowRight: { key: "ArrowRight", code: "ArrowRight", keyCode: 39 },
    Home: { key: "Home", code: "Home", keyCode: 36 },
    End: { key: "End", code: "End", keyCode: 35 },
    PageUp: { key: "PageUp", code: "PageUp", keyCode: 33 },
    PageDown: { key: "PageDown", code: "PageDown", keyCode: 34 },
    " ": { key: " ", code: "Space", keyCode: 32 },
  };

  function keyDefFor(key) {
    if (KEY_DEFS[key]) return KEY_DEFS[key];
    if (key.length === 1) {
      const upper = key.toUpperCase();
      const isLetter = upper >= "A" && upper <= "Z";
      const isDigit = key >= "0" && key <= "9";
      return {
        key,
        code: isLetter ? `Key${upper}` : isDigit ? `Digit${key}` : key,
        keyCode: key.charCodeAt(0),
      };
    }
    return { key, code: key, keyCode: 0 };
  }

  function keyboardEvent(type, def, mods) {
    return new KeyboardEvent(type, {
      bubbles: true,
      cancelable: true,
      composed: true,
      key: def.key,
      code: def.code,
      keyCode: def.keyCode,
      which: def.keyCode,
      ctrlKey: mods.includes("Control"),
      altKey: mods.includes("Alt"),
      shiftKey: mods.includes("Shift"),
      metaKey: mods.includes("Meta"),
    });
  }

  function moveFocus(backward) {
    const tabbables = Array.from(
      document.querySelectorAll(
        'a[href], button, input:not([type="hidden"]), textarea, select, [tabindex]',
      ),
    ).filter((el) => !el.disabled && el.tabIndex >= 0 && !isSkipped(el));
    if (tabbables.length === 0) return;
    const current = tabbables.indexOf(document.activeElement);
    const next =
      current === -1
        ? backward
          ? tabbables[tabbables.length - 1]
          : tabbables[0]
        : tabbables[(current + (backward ? -1 : 1) + tabbables.length) % tabbables.length];
    next.focus();
  }

  function press(key) {
    const parts = String(key).split("+");
    const base = parts.pop();
    const mods = parts.map((part) => (part === "ControlOrMeta" ? "Meta" : part));
    const def = keyDefFor(base);
    const target = document.activeElement && document.activeElement !== document.body
      ? document.activeElement
      : document.body;
    const down = keyboardEvent("keydown", def, mods);
    target.dispatchEvent(down);
    if (def.key.length === 1) target.dispatchEvent(keyboardEvent("keypress", def, mods));
    target.dispatchEvent(keyboardEvent("keyup", def, mods));

    if (def.key === "Enter" && !down.defaultPrevented) {
      if (target.tagName === "A" && target.href) {
        target.click();
      } else {
        const form = target.form || (target.closest && target.closest("form"));
        if (form) {
          try {
            form.requestSubmit();
          } catch {
            const submitter = form.querySelector('[type="submit"]');
            if (submitter) submitter.click();
          }
        }
      }
    }
    if (def.key === "Tab") moveFocus(mods.includes("Shift"));
    if (def.key === " " && (target.tagName === "BUTTON" || (target.tagName === "INPUT" && ["checkbox", "radio"].includes(target.type)))) {
      target.click();
    }
    return { pressed: key };
  }

  function selectOption(ref, values) {
    const el = resolve(ref);
    if (el.tagName.toLowerCase() !== "select") {
      throw new Error(`${describeTarget(el)} 不是下拉选择框`);
    }
    const wanted = new Set((values || []).map(String));
    const chosen = [];
    for (const option of el.options) {
      const hit = wanted.has(option.value) || wanted.has(option.text) || wanted.has(option.label);
      option.selected = el.multiple ? hit : hit && chosen.length === 0;
      if (hit && (el.multiple || chosen.length === 0)) chosen.push(option.text);
    }
    if (chosen.length === 0) {
      throw new Error(`没有匹配的选项:${(values || []).join(", ")}`);
    }
    fireInput(el, null);
    return { selected: chosen };
  }

  // ---------- 求值与文本 ----------

  function stringifySafe(value) {
    if (value === undefined) return null;
    const seen = new WeakSet();
    try {
      const json = JSON.stringify(
        value,
        (key, item) => {
          if (typeof item === "object" && item !== null) {
            if (seen.has(item)) return "[Circular]";
            seen.add(item);
          }
          if (typeof item === "function") return `[Function ${item.name || "anonymous"}]`;
          if (typeof item === "undefined") return null;
          return item;
        },
        2,
      );
      return json.length > MAX_TEXT ? json.slice(0, MAX_TEXT) + "…(已截断)" : json;
    } catch {
      return String(value);
    }
  }

  async function evaluate(source) {
    if (typeof source !== "string" || !source.trim()) {
      throw new Error("browser_evaluate 需要一段 JavaScript 源码");
    }
    // 间接 eval,让源码运行在页面全局作用域里。
    const run = (0, eval)(`(async () => { const v = (${source}); return typeof v === "function" ? await v() : await v; })`);
    const value = await run();
    return stringifySafe(value);
  }

  function pageText() {
    const body = document.body;
    if (!body) return "";
    // innerText 天然跳过 script/style 和不可见内容;没有它(极少见)就退到
    // 可见元素下的文本节点。
    let text = typeof body.innerText === "string" ? body.innerText : "";
    if (!text.trim()) {
      const parts = [];
      const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT, {
        acceptNode(node) {
          const parent = node.parentElement;
          if (!parent) return NodeFilter.FILTER_REJECT;
          if (/^(script|style|noscript|template)$/i.test(parent.tagName)) {
            return NodeFilter.FILTER_REJECT;
          }
          return NodeFilter.FILTER_ACCEPT;
        },
      });
      let node;
      while ((node = walker.nextNode())) parts.push(node.textContent);
      text = parts.join(" ").replace(/\s+/g, " ").trim();
    }
    return text.length > MAX_TEXT ? text.slice(0, MAX_TEXT) : text;
  }

  // ---------- 入口 ----------

  window.__kodexBrowser = {
    async exec(request) {
      const req = request || {};
      switch (req.cmd) {
        case "ping":
          return { pong: true };
        case "snapshot":
          return snapshot();
        case "click":
          return click(req.ref);
        case "hover":
          return hover(req.ref);
        case "type":
          return type(req.ref, req.text == null ? "" : String(req.text), !!req.submit);
        case "press":
          return press(req.key);
        case "select":
          return selectOption(req.ref, req.values);
        case "evaluate":
          return { result: await evaluate(req.source) };
        case "text":
          return { text: pageText() };
        case "info":
          return { url: location.href, title: document.title };
        default:
          throw new Error(`未知命令:${req.cmd}`);
      }
    },
  };
})();
