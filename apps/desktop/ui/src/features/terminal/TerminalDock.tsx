import { memo, useCallback, useEffect, useRef, useState } from "react";
import type {
  ClipboardEvent as ReactClipboardEvent,
  PointerEvent as ReactPointerEvent,
} from "react";
import { FitAddon, Terminal, init } from "ghostty-web";
import {
  terminalList,
  terminalOpen,
  terminalResize,
  terminalScrollback,
  terminalTerminate,
  terminalWrite,
} from "../../lib/tauri";
import { onTerminalExit, onTerminalOutput, onTerminalStatus } from "../../lib/events";
import type { AppTheme, TerminalSession, TerminalSessionStatus } from "../../types";
import "./TerminalDock.css";

type GhosttyTerminal = InstanceType<typeof Terminal>;
type GhosttyFitAddon = InstanceType<typeof FitAddon>;

interface TerminalSurface {
  terminal: GhosttyTerminal;
  fitAddon: GhosttyFitAddon;
  disposables: Array<{ dispose: () => void }>;
  writeQueue: string[];
  writeFrame: number | null;
  writeGeneration: number;
  hydrated: boolean;
}

interface ResizeState {
  last: { cols: number; rows: number } | null;
  inFlight: boolean;
  queued: { cols: number; rows: number } | null;
}

const DEFAULT_COLS = 100;
const DEFAULT_ROWS = 24;
const MIN_HEIGHT = 140;
const MAX_VIEWPORT_RATIO = 0.66;
const OUTPUT_BUFFER_LIMIT = 500_000;
const TERMINAL_WRITE_CHUNK_SIZE = 8_192;
const TERMINAL_WRITE_CHUNKS_PER_FRAME = 8;

let ghosttyInitPromise: Promise<void> | null = null;

function ensureGhosttyLoaded(): Promise<void> {
  if (!ghosttyInitPromise) {
    ghosttyInitPromise = init();
  }
  return ghosttyInitPromise;
}

function terminalPalette(appTheme: AppTheme) {
  if (appTheme === "light") {
    return {
      background: "#ffffff",
      foreground: "#2f353a",
      cursor: "#2f353a",
      selection: "#d7e2ef",
      black: "#2f353a",
      red: "#b84a3d",
      green: "#367a48",
      yellow: "#8a6b1f",
      blue: "#2b6fb0",
      magenta: "#7b56a6",
      cyan: "#317b7d",
      white: "#f2f3f3",
      brightBlack: "#78838c",
      brightRed: "#d24d3e",
      brightGreen: "#2f8b4d",
      brightYellow: "#a98122",
      brightBlue: "#1f78c8",
      brightMagenta: "#8c5ac2",
      brightCyan: "#258d91",
      brightWhite: "#ffffff",
    };
  }

  return {
    background: "#151616",
    foreground: "#d3d9dd",
    cursor: "#d3d9dd",
    selection: "#34424b",
    black: "#0c0d0f",
    red: "#f07568",
    green: "#5cc887",
    yellow: "#d6b765",
    blue: "#7fb4f0",
    magenta: "#c69bf5",
    cyan: "#6ec9d2",
    white: "#d3d9dd",
    brightBlack: "#7c8790",
    brightRed: "#ff887a",
    brightGreen: "#6ee69d",
    brightYellow: "#efd27a",
    brightBlue: "#9cccff",
    brightMagenta: "#d8b1ff",
    brightCyan: "#83e4ec",
    brightWhite: "#ffffff",
  };
}

function clampHeight(value: number) {
  const max = Math.max(MIN_HEIGHT, Math.floor(window.innerHeight * MAX_VIEWPORT_RATIO));
  return Math.min(max, Math.max(MIN_HEIGHT, Math.round(value)));
}

function normalizeWorkspaceRoot(value: string) {
  return value.replace(/\\/g, "/").toLowerCase();
}

function sameWorkspaceRoot(a: string, b: string) {
  return normalizeWorkspaceRoot(a) === normalizeWorkspaceRoot(b);
}

function trimTerminalBuffer(value: string) {
  if (value.length <= OUTPUT_BUFFER_LIMIT) return value;
  return value.slice(value.length - OUTPUT_BUFFER_LIMIT);
}

function terminalTabLabel(session: TerminalSession | null) {
  if (!session) return "终端";
  return session.cwd ? `${session.shell}  ${session.cwd}` : session.shell;
}

function terminalStatusLabel(status: TerminalSessionStatus) {
  return status === "running" ? "运行中" : "已退出";
}

function cancelSurfaceWrites(surface: TerminalSurface) {
  surface.writeGeneration += 1;
  surface.writeQueue = [];
  if (surface.writeFrame != null) {
    window.cancelAnimationFrame(surface.writeFrame);
    surface.writeFrame = null;
  }
}

interface TerminalDockProps {
  workspaceRoot: string;
  appTheme: AppTheme;
  visible: boolean;
  height: number;
  layoutSignal: string;
  onHeightChange: (height: number) => void;
  onHide: () => void;
}

export function TerminalDock({
  workspaceRoot,
  appTheme,
  visible,
  height,
  layoutSignal,
  onHeightChange,
  onHide,
}: TerminalDockProps) {
  const viewportNodesRef = useRef<Map<string, HTMLDivElement>>(new Map());
  const surfacesRef = useRef<Map<string, TerminalSurface>>(new Map());
  const sessionsRef = useRef<TerminalSession[]>([]);
  const activeTerminalIdRef = useRef<string | null>(null);
  const sessionRef = useRef<TerminalSession | null>(null);
  const outputBuffersRef = useRef<Map<string, string>>(new Map());
  const resizeFramesRef = useRef<Map<string, number>>(new Map());
  const resizeStatesRef = useRef<Map<string, ResizeState>>(new Map());
  const visibleRef = useRef(visible);
  const [sessions, setSessions] = useState<TerminalSession[]>([]);
  const [activeTerminalId, setActiveTerminalId] = useState<string | null>(null);
  const [status, setStatus] = useState<TerminalSessionStatus>("running");
  const [opening, setOpening] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ghosttyReady, setGhosttyReady] = useState(false);
  const [eventListenersReady, setEventListenersReady] = useState(false);
  const [terminating, setTerminating] = useState(false);
  const session = activeTerminalId
    ? sessions.find((item) => item.terminal_id === activeTerminalId) ?? null
    : null;

  const disposeSurface = useCallback((terminalId: string) => {
    const surface = surfacesRef.current.get(terminalId);
    if (!surface) return;
    cancelSurfaceWrites(surface);
    surface.disposables.forEach((disposable) => disposable.dispose());
    surface.fitAddon.dispose?.();
    surface.terminal.dispose();
    surfacesRef.current.delete(terminalId);

    const resizeFrame = resizeFramesRef.current.get(terminalId);
    if (resizeFrame != null) {
      window.cancelAnimationFrame(resizeFrame);
      resizeFramesRef.current.delete(terminalId);
    }
    resizeStatesRef.current.delete(terminalId);
  }, []);

  const disposeAllSurfaces = useCallback(() => {
    Array.from(surfacesRef.current.keys()).forEach((terminalId) => {
      disposeSurface(terminalId);
    });
  }, [disposeSurface]);

  const enqueueSurfaceWrite = useCallback((terminalId: string, data: string, force = false) => {
    if (!data) return;
    const surface = surfacesRef.current.get(terminalId);
    if (!surface) return;
    if (!surface.hydrated && !force) return;

    surface.writeQueue.push(data);
    if (surface.writeFrame != null) return;

    const generation = surface.writeGeneration;
    const drain = () => {
      surface.writeFrame = null;
      if (generation !== surface.writeGeneration) return;

      let written = 0;
      while (
        surface.writeQueue.length > 0 &&
        written < TERMINAL_WRITE_CHUNKS_PER_FRAME
      ) {
        const next = surface.writeQueue[0] ?? "";
        const chunk = next.slice(0, TERMINAL_WRITE_CHUNK_SIZE);
        try {
          surface.terminal.write(chunk);
        } catch (err) {
          surface.writeQueue = [];
          setError(String(err));
          return;
        }
        if (next.length <= TERMINAL_WRITE_CHUNK_SIZE) {
          surface.writeQueue.shift();
        } else {
          surface.writeQueue[0] = next.slice(TERMINAL_WRITE_CHUNK_SIZE);
        }
        written += 1;
      }

      if (surface.writeQueue.length > 0) {
        surface.writeFrame = window.requestAnimationFrame(drain);
      }
    };
    surface.writeFrame = window.requestAnimationFrame(drain);
  }, []);

  const sendResize = useCallback((terminalId: string, cols: number, rows: number) => {
    const target = sessionsRef.current.find((item) => item.terminal_id === terminalId);
    if (!target || target.status !== "running") return;

    const next = {
      cols: Math.max(2, Math.round(cols)),
      rows: Math.max(1, Math.round(rows)),
    };
    const state =
      resizeStatesRef.current.get(terminalId) ??
      { last: null, inFlight: false, queued: null };
    resizeStatesRef.current.set(terminalId, state);

    if (state.last && state.last.cols === next.cols && state.last.rows === next.rows) {
      return;
    }

    if (state.inFlight) {
      state.queued = next;
      return;
    }

    state.inFlight = true;
    state.last = next;
    void terminalResize({
      terminal_id: terminalId,
      cols: next.cols,
      rows: next.rows,
    })
      .catch(() => {})
      .finally(() => {
        state.inFlight = false;
        const queued = state.queued;
        state.queued = null;
        if (queued) {
          sendResize(terminalId, queued.cols, queued.rows);
        }
      });
  }, []);

  const fitSurface = useCallback(
    (terminalId: string) => {
      if (!visibleRef.current) return;
      const previous = resizeFramesRef.current.get(terminalId);
      if (previous != null) {
        window.cancelAnimationFrame(previous);
      }
      const frame = window.requestAnimationFrame(() => {
        resizeFramesRef.current.delete(terminalId);
        const surface = surfacesRef.current.get(terminalId);
        if (!surface) return;
        try {
          surface.fitAddon.fit();
          const dims = surface.fitAddon.proposeDimensions?.();
          sendResize(
            terminalId,
            dims?.cols ?? surface.terminal.cols ?? DEFAULT_COLS,
            dims?.rows ?? surface.terminal.rows ?? DEFAULT_ROWS,
          );
        } catch {
          // The dock can briefly be zero-sized while showing or resizing.
        }
      });
      resizeFramesRef.current.set(terminalId, frame);
    },
    [sendResize],
  );

  const focusSurface = useCallback((terminalId: string | null) => {
    if (!terminalId) return;
    const terminal = surfacesRef.current.get(terminalId)?.terminal;
    terminal?.focus();
    terminal?.textarea?.focus();
  }, []);

  const focusTerminal = useCallback(() => {
    focusSurface(activeTerminalIdRef.current);
  }, [focusSurface]);

  const fitTerminal = useCallback(() => {
    const terminalId = activeTerminalIdRef.current;
    if (terminalId) {
      fitSurface(terminalId);
    }
  }, [fitSurface]);

  const activeDimensions = useCallback(() => {
    const terminalId = activeTerminalIdRef.current;
    const surface = terminalId ? surfacesRef.current.get(terminalId) : null;
    const dims = surface?.fitAddon.proposeDimensions?.();
    return {
      cols: dims?.cols ?? surface?.terminal.cols ?? DEFAULT_COLS,
      rows: dims?.rows ?? surface?.terminal.rows ?? DEFAULT_ROWS,
    };
  }, []);

  const createSurface = useCallback(
    (terminalId: string, node: HTMLDivElement) => {
      if (!ghosttyReady || !visibleRef.current || surfacesRef.current.has(terminalId)) {
        return;
      }
      node.replaceChildren();

      const terminal = new Terminal({
        cursorBlink: true,
        fontFamily: "ui-monospace, SFMono-Regular, Consolas, Liberation Mono, monospace",
        fontSize: 13,
        scrollback: 8000,
        theme: terminalPalette(appTheme),
      });
      const fitAddon = new FitAddon();
      terminal.loadAddon(fitAddon);
      terminal.open(node);
      fitAddon.observeResize?.();

      const disposables = [
        terminal.onData((data) => {
          const target = sessionsRef.current.find((item) => item.terminal_id === terminalId);
          if (!target || target.status !== "running") return;
          void terminalWrite({
            terminal_id: terminalId,
            data,
          }).catch((err) => setError(String(err)));
        }),
        terminal.onResize(({ cols, rows }) => {
          sendResize(terminalId, cols, rows);
        }),
      ];

      const surface: TerminalSurface = {
        terminal,
        fitAddon,
        disposables,
        writeQueue: [],
        writeFrame: null,
        writeGeneration: 0,
        hydrated: false,
      };
      surfacesRef.current.set(terminalId, surface);
      window.requestAnimationFrame(() => {
        try {
          surface.fitAddon.fit();
          const dims = surface.fitAddon.proposeDimensions?.();
          sendResize(
            terminalId,
            dims?.cols ?? surface.terminal.cols ?? DEFAULT_COLS,
            dims?.rows ?? surface.terminal.rows ?? DEFAULT_ROWS,
          );
        } catch {
          // The dock can briefly be zero-sized while showing or resizing.
        }
        surface.hydrated = true;
        enqueueSurfaceWrite(terminalId, outputBuffersRef.current.get(terminalId) ?? "", true);
        if (activeTerminalIdRef.current === terminalId) {
          focusSurface(terminalId);
        }
      });
    },
    [appTheme, enqueueSurfaceWrite, focusSurface, ghosttyReady, sendResize],
  );

  const handleViewportNodeChange = useCallback(
    (terminalId: string, node: HTMLDivElement | null) => {
      if (node) {
        viewportNodesRef.current.set(terminalId, node);
        return;
      }

      viewportNodesRef.current.delete(terminalId);
      disposeSurface(terminalId);
    },
    [disposeSurface],
  );

  useEffect(() => {
    sessionsRef.current = sessions;
  }, [sessions]);

  useEffect(() => {
    activeTerminalIdRef.current = activeTerminalId;
    const active = activeTerminalId
      ? sessionsRef.current.find((item) => item.terminal_id === activeTerminalId) ?? null
      : null;
    sessionRef.current = active;
    if (active) {
      setStatus(active.status);
      window.requestAnimationFrame(() => {
        fitSurface(active.terminal_id);
        focusSurface(active.terminal_id);
      });
    }
  }, [activeTerminalId, fitSurface, focusSurface, sessions]);

  useEffect(() => {
    if (activeTerminalId && sessions.some((item) => item.terminal_id === activeTerminalId)) {
      return;
    }
    setActiveTerminalId(sessions[0]?.terminal_id ?? null);
  }, [activeTerminalId, sessions]);

  useEffect(() => {
    disposeAllSurfaces();
    outputBuffersRef.current.clear();
    viewportNodesRef.current.clear();
    sessionsRef.current = [];
    activeTerminalIdRef.current = null;
    sessionRef.current = null;
    setSessions([]);
    setActiveTerminalId(null);
  }, [disposeAllSurfaces, workspaceRoot]);

  const upsertSession = useCallback((next: TerminalSession, activate = true) => {
    outputBuffersRef.current.set(
      next.terminal_id,
      outputBuffersRef.current.get(next.terminal_id) ?? "",
    );
    setSessions((prev) => {
      const existingIndex = prev.findIndex((item) => item.terminal_id === next.terminal_id);
      if (existingIndex === -1) return [...prev, next];
      const copy = [...prev];
      copy[existingIndex] = next;
      return copy;
    });
    if (activate) {
      setActiveTerminalId(next.terminal_id);
    }
  }, []);

  const hydrateScrollback = useCallback(
    async (terminalId: string) => {
      try {
        const response = await terminalScrollback({ terminal_id: terminalId });
        const current = outputBuffersRef.current.get(terminalId) ?? "";
        if (!response.data || current.length >= response.data.length) {
          return;
        }
        const delta = response.data.startsWith(current)
          ? response.data.slice(current.length)
          : current.length === 0
            ? response.data
            : "";
        outputBuffersRef.current.set(terminalId, trimTerminalBuffer(response.data));
        if (delta) {
          enqueueSurfaceWrite(terminalId, delta);
        }
      } catch {
        // The terminal may have exited or been closed while the dock was restoring.
      }
    },
    [enqueueSurfaceWrite],
  );

  useEffect(() => {
    sessionRef.current = session;
    if (session) {
      setStatus(session.status);
    }
  }, [session]);

  useEffect(() => {
    visibleRef.current = visible;
  }, [visible]);

  useEffect(() => {
    let disposed = false;
    setGhosttyReady(false);
    disposeAllSurfaces();
    ensureGhosttyLoaded()
      .then(() => {
        if (!disposed) {
          setGhosttyReady(true);
        }
      })
      .catch((err) => {
        if (!disposed) setError(String(err));
      });

    return () => {
      disposed = true;
      setGhosttyReady(false);
      disposeAllSurfaces();
    };
  }, [appTheme, disposeAllSurfaces]);

  useEffect(() => {
    if (!visible || !ghosttyReady) return;
    const activeIds = new Set(sessions.map((item) => item.terminal_id));
    Array.from(surfacesRef.current.keys()).forEach((terminalId) => {
      if (!activeIds.has(terminalId)) {
        disposeSurface(terminalId);
      }
    });
    sessions.forEach((item) => {
      const node = viewportNodesRef.current.get(item.terminal_id);
      if (node) {
        createSurface(item.terminal_id, node);
      }
    });
  }, [createSurface, disposeSurface, ghosttyReady, sessions, visible]);

  const openSession = useCallback(async () => {
    const current = sessionRef.current;
    if (
      current?.status === "running" &&
      sameWorkspaceRoot(current.workspace_root, workspaceRoot)
    ) {
      window.requestAnimationFrame(() => {
        fitTerminal();
        focusTerminal();
      });
      return;
    }
    setOpening(true);
    setError(null);
    try {
      const dims = activeDimensions();
      const existing = (await terminalList(workspaceRoot)).filter(
        (item) => item.status === "running",
      );
      if (existing.length > 0) {
        await Promise.all(existing.map((item) => hydrateScrollback(item.terminal_id)));
        existing.forEach((item) => {
          outputBuffersRef.current.set(
            item.terminal_id,
            outputBuffersRef.current.get(item.terminal_id) ?? "",
          );
        });
        setSessions(existing);
        setActiveTerminalId((prev) =>
          prev && existing.some((item) => item.terminal_id === prev)
            ? prev
            : existing[0]?.terminal_id ?? null,
        );
        setTerminating(false);
        window.requestAnimationFrame(() => {
          fitTerminal();
          focusTerminal();
        });
        return;
      }
      const next = await terminalOpen({
        workspace_root: workspaceRoot,
        cols: dims.cols,
        rows: dims.rows,
      });
      upsertSession(next);
      setTerminating(false);
      window.requestAnimationFrame(() => {
        fitTerminal();
        focusTerminal();
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setOpening(false);
    }
  }, [
    activeDimensions,
    fitTerminal,
    focusTerminal,
    hydrateScrollback,
    upsertSession,
    workspaceRoot,
  ]);

  useEffect(() => {
    let disposed = false;
    const cleanups: Array<() => void> = [];
    setEventListenersReady(false);

    const outputCleanup = onTerminalOutput((event) => {
      if (!sameWorkspaceRoot(event.workspace_root, workspaceRoot)) return;
      const previousOutput = outputBuffersRef.current.get(event.terminal_id) ?? "";
      outputBuffersRef.current.set(
        event.terminal_id,
        trimTerminalBuffer(previousOutput + event.data),
      );
      enqueueSurfaceWrite(event.terminal_id, event.data);
    });

    const statusCleanup = onTerminalStatus((event) => {
      if (!sameWorkspaceRoot(event.workspace_root, workspaceRoot)) return;
      if (activeTerminalIdRef.current === event.terminal_id) {
        setTerminating(false);
        setStatus(event.status);
      }
      setSessions((prev) =>
        prev.map((item) =>
          item.terminal_id === event.terminal_id
            ? {
                ...item,
                status: event.status,
                cwd: event.cwd,
                shell: event.shell,
                exit_code: event.exit_code,
              }
            : item,
        ),
      );
    });

    const exitCleanup = onTerminalExit((event) => {
      if (!sameWorkspaceRoot(event.workspace_root, workspaceRoot)) return;
      if (activeTerminalIdRef.current === event.terminal_id) {
        setTerminating(false);
        setStatus("exited");
      }
      setSessions((prev) =>
        prev.map((item) =>
          item.terminal_id === event.terminal_id
            ? { ...item, status: "exited", exit_code: event.exit_code }
            : item,
        ),
      );
    });

    Promise.all([outputCleanup, statusCleanup, exitCleanup])
      .then((registeredCleanups) => {
        if (disposed) {
          registeredCleanups.forEach((cleanup) => cleanup());
          return;
        }
        cleanups.push(...registeredCleanups);
        setEventListenersReady(true);
      })
      .catch((err) => {
        if (!disposed) setError(String(err));
      });

    return () => {
      disposed = true;
      cleanups.forEach((cleanup) => cleanup());
      setEventListenersReady(false);
    };
  }, [enqueueSurfaceWrite, workspaceRoot]);

  useEffect(() => {
    if (!visible || !ghosttyReady || !eventListenersReady) return;
    void openSession();
  }, [eventListenersReady, ghosttyReady, openSession, visible]);

  useEffect(() => {
    if (!visible) return;
    window.requestAnimationFrame(() => {
      fitTerminal();
      focusTerminal();
    });
  }, [fitTerminal, focusTerminal, height, layoutSignal, visible]);

  const handleResizeStart = useCallback(
    (event: ReactPointerEvent<HTMLButtonElement>) => {
      event.preventDefault();
      const pointerId = event.pointerId;
      event.currentTarget.setPointerCapture(pointerId);
      const startY = event.clientY;
      const startHeight = height;
      document.body.classList.add("is-resizing-terminal-dock");

      const handlePointerMove = (moveEvent: PointerEvent) => {
        onHeightChange(clampHeight(startHeight - (moveEvent.clientY - startY)));
      };
      const handlePointerUp = () => {
        document.body.classList.remove("is-resizing-terminal-dock");
        window.removeEventListener("pointermove", handlePointerMove);
        window.removeEventListener("pointerup", handlePointerUp);
        window.removeEventListener("pointercancel", handlePointerUp);
        window.requestAnimationFrame(fitTerminal);
      };

      window.addEventListener("pointermove", handlePointerMove);
      window.addEventListener("pointerup", handlePointerUp);
      window.addEventListener("pointercancel", handlePointerUp);
    },
    [fitTerminal, height, onHeightChange],
  );

  const handleNewTerminal = useCallback(async () => {
    setError(null);
    setOpening(true);
    try {
      const dims = activeDimensions();
      const next = await terminalOpen({
        workspace_root: workspaceRoot,
        force_new: true,
        cols: dims.cols,
        rows: dims.rows,
      });
      outputBuffersRef.current.set(next.terminal_id, "");
      upsertSession(next);
      setTerminating(false);
      window.requestAnimationFrame(() => {
        fitSurface(next.terminal_id);
        focusSurface(next.terminal_id);
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setOpening(false);
    }
  }, [activeDimensions, fitSurface, focusSurface, upsertSession, workspaceRoot]);

  const removeSessionFromDock = useCallback(
    (terminalId: string) => {
      const currentSessions = sessionsRef.current;
      const removeIndex = currentSessions.findIndex((item) => item.terminal_id === terminalId);
      const nextSessions = currentSessions.filter((item) => item.terminal_id !== terminalId);
      const wasActive = activeTerminalIdRef.current === terminalId;

      outputBuffersRef.current.delete(terminalId);
      disposeSurface(terminalId);
      if (nextSessions.length === 0) {
        outputBuffersRef.current.clear();
        sessionsRef.current = [];
        activeTerminalIdRef.current = null;
        sessionRef.current = null;
        setSessions([]);
        setActiveTerminalId(null);
        setTerminating(false);
        setStatus("running");
        onHide();
        return;
      }

      setSessions(nextSessions);

      if (wasActive) {
        const nextActive =
          nextSessions[removeIndex]?.terminal_id ??
          nextSessions[Math.max(0, removeIndex - 1)]?.terminal_id ??
          null;
        setActiveTerminalId(nextActive);
        setTerminating(false);
        if (!nextActive) {
          sessionRef.current = null;
        }
      }
    },
    [disposeSurface, onHide],
  );

  const handleCloseTerminal = useCallback(
    (terminalId: string) => {
      const target = sessionsRef.current.find((item) => item.terminal_id === terminalId);
      if (!target) return;

      setError(null);
      removeSessionFromDock(terminalId);
      void terminalTerminate({ terminal_id: terminalId }).catch((err) => {
        const message = String(err);
        if (!message.toLowerCase().includes("terminal not found")) {
          setError(message);
        }
      });
    },
    [removeSessionFromDock],
  );

  const handleHide = useCallback(() => {
    setError(null);
    onHide();
  }, [onHide]);

  const handlePaste = useCallback((event: ReactClipboardEvent<HTMLDivElement>) => {
    const text = event.clipboardData.getData("text");
    if (!text) return;
    event.preventDefault();
    const terminalId = activeTerminalIdRef.current;
    if (terminalId) {
      surfacesRef.current.get(terminalId)?.terminal.paste(text);
    }
  }, []);

  const activeStatus = session?.status ?? status;
  const statusLabel = opening ? "启动中" : terminating ? "停止中" : terminalStatusLabel(activeStatus);
  const visibleTabs = sessions.length > 0 ? sessions : [];

  return (
    <section
      className={`terminal-dock ${visible ? "is-visible" : "is-hidden"}`}
      style={{ height: visible ? clampHeight(height) : 0 }}
      aria-label="终端"
      aria-hidden={!visible}
    >
      <button
        type="button"
        className="terminal-dock-resizer"
        aria-label="调整终端高度"
        title="拖拽调整终端高度"
        onPointerDown={handleResizeStart}
      />
      <header className="terminal-dock-header">
        <div className="terminal-tab-strip">
          {visibleTabs.length === 0 ? (
            <button
              type="button"
              className="terminal-tab is-active"
              title={statusLabel}
              onClick={focusTerminal}
            >
              <TerminalTabIcon />
              <span className="terminal-tab-label">终端</span>
            </button>
          ) : (
            visibleTabs.map((item) => {
              const itemActive = item.terminal_id === activeTerminalId;
              const itemStatus = itemActive && (opening || terminating)
                ? statusLabel
                : terminalStatusLabel(item.status);
              return (
                <div
                  key={item.terminal_id}
                  className={`terminal-tab ${itemActive ? "is-active" : ""} ${
                    item.status === "exited" ? "is-exited" : ""
                  }`}
                  title={`${itemStatus} · ${terminalTabLabel(item)}`}
                >
                  <button
                    type="button"
                    className="terminal-tab-main"
                    onClick={() => {
                      setActiveTerminalId(item.terminal_id);
                      window.requestAnimationFrame(() => {
                        fitSurface(item.terminal_id);
                        focusSurface(item.terminal_id);
                      });
                    }}
                  >
                    <TerminalTabIcon />
                    <span className="terminal-tab-label">{terminalTabLabel(item)}</span>
                  </button>
                  <button
                    type="button"
                    className="terminal-tab-close"
                    onClick={(event) => {
                      event.stopPropagation();
                      handleCloseTerminal(item.terminal_id);
                    }}
                    title={`关闭 ${terminalTabLabel(item)}`}
                    aria-label={`关闭 ${terminalTabLabel(item)}`}
                  >
                    <CloseIcon />
                  </button>
                </div>
              );
            })
          )}
          <button
            type="button"
            className="terminal-icon-action terminal-new-action"
            onClick={handleNewTerminal}
            title="新建终端"
            aria-label="新建终端"
            disabled={opening}
          >
            <PlusIcon />
          </button>
        </div>
        <div className="terminal-dock-actions">
          {error && <span className="terminal-error" title={error}>{error}</span>}
          <button
            type="button"
            className="terminal-icon-action"
            onClick={handleHide}
            title="隐藏终端"
            aria-label="隐藏终端"
          >
            <CloseIcon />
          </button>
        </div>
      </header>
      <div className="terminal-dock-viewports" onPaste={handlePaste}>
        {visibleTabs.map((item) => (
          <TerminalViewport
            key={item.terminal_id}
            terminalId={item.terminal_id}
            active={item.terminal_id === activeTerminalId}
            onNodeChange={handleViewportNodeChange}
            onFocus={focusSurface}
          />
        ))}
      </div>
    </section>
  );
}

interface TerminalViewportProps {
  terminalId: string;
  active: boolean;
  onNodeChange: (terminalId: string, node: HTMLDivElement | null) => void;
  onFocus: (terminalId: string) => void;
}

const TerminalViewport = memo(function TerminalViewport({
  terminalId,
  active,
  onNodeChange,
  onFocus,
}: TerminalViewportProps) {
  const ref = useCallback(
    (node: HTMLDivElement | null) => {
      onNodeChange(terminalId, node);
    },
    [onNodeChange, terminalId],
  );
  const focus = useCallback(() => {
    onFocus(terminalId);
  }, [onFocus, terminalId]);

  return (
    <div
      ref={ref}
      className={`terminal-dock-viewport ${active ? "is-active" : ""}`}
      role="application"
      tabIndex={active ? 0 : -1}
      aria-hidden={!active}
      onPointerDownCapture={focus}
      onClick={focus}
    />
  );
});

function TerminalTabIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <rect x="2.5" y="3" width="11" height="10" rx="2" />
      <path d="m5.2 6.1 2 1.9-2 1.9" />
      <path d="M8.6 10.1h2.4" />
    </svg>
  );
}

function PlusIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="M8 3.5v9" />
      <path d="M3.5 8h9" />
    </svg>
  );
}

function CloseIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="m4.5 4.5 7 7" />
      <path d="m11.5 4.5-7 7" />
    </svg>
  );
}
