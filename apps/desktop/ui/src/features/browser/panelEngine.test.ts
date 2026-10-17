/**
 * The panel browser engine (`crates/app-core/src/panel_browser_tools/engine.js`)
 * is what the agent's browser tools run inside the panel's pages. These tests
 * load the real file into jsdom and exercise it the way the MCP dispatch does:
 * snapshot a DOM, then act on the refs it produced.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeEach, describe, expect, it } from "vitest";

const ENGINE = readFileSync(
  resolve(process.cwd(), "../../../crates/app-core/src/panel_browser_tools/engine.js"),
  "utf8",
);

/** The wrapper the desktop shell puts around every page call
 *  (`apps/desktop/src-tauri/src/browser_panel/wrap_eval.js`). It is run for
 *  real here: a syntax error in it must fail this suite, not a user's tool
 *  call. */
const WRAP = readFileSync(
  resolve(process.cwd(), "../../../apps/desktop/src-tauri/src/browser_panel/wrap_eval.js"),
  "utf8",
);

interface BridgeCall {
  cmd: string;
  args: { callId: string; payload: unknown };
}

/** Build the script the app would inject, and stub the IPC bridge it answers
 *  over. Returns the calls the page made. */
function buildWrapped(callId: string, script: string): {
  source: string;
  calls: BridgeCall[];
} {
  const calls: BridgeCall[] = [];
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
    invoke: (cmd: string, args: { callId: string; payload: unknown }) => {
      calls.push({ cmd, args });
      return Promise.resolve();
    },
  };
  const source = WRAP.replace("__KODEX_CALL_ID__", JSON.stringify(callId)).replace(
    "__KODEX_SCRIPT__",
    script,
  );
  return { source, calls };
}

/** One task turn: enough for the wrapper's async body to answer. */
function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/** The script panel_browser_tools::exec builds for an engine request. */
function execScript(request: Record<string, unknown>): string {
  return `(function(){ ${ENGINE}\nreturn window.__kodexBrowser.exec(${JSON.stringify(request)}); })()`;
}

interface KodexEngine {
  exec(request: Record<string, unknown>): Promise<Record<string, unknown>>;
}

declare global {
  interface Window {
    __kodexBrowser?: KodexEngine;
  }
}

function install(): KodexEngine {
  new Function(ENGINE)();
  const engine = window.__kodexBrowser;
  if (!engine) throw new Error("engine did not install");
  return engine;
}

function exec(
  engine: KodexEngine,
  request: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  return engine.exec(request);
}

async function yaml(engine: KodexEngine): Promise<string> {
  const snap = await exec(engine, { cmd: "snapshot" });
  return String(snap.yaml);
}

beforeEach(() => {
  document.body.innerHTML = "";
  document.title = "测试页面";
});

describe("panel browser engine", () => {
  it("installs once and stays", () => {
    const first = install();
    const second = install();
    expect(second).toBe(first);
  });

  it("snapshots roles, names and refs of a page", async () => {
    const engine = install();
    document.body.innerHTML = `
      <h1>示例站点</h1>
      <p>一段正文</p>
      <a href="https://a.dev/docs">文档</a>
      <label for="q">搜索</label>
      <input id="q" type="text" placeholder="输入关键词" />
      <button id="go">提交</button>
      <button id="off" disabled>不可点</button>
    `;

    const out = await yaml(engine);

    expect(out).toContain('- heading "示例站点" [level=1]');
    expect(out).toContain('- text: "一段正文"');
    expect(out).toMatch(/- link "文档" \[ref=e\d+\]/);
    // 输入框的名字来自关联的 label,而不是 placeholder。
    expect(out).toMatch(/- textbox "搜索" \[ref=e\d+\]/);
    expect(out).toMatch(/- button "提交" \[ref=e\d+\]/);
    expect(out).toMatch(/- button "不可点" \[ref=e\d+\] \[disabled\]/);
  });

  it("skips hidden elements and script/style content", async () => {
    const engine = install();
    document.body.innerHTML = `
      <button id="shown">可见</button>
      <button id="none" style="display:none">不可见</button>
      <button id="attr" hidden>属性隐藏</button>
      <script>window.__secret = 1;</script>
      <style>.a{color:red}</style>
    `;

    const out = await yaml(engine);

    expect(out).toContain('button "可见"');
    expect(out).not.toContain("不可见");
    expect(out).not.toContain("属性隐藏");
    expect(out).not.toContain("__secret");
  });

  it("gives actionable divs a ref too (role=button)", async () => {
    const engine = install();
    document.body.innerHTML = `<div role="button" aria-label="播放视频"></div>`;

    const out = await yaml(engine);

    expect(out).toMatch(/- button "播放视频" \[ref=e\d+\]/);
  });

  it("marks checkboxes and select values", async () => {
    const engine = install();
    document.body.innerHTML = `
      <input type="checkbox" id="c" checked />
      <select id="s">
        <option value="a">甲</option>
        <option value="b" selected>乙</option>
      </select>
    `;

    const out = await yaml(engine);

    expect(out).toMatch(/- checkbox .* \[checked\]/);
    expect(out).toMatch(/- combobox .* \[value="乙"\]/);
  });

  it("click dispatches a mouse sequence and fires the handler", async () => {
    const engine = install();
    document.body.innerHTML = `<button id="go">提交</button>`;
    const events: string[] = [];
    const button = document.getElementById("go")!;
    for (const type of ["mousedown", "mouseup", "click"]) {
      button.addEventListener(type, () => events.push(type));
    }
    const snap = (await exec(engine, { cmd: "snapshot" })) as { yaml: string };
    const ref = snap.yaml.match(/button "提交" \[ref=(e\d+)\]/)![1];

    await exec(engine, { cmd: "click", ref });

    expect(events).toEqual(["mousedown", "mouseup", "click"]);
  });

  it("type fills through the native setter and fires input", async () => {
    const engine = install();
    document.body.innerHTML = `<input id="q" type="text" value="旧值" />`;
    const seen: string[] = [];
    const input = document.getElementById("q") as HTMLInputElement;
    input.addEventListener("input", () => seen.push(input.value));
    const snap = (await exec(engine, { cmd: "snapshot" })) as { yaml: string };
    const ref = snap.yaml.match(/textbox[^[]*\[ref=(e\d+)\]/)![1];

    await exec(engine, { cmd: "type", ref, text: "新值" });

    expect(input.value).toBe("新值");
    expect(seen).toEqual(["新值"]);
  });

  it("a stale ref fails and says to take a fresh snapshot", async () => {
    const engine = install();
    document.body.innerHTML = `<button id="go">提交</button>`;
    const snap = (await exec(engine, { cmd: "snapshot" })) as { yaml: string };
    const ref = snap.yaml.match(/\[ref=(e\d+)\]/)![1];
    document.getElementById("go")!.remove();

    await expect(exec(engine, { cmd: "click", ref })).rejects.toThrow(
      /已过期|不存在/,
    );
  });

  it("refs from an older snapshot never resolve to a new element", async () => {
    const engine = install();
    document.body.innerHTML = `<button id="a">甲</button>`;
    const first = (await exec(engine, { cmd: "snapshot" })) as { yaml: string };
    const ref = first.yaml.match(/\[ref=(e\d+)\]/)![1];
    document.body.innerHTML = `<button id="b">乙</button><button id="c">丙</button>`;
    const second = await yaml(engine);

    // 序号单调递增:新快照里的 ref 全部排在旧 ref 之后;旧 ref 查无此
    // 元素,而不是恰好点到「乙」上。
    const oldNumber = Number(ref.slice(1));
    for (const [, next] of second.matchAll(/\[ref=e(\d+)\]/g)) {
      expect(Number(next)).toBeGreaterThan(oldNumber);
    }
    await expect(exec(engine, { cmd: "click", ref })).rejects.toThrow(/已过期|不存在/);
  });

  it("select picks options by value or text", async () => {
    const engine = install();
    document.body.innerHTML = `
      <select id="s">
        <option value="a">甲</option>
        <option value="b">乙</option>
      </select>
    `;
    const select = document.getElementById("s") as HTMLSelectElement;
    const snap = (await exec(engine, { cmd: "snapshot" })) as { yaml: string };
    const ref = snap.yaml.match(/combobox[^[]*\[ref=(e\d+)\]/)![1];

    const outcome = (await exec(engine, { cmd: "select", ref, values: ["b"] })) as {
      selected: string[];
    };

    expect(select.value).toBe("b");
    expect(outcome.selected).toEqual(["乙"]);
  });

  it("press dispatches key events to the focused element", async () => {
    const engine = install();
    document.body.innerHTML = `<input id="q" />`;
    const input = document.getElementById("q") as HTMLInputElement;
    input.focus();
    const keys: string[] = [];
    input.addEventListener("keydown", (event) => keys.push((event as KeyboardEvent).key));

    await exec(engine, { cmd: "press", key: "a" });
    await exec(engine, { cmd: "press", key: "Enter" });

    expect(keys).toEqual(["a", "Enter"]);
  });

  it("Tab moves focus to the next field", async () => {
    const engine = install();
    document.body.innerHTML = `<input id="a" /><input id="b" />`;
    const first = document.getElementById("a") as HTMLInputElement;
    first.focus();

    await exec(engine, { cmd: "press", key: "Tab" });

    expect(document.activeElement).toBe(document.getElementById("b"));
  });

  it("evaluate runs page JavaScript and returns JSON", async () => {
    const engine = install();
    document.title = "求值页面";

    const outcome = (await exec(engine, {
      cmd: "evaluate",
      source: "({title: document.title, count: document.querySelectorAll('div').length})",
    })) as { result: string };

    expect(JSON.parse(outcome.result)).toEqual({ title: "求值页面", count: 0 });
  });

  it("evaluate reports page errors instead of dying", async () => {
    const engine = install();

    await expect(
      exec(engine, { cmd: "evaluate", source: "nonexistent_thing()" }),
    ).rejects.toThrow();
  });

  it("text returns the visible page text for wait_for", async () => {
    const engine = install();
    document.body.innerHTML = `<p>加载完成,共 42 条结果</p><script>var x = 1;</script>`;

    const outcome = (await exec(engine, { cmd: "text" })) as { text: string };

    expect(outcome.text).toContain("加载完成,共 42 条结果");
    expect(outcome.text).not.toContain("var x");
  });

  it("bounds the walk on huge pages and says so", async () => {
    const engine = install();
    // 远超遍历上限的节点数:快照必须按时返回并标记截断,而不是把页面的主
    // 线程占满(页面看起来「不响应」的主要来源)。
    const many = Array.from({ length: 8000 }, (_, i) => `<div><span>项 ${i}</span></div>`).join("");
    document.body.innerHTML = many;

    const snap = (await exec(engine, { cmd: "snapshot" })) as {
      yaml: string;
      truncated: boolean;
    };

    expect(snap.truncated).toBe(true);
    expect(snap.yaml.split("\n").length).toBeLessThanOrEqual(400);
  });

  it("bounds the walk on pathologically deep pages", async () => {
    const engine = install();
    let html = "叶子";
    for (let i = 0; i < 200; i += 1) html = `<div>${html}</div>`;
    document.body.innerHTML = html;

    // 深度有上界:不爆栈,正常返回。
    const snap = (await exec(engine, { cmd: "snapshot" })) as { yaml: string };
    expect(typeof snap.yaml).toBe("string");
  });
});

describe("eval wrapper (the app's page-call envelope)", () => {
  it("parses, awaits the script and reports ok with the value", async () => {
    const { source, calls } = buildWrapped("c1", "Promise.resolve(42)");

    new Function(source)();
    await tick();

    expect(calls).toEqual([
      {
        cmd: "browser_panel_eval_result",
        args: { callId: "c1", payload: { ok: true, value: 42 } },
      },
    ]);
  });

  it("reports page errors as ok:false with the message", async () => {
    const { source, calls } = buildWrapped("c2", "Promise.reject(new Error(\"炸了\"))");

    new Function(source)();
    await tick();

    expect(calls).toEqual([
      {
        cmd: "browser_panel_eval_result",
        args: { callId: "c2", payload: { ok: false, error: "炸了" } },
      },
    ]);
  });

  it("drives the engine through the wrapper the way the app does", async () => {
    // The full chain: wrapper → engine install guard → exec → snapshot answer.
    document.body.innerHTML = `<button id="go">提交</button>`;
    const { source, calls } = buildWrapped("c3", execScript({ cmd: "snapshot" }));

    new Function(source)();
    await tick();

    expect(calls).toHaveLength(1);
    expect(calls[0].args.callId).toBe("c3");
    const payload = calls[0].args.payload as {
      ok: boolean;
      value: { url: string; yaml: string; truncated: boolean };
    };
    expect(payload.ok).toBe(true);
    expect(payload.value.yaml).toMatch(/- button "提交" \[ref=e\d+\]/);
  });

  it("engine errors travel back through the wrapper as ok:false", async () => {
    document.body.innerHTML = `<button id="go">提交</button>`;
    const { source, calls } = buildWrapped(
      "c4",
      execScript({ cmd: "click", ref: "e999" }),
    );

    new Function(source)();
    await tick();

    const payload = calls[0].args.payload as { ok: boolean; error: string };
    expect(payload.ok).toBe(false);
    expect(payload.error).toMatch(/e999/);
  });
});
