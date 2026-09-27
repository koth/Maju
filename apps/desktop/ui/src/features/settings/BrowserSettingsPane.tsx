import { useCallback, useEffect, useState } from "react";
import { onBrowserInstallProgress } from "../../lib/events";
import {
  browserInstall,
  browserPreflight,
  browserRefreshPreflight,
  settingsSaveBrowserSettings,
} from "../../lib/tauri";
import type {
  AgentSettingsSnapshot,
  BrowserInstallState,
  BrowserPreflight,
  BrowserSettings,
} from "../../types";
import "./BrowserSettingsPane.css";

export interface BrowserSettingsPaneProps {
  settings: BrowserSettings;
  /** Preflight carried by the settings snapshot, so the pane paints at once. */
  preflight: BrowserPreflight | null;
  /**
   * Receives the snapshot the save command returned, not the draft. The command
   * re-runs preflight server-side, so its answer is fresher than anything the
   * client could reconstruct.
   */
  onSaved: (snapshot: AgentSettingsSnapshot) => void;
}

type SaveState =
  | { kind: "idle" }
  | { kind: "saving" }
  | { kind: "saved" }
  | { kind: "error"; message: string };

/**
 * Whether the preflight result permits enabling the capability.
 *
 * A configuration that cannot run is shown with its remedy rather than being
 * silently accepted, because "enabled but the tools never appear" is the
 * failure mode this pane exists to prevent.
 */
export function preflightBlocksEnable(
  preflight: BrowserPreflight | null,
  settings: BrowserSettings,
): string | null {
  if (!preflight) return null;
  // Attach mode talks to a browser the user already runs, so it needs no
  // local install and preflight is not the gate there. Persistent mode does
  // need the local browser, so it stays gated.
  if (settings.mode === "attach") return null;
  if (preflight.state.state === "ready") return null;
  return preflight.state.remedy;
}

/**
 * Whether the pane should offer an install button.
 *
 * Only for a dependency the installer can supply. A missing Node runtime, or
 * a bad configuration, has a different remedy, and offering "Install" for
 * either would send the user down a path that cannot work.
 */
export function wantsInstallAction(
  preflight: BrowserPreflight | null,
  settings: BrowserSettings,
): boolean {
  if (!preflight) return false;
  if (settings.mode === "attach") return false;
  // Narrowed explicitly rather than by an early return, so a future variant
  // without a `fix` field is a type error instead of a silently missing button.
  const state = preflight.state;
  return state.state !== "ready" && state.fix === "install";
}

export function BrowserSettingsPane({
  settings,
  preflight,
  onSaved,
}: BrowserSettingsPaneProps) {
  const [draft, setDraft] = useState<BrowserSettings>(settings);
  const [state, setState] = useState<SaveState>({ kind: "idle" });
  const [live, setLive] = useState<BrowserPreflight | null>(preflight);
  const [install, setInstall] = useState<BrowserInstallState | null>(null);

  useEffect(() => setDraft(settings), [settings]);
  useEffect(() => setLive(preflight), [preflight]);

  // Progress is pushed rather than polled: an install takes tens of seconds,
  // and a pane that looks idle is worse than one naming the running step.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    onBrowserInstallProgress((next) => {
      if (!cancelled) setInstall(next);
    })
      .then((dispose) => {
        if (cancelled) dispose();
        else unlisten = dispose;
      })
      .catch(() => {
        /* Without the event channel the pane still shows the terminal state
         * returned by the command itself. */
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const update = useCallback((patch: Partial<BrowserSettings>) => {
    setDraft((current) => ({ ...current, ...patch }));
    setState({ kind: "idle" });
  }, []);

  const save = useCallback(async () => {
    setState({ kind: "saving" });
    try {
      onSaved(await settingsSaveBrowserSettings(draft));
      setState({ kind: "saved" });
    } catch (error) {
      setState({
        kind: "error",
        message: error instanceof Error ? error.message : String(error),
      });
    }
  }, [draft, onSaved]);

  const startInstall = useCallback(async () => {
    setInstall({ phase: { phase: "resolving" }, label: "Starting…", running: true, verified: false, installed: false });
    try {
      const result = await browserInstall();
      setInstall(result);
      // Re-check so the enable toggle becomes usable without reopening
      // settings — a stale "missing" would strand the user on a done install.
      setLive(await browserRefreshPreflight());
    } catch (error) {
      setInstall({
        phase: {
          phase: "failed",
          step: "Installing",
          detail: error instanceof Error ? error.message : String(error),
        },
        label: "安装失败。",
        running: false,
        verified: false,
        installed: false,
      });
    }
  }, []);

  const recheck = useCallback(async () => {
    try {
      setLive(await browserPreflight());
    } catch {
      setLive({
        state: {
          state: "missing",
          detail: "无法运行浏览器预检。",
          remedy: "请检查 Kodex 能否连接自己的后端。",
          // Kodex's own backend is unreachable, which an install cannot fix.
          fix: "configure",
        },
        provider_version: draft.provider_version,
      });
    }
  }, [draft.provider_version]);

  const blocker = preflightBlocksEnable(live, draft);
  const installable = wantsInstallAction(live, draft);
  const isAttach = draft.mode === "attach";
  const isPersistent = draft.mode === "persistent";
  const installing = install?.running ?? false;

  return (
    <section className="browser-settings" aria-label="Browser tools">
      <h3 className="browser-settings__heading">浏览器工具</h3>
      <p className="browser-settings__blurb">
        让 agent 驱动真实浏览器：导航页面、读取内容、点击操作。默认每个会话
        使用各自独立的浏览器。
      </p>

      <label className="browser-settings__row">
        <input
          checked={draft.enabled}
          onChange={(event) => update({ enabled: event.target.checked })}
          type="checkbox"
        />
        <span>启用浏览器工具</span>
      </label>

      <label className="browser-settings__row">
        <span>浏览器</span>
        <select
          onChange={(event) =>
            update({ mode: event.target.value === "attach" ? "attach" : "launch" })
          }
          value={draft.mode}
        >
          <option value="launch">每个会话开一个全新的浏览器</option>
          <option value="attach">接管我已经在运行的浏览器</option>
          <option value="persistent">跨会话保留登录状态</option>
        </select>
      </label>

      {isPersistent ? (
        <div className="browser-settings__profile">
          <label className="browser-settings__row">
            <span>配置档名</span>
            <input
              onChange={(event) => update({ profile_name: event.target.value })}
              placeholder="default"
              value={draft.profile_name}
            />
          </label>
          <p className="browser-settings__note">
            Kodex 自己拥有的浏览器配置档，位于独立数据目录。在这里完成的
            登录会在会话结束后保留，且绝不会碰到你真实的浏览器。同一配置档
            同时只能被一个会话使用。
          </p>
        </div>
      ) : null}

      {isAttach ? (
        <div className="browser-settings__attach">
          <label className="browser-settings__row">
            <input
              checked={draft.allow_attach}
              onChange={(event) => update({ allow_attach: event.target.checked })}
              type="checkbox"
            />
            <span>
              我理解这会让 agent 操作我已登录的浏览器
            </span>
          </label>
          <label className="browser-settings__row">
            <span>调试端点</span>
            <input
              onChange={(event) => update({ endpoint: event.target.value })}
              placeholder="http://127.0.0.1:9222"
              value={draft.endpoint}
            />
          </label>
        </div>
      ) : (
        <label className="browser-settings__row">
          <input
            checked={draft.headless}
            onChange={(event) => update({ headless: event.target.checked })}
            type="checkbox"
          />
          <span>不显示浏览器窗口</span>
        </label>
      )}

      <div className="browser-settings__preflight">
        <div className="browser-settings__preflight-head">
          <span>运行前提</span>
          <button onClick={() => void recheck()} type="button">
            重新检查
          </button>
        </div>
        {live?.state.state === "ready" ? (
          <p className="browser-settings__ok">
            已就绪{live.node_executable ? `（${live.node_executable}）` : ""}
          </p>
        ) : live ? (
          <div className="browser-settings__blocked">
            <p className="browser-settings__blocked-detail">{live.state.detail}</p>
            <p className="browser-settings__blocked-remedy">{live.state.remedy}</p>
          </div>
        ) : (
          <p className="browser-settings__pending">尚未检查。</p>
        )}
      </div>

      {blocker ? (
        <div className="browser-settings__blocker">
          <p className="browser-settings__warning">
            解决之前浏览器工具无法启动。
          </p>
          {installable ? (
            <button
              disabled={installing}
              onClick={() => void startInstall()}
              type="button"
            >
              {installing ? "安装中…" : "帮我安装"}
            </button>
          ) : null}
        </div>
      ) : null}

      {/* The install status lives outside the blocker, and renders whenever
          there is one. A successful install removes the blocker, so the
          confirmation has to outlive the problem it fixed; a failed one leaves
          the blocker standing, so its reason has to show alongside it. */}
      {install ? (
        <>
          <p
            className={
              install.verified
                ? "browser-settings__install-status browser-settings__install-status--ok"
                : "browser-settings__install-status"
            }
          >
            {install.label}
          </p>
          {/* The label names the step that failed and nothing else, so a
              failure without this is unactionable. It is the only place the
              installer's own words — npm's error, a spawn failure — reach the
              user, and reading them needs `phase` to be an object, which is
              the same nesting the preflight payload needed. */}
          {install.phase.phase === "failed" ? (
            <p className="browser-settings__install-detail">
              {install.phase.detail}
            </p>
          ) : null}
        </>
      ) : null}

      {state.kind === "error" ? (
        <p className="browser-settings__error">{state.message}</p>
      ) : null}
      {state.kind === "saved" ? (
        <p className="browser-settings__ok">已保存。</p>
      ) : null}

      <div className="browser-settings__actions">
        <button
          disabled={state.kind === "saving"}
          onClick={() => void save()}
          type="button"
        >
          {state.kind === "saving" ? "保存中…" : "保存"}
        </button>
      </div>
    </section>
  );
}
