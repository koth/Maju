import { useState, useRef, useCallback, useMemo, useEffect } from "react";
import type { AvailableCommand, SessionConfigControl, UiSnapshot, UserPromptContent } from "../../types";
import { sessionCancel, sessionSendPrompt, sessionReconnect, sessionSetConfigControl, workspaceOpen } from "../../lib/tauri";
import { open } from "@tauri-apps/plugin-dialog";
import "./Composer.css";

interface Props {
  snapshot: UiSnapshot;
  onStateChange: () => void;
  onWorkspaceChanged: (snapshot: UiSnapshot) => void;
}

interface Attachment {
  id: string;
  name: string;
  mimeType: string;
  data: string;
  kind: "image" | "file";
  previewUrl: string | null;
  thumbnailData: string | null;
  thumbnailMimeType: string | null;
}

export function Composer({ snapshot, onStateChange, onWorkspaceChanged }: Props) {
  const [input, setInput] = useState("");
  const [reconnecting, setReconnecting] = useState(false);
  const [switchingWorkspace, setSwitchingWorkspace] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [workspaceError, setWorkspaceError] = useState<string | null>(null);
  const [utilityOpen, setUtilityOpen] = useState(false);
  const [controlError, setControlError] = useState<string | null>(null);
  const [pendingControlId, setPendingControlId] = useState<string | null>(null);
  const [openControlId, setOpenControlId] = useState<string | null>(null);
  const [optimisticTurnActive, setOptimisticTurnActive] = useState(false);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [slashMenuOpen, setSlashMenuOpen] = useState(false);
  const [slashFilter, setSlashFilter] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const attachmentInputRef = useRef<HTMLInputElement>(null);

  const isInterrupted = snapshot.session.status === "Interrupted";
  const controls = snapshot.session_config.controls;
  const modelControl = useMemo(
    () => controls.find((control) => control.category === "Model"),
    [controls],
  );
  const modeControl = useMemo(
    () => controls.find((control) => control.category === "Mode"),
    [controls],
  );
  const extraControls = useMemo(
    () => controls.filter(
      (control) =>
        control.category !== "Model" &&
        control.category !== "Mode" &&
        !usesAgentDefault(control),
    ),
    [controls],
  );
  const controlsEnabled = snapshot.session.status === "Idle";
  const sessionBusy =
    snapshot.session.status === "Streaming" || snapshot.session.status === "WaitingForTool";
  const turnActive = sessionBusy || optimisticTurnActive;
  const imageInputEnabled = snapshot.prompt_capabilities?.image === true;
  const fileInputEnabled = snapshot.prompt_capabilities?.embedded_context === true;
  const attachmentInputEnabled = imageInputEnabled || fileInputEnabled;
  const canSend =
    (input.trim().length > 0 || attachments.length > 0) &&
    snapshot.session.status === "Idle" &&
    !optimisticTurnActive;

  const availableCommands = snapshot.available_commands ?? [];
  const filteredCommands = useMemo(() => {
    if (!slashMenuOpen || availableCommands.length === 0) return [];
    const filter = slashFilter.toLowerCase();
    return availableCommands.filter(
      (cmd) =>
        cmd.name.toLowerCase().includes(filter) ||
        cmd.description.toLowerCase().includes(filter),
    );
  }, [slashMenuOpen, slashFilter, availableCommands]);

  useEffect(() => {
    if (snapshot.session.status === "Idle" || snapshot.session.status === "Interrupted") {
      setOptimisticTurnActive(false);
      setCancelling(false);
    }
  }, [snapshot.session.status]);

  const handleSend = useCallback(async () => {
    if (!canSend) return;
    const prompt: UserPromptContent[] = [];
    const text = input.trim();
    if (text.length > 0) {
      prompt.push({ type: "text", text });
    }
    for (const attachment of attachments) {
      if (attachment.kind === "image") {
        prompt.push({
          type: "image",
          data: attachment.data,
          mime_type: attachment.mimeType,
          name: attachment.name,
          thumbnail_data: attachment.thumbnailData,
          thumbnail_mime_type: attachment.thumbnailMimeType,
        });
      } else {
        prompt.push({
          type: "file",
          data: attachment.data,
          mime_type: attachment.mimeType || null,
          name: attachment.name,
        });
      }
    }
    setInput("");
    setAttachments([]);
    setSlashMenuOpen(false);
    setOptimisticTurnActive(true);
    try {
      await sessionSendPrompt(prompt);
      onStateChange();
    } catch (error) {
      setOptimisticTurnActive(false);
      setControlError(String(error));
    }
  }, [attachments, canSend, input, onStateChange]);

  const handleInputChange = useCallback((value: string) => {
    setInput(value);
    if (value.startsWith("/") && availableCommands.length > 0 && !turnActive) {
      const afterSlash = value.slice(1).split(/\s/)[0];
      if (!value.includes(" ")) {
        setSlashMenuOpen(true);
        setSlashFilter(afterSlash);
      } else {
        setSlashMenuOpen(false);
      }
    } else {
      setSlashMenuOpen(false);
      setSlashFilter("");
    }
  }, [availableCommands, turnActive]);

  const handleSlashCommandSelect = useCallback((command: AvailableCommand) => {
    setInput(`/${command.name} `);
    setSlashMenuOpen(false);
    setSlashFilter("");
    textareaRef.current?.focus();
  }, []);

  const handleAttachmentFiles = useCallback(async (files: FileList | null) => {
    if (!files || files.length === 0 || !attachmentInputEnabled) return;
    setControlError(null);
    try {
      const selected = Array.from(files).filter((file) =>
        file.type.startsWith("image/") ? imageInputEnabled : fileInputEnabled,
      );
      if (selected.length !== files.length) {
        setControlError("部分文件已被跳过，因为当前智能体不支持它们");
      }
      const nextAttachments = await Promise.all(selected.map(readAttachment));
      setAttachments((current) => [...current, ...nextAttachments]);
    } catch (error) {
      setControlError(String(error));
    } finally {
      if (attachmentInputRef.current) {
        attachmentInputRef.current.value = "";
      }
    }
  }, [attachmentInputEnabled, fileInputEnabled, imageInputEnabled]);

  const handlePaste = useCallback(async (event: React.ClipboardEvent<HTMLTextAreaElement>) => {
    if (!imageInputEnabled) return;
    const files = Array.from(event.clipboardData.items)
      .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
      .map((item) => item.getAsFile())
      .filter((file): file is File => file !== null);
    if (files.length === 0) return;

    event.preventDefault();
    setControlError(null);
    try {
      const nextAttachments = await Promise.all(files.map(readAttachment));
      setAttachments((current) => [...current, ...nextAttachments]);
    } catch (error) {
      setControlError(String(error));
    }
  }, [imageInputEnabled]);

  const handleCancel = useCallback(async () => {
    if (!turnActive || cancelling) return;
    setCancelling(true);
    try {
      await sessionCancel();
      onStateChange();
    } catch (_e) {
      // error handling via polling
    } finally {
      setCancelling(false);
    }
  }, [turnActive, cancelling, onStateChange]);

  const handleReconnect = useCallback(async () => {
    setReconnecting(true);
    try {
      await sessionReconnect();
      onStateChange();
    } catch (_e) {
      // error handling via polling
    } finally {
      setReconnecting(false);
    }
  }, [onStateChange]);

  const handleSwitchWorkspace = useCallback(async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      const path = Array.isArray(selected) ? selected[0] : selected;
      if (!path) return;
      setSwitchingWorkspace(true);
      setWorkspaceError(null);
      const nextSnapshot = await workspaceOpen(path);
      onWorkspaceChanged(nextSnapshot);
      setUtilityOpen(false);
    } catch (error) {
      setWorkspaceError(String(error));
    } finally {
      setSwitchingWorkspace(false);
    }
  }, [onWorkspaceChanged]);

  const handleControlChange = useCallback(
    async (control: SessionConfigControl, valueId: string) => {
      if (!controlsEnabled || control.current_value_id === valueId) return;
      setPendingControlId(control.id);
      setControlError(null);
      try {
        await sessionSetConfigControl(control.id, valueId);
        onStateChange();
      } catch (error) {
        setControlError(String(error));
      } finally {
        setPendingControlId(null);
      }
    },
    [controlsEnabled, onStateChange],
  );

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (slashMenuOpen && filteredCommands.length > 0) {
      if (e.key === "Escape") {
        e.preventDefault();
        setSlashMenuOpen(false);
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.ctrlKey)) {
        e.preventDefault();
        handleSlashCommandSelect(filteredCommands[0]);
        return;
      }
    }
    if (e.key === "Enter" && e.ctrlKey && canSend) {
      e.preventDefault();
      handleSend();
    }
  };

  if (isInterrupted) {
    return (
      <div className="composer">
        <div className="composer-inner composer-disconnected">
          <span className="composer-disconnected-text">
            会话已断开
          </span>
          <button
            className="composer-reconnect-btn"
            onClick={handleReconnect}
            disabled={reconnecting}
          >
            {reconnecting ? "正在重新连接..." : "重新连接"}
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="composer">
      <div className={`composer-inner ${turnActive ? "is-turn-active" : ""}`}>
        <div className="composer-input-wrap">
          {slashMenuOpen && filteredCommands.length > 0 && (
            <div className="composer-slash-menu" role="listbox" aria-label="斜杠命令">
              {filteredCommands.map((cmd) => (
                <button
                  key={cmd.name}
                  className="composer-slash-option"
                  type="button"
                  role="option"
                  onMouseDown={(e) => {
                    e.preventDefault();
                    handleSlashCommandSelect(cmd);
                  }}
                >
                  <span className="composer-slash-name">/{cmd.name}</span>
                  <span className="composer-slash-desc">{cmd.description}</span>
                </button>
              ))}
            </div>
          )}
          <textarea
            ref={textareaRef}
            className="composer-textarea"
            value={input}
            onChange={(e) => handleInputChange(e.target.value)}
            onKeyDown={handleKeyDown}
            onPaste={handlePaste}
            placeholder={turnActive ? "智能体正在工作...添加指引或停止本轮" : "委托任务，附加上下文，然后按 Ctrl+Enter 发送"}
            rows={2}
          />
          <button
            className={`composer-send-btn ${canSend ? "composer-send-btn-active" : ""} ${turnActive ? "composer-stop-btn" : ""}`}
            disabled={turnActive ? cancelling : !canSend}
            onClick={turnActive ? handleCancel : handleSend}
            title={turnActive ? "取消当前轮次" : "发送提示"}
            aria-label={turnActive ? "取消当前轮次" : "发送提示"}
          >
            {turnActive ? <span className="composer-stop-icon" /> : "↑"}
          </button>
        </div>
        {attachments.length > 0 && (
          <div className="composer-attachment-strip" aria-label="已附加的文件">
            {attachments.map((attachment) => (
              <div className="composer-attachment-chip" key={attachment.id}>
                {attachment.previewUrl ? (
                  <img src={attachment.previewUrl} alt={attachment.name} />
                ) : (
                  <span className="composer-file-glyph">FILE</span>
                )}
                <span>{attachment.name}</span>
                <button
                  type="button"
                  onClick={() => setAttachments((current) => current.filter((item) => item.id !== attachment.id))}
                  aria-label={`移除 ${attachment.name}`}
                >
                  x
                </button>
              </div>
            ))}
          </div>
        )}
        <div className="composer-control-rail">
          <input
            ref={attachmentInputRef}
            className="composer-attachment-input"
            type="file"
            multiple
            onChange={(event) => handleAttachmentFiles(event.currentTarget.files)}
          />
          <button
            className="composer-attachment-btn"
            type="button"
            disabled={!controlsEnabled || !attachmentInputEnabled || pendingControlId !== null}
            onClick={() => attachmentInputRef.current?.click()}
            title={attachmentInputEnabled ? "附加图片或文件" : "当前智能体不支持附件"}
            aria-label="附加图片或文件"
          >
            +
          </button>
          <div className="composer-utility">
            <button
              className="composer-utility-btn"
              type="button"
              onClick={() => setUtilityOpen((open) => !open)}
              aria-expanded={utilityOpen}
              title={snapshot.workspace.root}
            >
              <span className="composer-utility-mark" aria-hidden="true">
                <svg viewBox="0 0 20 20" focusable="false">
                  <path d="M4 6h9.5l-2-2L13 2.5 17.5 7 13 11.5 11.5 10l2-2H4V6Z" />
                  <path d="M16 14H6.5l2 2L7 17.5 2.5 13 7 8.5 8.5 10l-2 2H16v2Z" />
                </svg>
              </span>
              <span>{snapshot.workspace.name}</span>
            </button>
            {utilityOpen && (
              <div className="composer-utility-menu">
                <button
                  type="button"
                  onClick={handleSwitchWorkspace}
                  disabled={switchingWorkspace || snapshot.session.status !== "Idle"}
                >
                  {switchingWorkspace ? "正在切换..." : "切换工作区"}
                </button>
              </div>
            )}
          </div>

          {modelControl ? (
            <SessionControlSelect
              control={modelControl}
              disabled={!controlsEnabled || pendingControlId !== null}
              pending={pendingControlId === modelControl.id}
              open={openControlId === modelControl.id}
              onOpenChange={(open) => setOpenControlId(open ? modelControl.id : null)}
              onChange={handleControlChange}
            />
          ) : (
            <span className="composer-static-control">{snapshot.session.model}</span>
          )}
          {modeControl && (
            <SessionControlSelect
              control={modeControl}
              disabled={!controlsEnabled || pendingControlId !== null}
              pending={pendingControlId === modeControl.id}
              open={openControlId === modeControl.id}
              onOpenChange={(open) => setOpenControlId(open ? modeControl.id : null)}
              onChange={handleControlChange}
            />
          )}
          {extraControls.map((control) => (
            <SessionControlSelect
              key={control.id}
              control={control}
              disabled={!controlsEnabled || pendingControlId !== null}
              pending={pendingControlId === control.id}
              open={openControlId === control.id}
              onOpenChange={(open) => setOpenControlId(open ? control.id : null)}
              onChange={handleControlChange}
            />
          ))}
          <span className="composer-rail-spacer" />
          <span className="composer-session-state">{snapshot.session.status}</span>
        </div>
        {(workspaceError || controlError) && (
          <div className="composer-error">{workspaceError ?? controlError}</div>
        )}
      </div>
    </div>
  );
}

function readAttachment(file: File): Promise<Attachment> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = async () => {
      const result = String(reader.result ?? "");
      const [, data = ""] = result.split(",", 2);
      if (!data) {
        reject(new Error(`Could not read ${file.name}`));
        return;
      }
      try {
        const isImage = file.type.startsWith("image/");
        const thumbnail = isImage ? await createImageThumbnail(result) : null;
        resolve({
          id: `${file.name}-${file.lastModified}-${file.size}`,
          name: file.name,
          mimeType: file.type || "application/octet-stream",
          data,
          kind: isImage ? "image" : "file",
          previewUrl: isImage ? result : null,
          thumbnailData: thumbnail?.data ?? null,
          thumbnailMimeType: thumbnail?.mimeType ?? null,
        });
      } catch (error) {
        reject(error);
      }
    };
    reader.onerror = () => reject(reader.error ?? new Error(`Could not read ${file.name}`));
    reader.readAsDataURL(file);
  });
}

function createImageThumbnail(dataUrl: string): Promise<{ data: string; mimeType: string }> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => {
      const canvas = document.createElement("canvas");
      const size = 64;
      canvas.width = size;
      canvas.height = size;
      const context = canvas.getContext("2d");
      if (!context) {
        reject(new Error("Could not create image thumbnail"));
        return;
      }
      const scale = Math.min(size / image.width, size / image.height);
      const width = Math.max(1, Math.round(image.width * scale));
      const height = Math.max(1, Math.round(image.height * scale));
      context.clearRect(0, 0, size, size);
      context.drawImage(image, Math.round((size - width) / 2), Math.round((size - height) / 2), width, height);
      const thumbnailUrl = canvas.toDataURL("image/png");
      const [, data = ""] = thumbnailUrl.split(",", 2);
      if (!data) {
        reject(new Error("Could not create image thumbnail"));
        return;
      }
      resolve({ data, mimeType: "image/png" });
    };
    image.onerror = () => reject(new Error("Could not create image thumbnail"));
    image.src = dataUrl;
  });
}

function usesAgentDefault(control: SessionConfigControl) {
  const key = `${control.id} ${control.label}`.toLowerCase();
  return (
    control.category === "ThoughtLevel" ||
    key.includes("deep think") ||
    key.includes("think") ||
    key.includes("thought") ||
    key.includes("reasoning") ||
    key.includes("sandbox")
  );
}

function SessionControlSelect({
  control,
  disabled,
  pending,
  open,
  onOpenChange,
  onChange,
}: {
  control: SessionConfigControl;
  disabled: boolean;
  pending: boolean;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onChange: (control: SessionConfigControl, valueId: string) => void;
}) {
  if (control.choices.length === 0) return null;

  const unavailable = disabled || !control.enabled || pending;
  const selected =
    control.choices.find((choice) => choice.id === control.current_value_id) ??
    control.choices[0];
  const label = displayControlLabel(control);

  return (
    <div
      className={`composer-control-select ${open ? "is-open" : ""}`}
      onBlur={(event) => {
        const nextFocus = event.relatedTarget;
        if (!(nextFocus instanceof Node) || !event.currentTarget.contains(nextFocus)) {
          onOpenChange(false);
        }
      }}
    >
      <button
        className="composer-control-trigger"
        type="button"
        disabled={unavailable}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => onOpenChange(!open)}
      >
        {label && <span className="composer-control-label">{label}</span>}
        <span className="composer-control-value">{pending ? "更新中" : selected.label}</span>
        <span className="composer-control-chevron">v</span>
      </button>
      {open && !unavailable && (
        <div className="composer-control-menu" role="listbox">
          {control.choices.map((choice) => {
            const selectedChoice = choice.id === control.current_value_id;
            return (
              <button
                key={choice.id}
                className={`composer-control-option ${selectedChoice ? "is-selected" : ""}`}
                type="button"
                role="option"
                aria-selected={selectedChoice}
                onClick={() => {
                  onChange(control, choice.id);
                  onOpenChange(false);
                }}
              >
                <span>{choice.label}</span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

function displayControlLabel(control: SessionConfigControl) {
  if (control.category === "Model" || control.category === "Mode") return null;
  return control.label;
}
