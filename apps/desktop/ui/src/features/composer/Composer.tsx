import { useState, useRef, useCallback, useMemo, useEffect } from "react";
import type { AvailableCommand, SessionConfigControl, UiSnapshot, UserPromptContent } from "../../types";
import { editorGetContent, sessionCancel, sessionSendPrompt, sessionReconnect, sessionSetConfigControl } from "../../lib/tauri";
import "./Composer.css";

export interface ComposerReferenceRequest {
  id: string;
  path: string;
  text?: string | null;
  startLine?: number | null;
  endLine?: number | null;
}

interface Props {
  snapshot: UiSnapshot;
  onStateChange: () => void;
  referenceRequests?: ComposerReferenceRequest[];
  onReferenceRequestConsumed?: (id: string) => void;
  compact?: boolean;
}

interface Attachment {
  id: string;
  name: string;
  displayName: string;
  mimeType: string;
  data: string | null;
  text: string | null;
  uri: string | null;
  kind: "image" | "file" | "workspace_file";
  path: string | null;
  startLine: number | null;
  endLine: number | null;
  previewUrl: string | null;
  thumbnailData: string | null;
  thumbnailMimeType: string | null;
}

interface ModelProviderGroup {
  id: string;
  label: string;
  choices: SessionConfigControl["choices"];
}

interface ModelProviderBucket {
  label: string | null;
  choices: SessionConfigControl["choices"];
}

export function Composer({
  snapshot,
  onStateChange,
  referenceRequests = [],
  onReferenceRequestConsumed,
  compact = false,
}: Props) {
  const [input, setInput] = useState("");
  const [reconnecting, setReconnecting] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [controlError, setControlError] = useState<string | null>(null);
  const [pendingControlId, setPendingControlId] = useState<string | null>(null);
  const [openControlId, setOpenControlId] = useState<string | null>(null);
  const [optimisticTurnActive, setOptimisticTurnActive] = useState(false);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [activeImagePreviewId, setActiveImagePreviewId] = useState<string | null>(null);
  const [slashMenuOpen, setSlashMenuOpen] = useState(false);
  const [slashFilter, setSlashFilter] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const attachmentInputRef = useRef<HTMLInputElement>(null);
  const processingReferenceIds = useRef<Set<string>>(new Set());

  const isInterrupted = snapshot.session.status === "Interrupted";
  const controls = snapshot.session_config.controls;
  const modelControl = useMemo(
    () => controls.find((control) => control.category === "Model"),
    [controls],
  );
  const modelProviderGroups = useMemo(
    () => (modelControl ? byokModelProviderGroupsForControl(modelControl) : []),
    [modelControl],
  );
  const activeModelProviderGroup = useMemo(
    () => activeModelProviderGroupForControl(modelControl, modelProviderGroups),
    [modelControl, modelProviderGroups],
  );
  const modelProviderControl = useMemo(
    () => buildModelProviderControl(modelControl, modelProviderGroups, activeModelProviderGroup),
    [activeModelProviderGroup, modelControl, modelProviderGroups],
  );
  const visibleModelControl = useMemo(
    () => filterModelControlForProvider(modelControl, activeModelProviderGroup),
    [activeModelProviderGroup, modelControl],
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
  const sessionConfigPending = !snapshot.session_config.hydrated && snapshot.session.status === "Idle" && !optimisticTurnActive;
  const controlsEnabled = snapshot.session.status === "Idle" && !sessionConfigPending;
  const sessionBusy =
    snapshot.session.status === "Streaming" || snapshot.session.status === "WaitingForTool";
  const turnActive = sessionBusy || optimisticTurnActive;
  const imageInputEnabled = snapshot.prompt_capabilities?.image === true;
  const fileInputEnabled = snapshot.prompt_capabilities?.embedded_context === true;
  const sessionSteerEnabled = snapshot.prompt_capabilities?.session_steer === true;
  const attachmentInputEnabled = imageInputEnabled || fileInputEnabled;
  const canSteer = turnActive && sessionSteerEnabled;
  const textInputEnabled = (!turnActive || canSteer) && !sessionConfigPending;
  const activeAttachmentInputEnabled = !turnActive && !sessionConfigPending && attachmentInputEnabled;
  const attachmentButtonTitle = sessionConfigPending
    ? "正在加载模型列表"
    : turnActive
      ? "当前轮次运行中暂不支持附件"
      : attachmentInputEnabled
        ? "附加图片或文件"
        : "当前智能体不支持附件";
  const trimmedInput = input.trim();
  const canSendIdle =
    (trimmedInput.length > 0 || attachments.length > 0) &&
    snapshot.session.status === "Idle" &&
    !optimisticTurnActive &&
    !sessionConfigPending;
  const canSendSteer = canSteer && trimmedInput.length > 0;
  const canSend = canSendIdle || canSendSteer;
  const primaryActionIsStop = turnActive && !canSendSteer;
  const primaryActionEnabled = primaryActionIsStop ? !cancelling : canSend;
  const primaryActionTitle = sessionConfigPending
    ? "正在加载模型列表"
    : turnActive
      ? primaryActionIsStop
        ? "停止当前轮次"
        : "追加指令"
      : "发送提示";
  const activeImagePreview = useMemo(
    () => attachments.find((attachment) => attachment.id === activeImagePreviewId && attachment.previewUrl) ?? null,
    [activeImagePreviewId, attachments],
  );

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

  useEffect(() => {
    if (referenceRequests.length === 0) return;

    let disposed = false;
    async function addReference(request: ComposerReferenceRequest) {
      if (processingReferenceIds.current.has(request.id)) return;
      processingReferenceIds.current.add(request.id);
      try {
        if (turnActive) {
          setControlError("当前轮次运行中暂不支持添加引用");
          return;
        }
        if (!fileInputEnabled) {
          setControlError("当前智能体不支持文件引用");
          return;
        }
        const attachment = await attachmentFromReference(
          request,
          snapshot.workspace.root,
          imageInputEnabled,
        );
        if (disposed) return;
        setAttachments((current) => {
          if (attachment.uri && current.some((item) => item.uri === attachment.uri)) {
            return current;
          }
          return [...current, attachment];
        });
        setControlError(null);
        textareaRef.current?.focus();
      } catch (error) {
        if (!disposed) setControlError(String(error));
      } finally {
        processingReferenceIds.current.delete(request.id);
        onReferenceRequestConsumed?.(request.id);
      }
    }

    for (const request of referenceRequests) {
      void addReference(request);
    }

    return () => {
      disposed = true;
    };
  }, [
    fileInputEnabled,
    imageInputEnabled,
    onReferenceRequestConsumed,
    referenceRequests,
    snapshot.workspace.root,
    turnActive,
  ]);

  useEffect(() => {
    if (!activeImagePreview) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setActiveImagePreviewId(null);
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [activeImagePreview]);

  const removeAttachment = useCallback((attachmentId: string) => {
    setAttachments((current) => current.filter((item) => item.id !== attachmentId));
    setActiveImagePreviewId((current) => (current === attachmentId ? null : current));
  }, []);

  const handleSend = useCallback(async () => {
    if (!canSend) return;
    const prompt: UserPromptContent[] = [];
    const text = trimmedInput;
    if (turnActive) {
      prompt.push({ type: "text", text });
    } else {
      for (const attachment of attachments) {
        if (attachment.kind !== "workspace_file" || !attachment.path) continue;
        prompt.push({
          type: "workspace_file",
          path: attachment.path,
          start_line: attachment.startLine,
          end_line: attachment.endLine,
        });
      }
      if (text.length > 0) {
        prompt.push({ type: "text", text });
      }
      for (const attachment of attachments) {
        if (attachment.kind === "workspace_file") continue;
        if (attachment.kind === "image") {
          if (!attachment.data) continue;
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
            text: attachment.text,
            uri: attachment.uri,
            mime_type: attachment.mimeType || null,
            name: attachment.name,
          });
        }
      }
    }
    setInput("");
    if (!turnActive) {
      setAttachments([]);
    }
    setActiveImagePreviewId(null);
    setSlashMenuOpen(false);
    setOptimisticTurnActive(true);
    try {
      await sessionSendPrompt(prompt);
      onStateChange();
    } catch (error) {
      setOptimisticTurnActive(false);
      setControlError(String(error));
    }
  }, [attachments, canSend, onStateChange, trimmedInput, turnActive]);

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
    if (!files || files.length === 0 || !activeAttachmentInputEnabled) return;
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
  }, [activeAttachmentInputEnabled, fileInputEnabled, imageInputEnabled]);

  const handlePaste = useCallback(async (event: React.ClipboardEvent<HTMLTextAreaElement>) => {
    if (turnActive || !imageInputEnabled) return;
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
  }, [imageInputEnabled, turnActive]);

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

  const handlePrimaryAction = useCallback(() => {
    if (primaryActionIsStop) {
      void handleCancel();
      return;
    }
    void handleSend();
  }, [handleCancel, handleSend, primaryActionIsStop]);

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

  const handleControlChange = useCallback(
    async (control: SessionConfigControl, valueId: string) => {
      const isModelSelection = control.category === "Model" && control.id === modelControl?.id;
      if (!controlsEnabled || (!isModelSelection && control.current_value_id === valueId)) return;
      setPendingControlId(control.id);
      setControlError(null);
      try {
        const selectedChoice = control.choices.find((choice) => choice.id === valueId);
        const requestProvider = isModelSelection
          ? modelChoiceRequestProvider(selectedChoice, activeModelProviderGroup?.id)
          : selectedChoice?.provider ?? null;
        const requestValueId = configControlRequestValue(control, valueId, requestProvider);
        await sessionSetConfigControl(
          control.id,
          requestValueId,
          configControlRequestProvider(control, requestValueId, requestProvider),
        );
        onStateChange();
      } catch (error) {
        setControlError(String(error));
      } finally {
        setPendingControlId(null);
      }
    },
    [activeModelProviderGroup?.id, controlsEnabled, modelControl?.id, onStateChange],
  );

  const handleModelProviderChange = useCallback(
    async (_control: SessionConfigControl, providerId: string) => {
      if (!controlsEnabled || !modelControl) return;
      const group = modelProviderGroups.find((candidate) => candidate.id === providerId);
      const currentModelName = currentModelNameForControl(modelControl);
      const targetChoice =
        group?.choices.find((choice) => modelChoiceMatchesCurrentControl(choice, modelControl, group.id)) ??
        group?.choices.find((choice) => modelChoiceMatchesModelName(choice, currentModelName)) ??
        group?.choices[0];
      if (!targetChoice) return;
      const providerControlId = modelProviderControlId(modelControl.id);
      setPendingControlId(providerControlId);
      setControlError(null);
      try {
        const requestProvider = modelChoiceRequestProvider(targetChoice, group?.id);
        const requestValueId = configControlRequestValue(modelControl, targetChoice.id, requestProvider);
        await sessionSetConfigControl(
          modelControl.id,
          requestValueId,
          configControlRequestProvider(modelControl, requestValueId, requestProvider),
        );
        onStateChange();
      } catch (error) {
        setControlError(String(error));
      } finally {
        setPendingControlId(null);
      }
    },
    [controlsEnabled, modelControl, modelProviderGroups, onStateChange],
  );

  const handleKeyDown = (e: React.KeyboardEvent) => {
    // IME composition in progress (e.g. picking a Chinese candidate): let the
    // Enter confirm the candidate instead of sending the message.
    const isComposing = e.nativeEvent.isComposing || e.keyCode === 229;
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
    if (e.key === "Enter" && !isComposing && !e.shiftKey && canSend) {
      // Plain Enter sends; Shift+Enter inserts a newline.
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

  const composerClassName = [
    "composer",
    compact ? "is-compact" : "",
    attachments.length > 0 ? "has-attachments" : "",
  ].filter(Boolean).join(" ");

  return (
    <div className={composerClassName}>
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
          {attachments.length > 0 && (
            <div className="composer-attachment-strip" aria-label="已附加的文件">
              {attachments.map((attachment) =>
                attachment.previewUrl ? (
                  <div className="composer-image-attachment" key={attachment.id}>
                    <button
                      type="button"
                      className="composer-image-preview-btn"
                      onClick={() => setActiveImagePreviewId(attachment.id)}
                      aria-label={`预览 ${attachment.name}`}
                      title="预览图片"
                    >
                      <img src={attachment.previewUrl} alt={attachment.name} />
                    </button>
                    <button
                      className="composer-attachment-remove composer-image-remove"
                      type="button"
                      onClick={() => removeAttachment(attachment.id)}
                      aria-label={`移除 ${attachment.name}`}
                    >
                      x
                    </button>
                  </div>
                ) : (
                  <div
                    className={`composer-attachment-chip ${attachment.uri ? "composer-reference-chip" : ""}`}
                    key={attachment.id}
                  >
                    {attachment.uri ? (
                      <span className="composer-reference-mention" title={attachment.displayName}>
                        @{attachment.displayName}
                      </span>
                    ) : (
                      <>
                        <span className="composer-file-glyph">FILE</span>
                        <span title={attachment.name}>{attachment.name}</span>
                      </>
                    )}
                    <button
                      className="composer-attachment-remove"
                      type="button"
                      onClick={() => removeAttachment(attachment.id)}
                      aria-label={`移除 ${attachment.name}`}
                    >
                      x
                    </button>
                  </div>
                ),
              )}
            </div>
          )}
          <textarea
            ref={textareaRef}
            className={`composer-textarea ${attachments.length > 0 ? "has-attachments" : ""}`}
            value={input}
            onChange={(e) => handleInputChange(e.target.value)}
            onKeyDown={handleKeyDown}
            onPaste={handlePaste}
            disabled={!textInputEnabled}
            placeholder={sessionConfigPending ? "正在加载模型列表..." : turnActive ? (sessionSteerEnabled ? "补充约束，继续引导当前轮次" : "当前智能体暂不支持追加指令") : "交给 Kodex 一个明确目标"}
            rows={compact ? 1 : 2}
          />
        </div>
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
            disabled={!controlsEnabled || !activeAttachmentInputEnabled || pendingControlId !== null}
            onClick={() => attachmentInputRef.current?.click()}
            title={attachmentButtonTitle}
            aria-label="附加图片或文件"
          >
            <PaperclipIcon />
          </button>
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
          {modelProviderControl && (
            <SessionControlSelect
              control={modelProviderControl}
              disabled={!controlsEnabled || pendingControlId !== null}
              pending={pendingControlId === modelProviderControl.id}
              open={openControlId === modelProviderControl.id}
              onOpenChange={(open) => setOpenControlId(open ? modelProviderControl.id : null)}
              onChange={handleModelProviderChange}
            />
          )}
          {sessionConfigPending ? (
            <span className="composer-static-control is-loading" data-control-id="model" role="status" aria-live="polite" aria-label="模型加载中">
              <span className="composer-loading-spinner" aria-hidden="true" />
              模型加载中
            </span>
          ) : visibleModelControl ? (
            <SessionControlSelect
              control={visibleModelControl}
              disabled={!controlsEnabled || pendingControlId !== null}
              pending={pendingControlId === visibleModelControl.id}
              open={openControlId === visibleModelControl.id}
              onOpenChange={(open) => setOpenControlId(open ? visibleModelControl.id : null)}
              onChange={handleControlChange}
            />
          ) : (
            <span className="composer-static-control" data-control-id="model">
              {displayModelValue(snapshot.session.model)}
            </span>
          )}

          <span className="composer-session-state">
            <span className="composer-session-state-dot" />
            {snapshot.session.status}
          </span>
          <button
            className={[
              "composer-send-btn",
              canSend ? "composer-send-btn-active" : "",
              primaryActionIsStop ? "composer-stop-btn" : "",
            ].filter(Boolean).join(" ")}
            disabled={!primaryActionEnabled}
            onClick={handlePrimaryAction}
            title={primaryActionTitle}
            aria-label={primaryActionTitle}
          >
            {primaryActionIsStop ? <span className="composer-stop-icon" /> : <SendIcon />}
          </button>
        </div>
        {controlError && (
          <div className="composer-error">{controlError}</div>
        )}
      </div>
      {activeImagePreview?.previewUrl && (
        <div
          className="composer-image-preview-backdrop"
          role="dialog"
          aria-modal="true"
          aria-label="图片预览"
          onClick={() => setActiveImagePreviewId(null)}
        >
          <button
            type="button"
            className="composer-image-preview-close"
            onClick={() => setActiveImagePreviewId(null)}
            aria-label="关闭图片预览"
          >
            x
          </button>
          <img
            className="composer-image-preview-original"
            src={activeImagePreview.previewUrl}
            alt={activeImagePreview.name}
            onClick={(event) => event.stopPropagation()}
          />
        </div>
      )}
    </div>
  );
}

const MODEL_PROVIDER_LABELS: Record<string, string> = {
  timiai: "TimiAI",
  commandcode: "CommandCode",
  deepseek: "DeepSeek",
  kimi_code: "Kimi Code",
  xiaomi_mimo: "Xiaomi Token Plan",
  codebuddy: "CodeBuddy",
};

const MODEL_PROVIDER_ORDER = [
  "timiai",
  "commandcode",
  "deepseek",
  "kimi_code",
  "xiaomi_mimo",
  "codebuddy",
  "custom",
];
const BYOK_SOURCE_MODEL_PROVIDER_IDS = new Set(MODEL_PROVIDER_ORDER);
function isByokSourceModelProviderId(providerId: string) {
  return BYOK_SOURCE_MODEL_PROVIDER_IDS.has(providerId) || providerId.startsWith("custom_");
}
const GENERIC_MODEL_PROVIDER_IDS = new Set(["byok", "codex", "default", "kodex", "kodex_proxy", "kodex-proxy"]);

function providerDisplayLabel(providerId: string, label?: string | null) {
  const normalizedLabel = label?.trim();
  return normalizedLabel || MODEL_PROVIDER_LABELS[providerId] || providerId;
}

function byokModelProviderGroupsForControl(control: SessionConfigControl): ModelProviderGroup[] {
  const groups = new Map<string, ModelProviderBucket>();
  const choices = dedupeControlChoices(control.choices);
  for (const choice of choices) {
    const decoded = decodeProviderModelChoice(choice);
    const providerId = decoded.provider;
    if (!providerId) continue;
    const current = groups.get(providerId) ?? { label: null, choices: [] };
    const providerLabel = decoded.choice.provider_label?.trim();
    if (providerLabel) {
      current.label = providerLabel;
    }
    current.choices.push(decoded.choice);
    groups.set(providerId, current);
  }
  const currentModel = currentProviderModelForControl(control);
  if (currentModel) {
    const current = groups.get(currentModel.provider) ?? { label: null, choices: [] };
    if (!current.choices.some((choice) => modelChoiceMatchesProviderModel(choice, currentModel, currentModel.provider))) {
      current.choices.push({
        id: control.current_value_id,
        label: currentModel.model,
        description: null,
        provider: currentModel.provider,
        provider_label: providerDisplayLabel(currentModel.provider, current.label),
      });
    }
    current.label = providerDisplayLabel(currentModel.provider, current.label);
    groups.set(currentModel.provider, current);
  }
  if (groups.size === 0) return [];
  return [...groups.entries()]
    .sort(([left], [right]) => {
      const byOrder = modelProviderOrderIndex(left) - modelProviderOrderIndex(right);
      return byOrder || left.localeCompare(right);
    })
    .map(([id, group]) => ({
      id,
      label: providerDisplayLabel(id, group.label),
      choices: group.choices,
    }));
}
function activeModelProviderGroupForControl(
  control: SessionConfigControl | undefined,
  groups: ModelProviderGroup[],
) {
  if (!control || groups.length === 0) return null;
  const currentModel = currentProviderModelForControl(control);
  if (currentModel) {
    const currentGroup = groups.find((group) => group.id === currentModel.provider);
    if (currentGroup) return currentGroup;
  }
  const inferredProvider = inferredProviderForModelName(currentModelNameForControl(control), groups);
  if (inferredProvider) {
    const inferredGroup = groups.find((group) => group.id === inferredProvider);
    if (inferredGroup) return inferredGroup;
  }
  return groups.find((group) =>
    group.choices.some((choice) => modelChoiceMatchesCurrentControl(choice, control, group.id)),
  ) ?? groups[0];
}

function buildModelProviderControl(
  modelControl: SessionConfigControl | undefined,
  groups: ModelProviderGroup[],
  activeGroup: ModelProviderGroup | null,
): SessionConfigControl | null {
  if (!modelControl || groups.length === 0 || !activeGroup) return null;
  return {
    id: modelProviderControlId(modelControl.id),
    label: "Provider",
    description: "BYOK provider",
    category: "Other",
    source: modelControl.source,
    current_value_id: activeGroup.id,
    current_value_label: activeGroup.label,
    choices: groups.map((group) => ({
      id: group.id,
      label: group.label,
      description: `${group.choices.length} models`,
      provider: null,
    })),
    enabled: modelControl.enabled,
  };
}

function filterModelControlForProvider(
  modelControl: SessionConfigControl | undefined,
  activeGroup: ModelProviderGroup | null,
): SessionConfigControl | undefined {
  if (!modelControl || !activeGroup) return modelControl;
  const selected =
    activeGroup.choices.find((choice) => modelChoiceMatchesCurrentControl(choice, modelControl, activeGroup.id)) ??
    activeGroup.choices[0];
  return {
    ...modelControl,
    current_value_id: selected?.id ?? modelControl.current_value_id,
    current_value_label: selected?.label ?? modelControl.current_value_label,
    choices: activeGroup.choices,
  };
}

function modelProviderControlId(modelControlId: string) {
  return `${modelControlId}:provider`;
}

function modelProviderOrderIndex(providerId: string) {
  const index = MODEL_PROVIDER_ORDER.indexOf(providerId);
  return index === -1 ? MODEL_PROVIDER_ORDER.length : index;
}

function decodeProviderModelChoice(
  choice: SessionConfigControl["choices"][number],
): {
  provider: string | null;
  choice: SessionConfigControl["choices"][number];
} {
  const encoded = decodeProviderModelValue(choice.id) ?? decodeProviderModelValue(choice.label);
  const modelName = encoded?.model ?? displayModelChoiceLabel(choice);
  const provider =
    concreteModelProviderForModel(encoded?.provider, modelName) ??
    concreteModelProviderForModel(choice.provider, modelName);
  if (!provider) {
    return { provider: null, choice };
  }

  return {
    provider,
    choice: {
      ...choice,
      provider,
      label: displayModelChoiceLabel(choice),
    },
  };
}

function modelChoiceRequestProvider(
  choice: SessionConfigControl["choices"][number] | undefined,
  groupProvider: string | null | undefined,
) {
  const normalizedGroupProvider = normalizeModelProviderId(groupProvider);
  if (normalizedGroupProvider) return normalizedGroupProvider;
  if (!choice) return null;
  const encoded = decodeProviderModelValue(choice.id) ?? decodeProviderModelValue(choice.label);
  return encoded?.provider ?? normalizeModelProviderId(choice.provider);
}
function configControlRequestValue(
  control: SessionConfigControl,
  valueId: string,
  provider: string | null,
) {
  const normalizedProvider = normalizeModelProviderId(provider);
  if (
    control.category === "Model" &&
    normalizedProvider &&
    isByokSourceModelProviderId(normalizedProvider) &&
    !decodeProviderModelValue(valueId)
  ) {
    return `kodex-provider/byok/${normalizedProvider}/${valueId}`;
  }
  return valueId;
}

function configControlRequestProvider(
  control: SessionConfigControl,
  valueId: string,
  provider: string | null,
) {
  if (control.category === "Model" && decodeProviderModelValue(valueId)) {
    return null;
  }
  return provider;
}

function currentProviderModelForControl(control: SessionConfigControl) {
  const current =
    decodeProviderModelValue(control.current_value_id) ??
    decodeProviderModelValue(control.current_value_label);
  if (!current) return null;
  const provider = concreteModelProviderForModel(current.provider, current.model);
  return provider ? { ...current, provider } : null;
}

function currentModelNameForControl(control: SessionConfigControl) {
  return currentProviderModelForControl(control)?.model ??
    control.current_value_label ??
    control.current_value_id;
}

function modelChoiceMatchesCurrentControl(
  choice: SessionConfigControl["choices"][number],
  control: SessionConfigControl,
  groupProvider: string | null,
) {
  if (choice.id === control.current_value_id || choice.label === control.current_value_label) {
    return true;
  }

  const currentModel = currentProviderModelForControl(control);
  if (!currentModel) return false;
  return modelChoiceMatchesProviderModel(choice, currentModel, groupProvider);
}

function modelChoiceMatchesProviderModel(
  choice: SessionConfigControl["choices"][number],
  model: { provider: string; model: string },
  groupProvider: string | null,
) {
  const choiceModel = decodeProviderModelValue(choice.id) ?? decodeProviderModelValue(choice.label);
  if (choiceModel) {
    const choiceProvider =
      concreteModelProviderForModel(choiceModel.provider, choiceModel.model) ??
      concreteModelProviderForModel(choice.provider, choiceModel.model);
    return choiceProvider === model.provider && choiceModel.model === model.model;
  }

  const choiceProvider =
    concreteModelProviderForModel(choice.provider, displayModelChoiceLabel(choice)) ??
    groupProvider;
  return (
    choiceProvider === model.provider &&
    (choice.id === model.model || choice.label === model.model)
  );
}

function modelChoiceMatchesModelName(
  choice: SessionConfigControl["choices"][number],
  modelName: string,
) {
  const choiceModel = decodeProviderModelValue(choice.id) ?? decodeProviderModelValue(choice.label);
  return choice.id === modelName || choice.label === modelName || choiceModel?.model === modelName;
}

function inferredProviderForModelName(modelName: string, groups: ModelProviderGroup[]) {
  const provider = inferredProviderIdForModelName(modelName);
  if (!provider) return null;
  const group = groups.find((candidate) => candidate.id === provider);
  if (!group) return null;
  return group.choices.some((choice) => modelChoiceMatchesModelName(choice, modelName))
    ? provider
    : null;
}

function inferredProviderIdForModelName(modelName: string) {
  const normalized = modelName.trim().toLowerCase();
  if (
    normalized.startsWith("qwen/") ||
    normalized.startsWith("minimaxai/") ||
    normalized.startsWith("moonshotai/") ||
    normalized.startsWith("zai-org/") ||
    normalized.startsWith("stepfun/") ||
    normalized.startsWith("google/") ||
    normalized.startsWith("openai/")
  ) {
    return "commandcode";
  }
  if (normalized.includes("deepseek")) return "deepseek";
  if (normalized.includes("kimi")) return "kimi_code";
  if (normalized.includes("mimo") || normalized.includes("xiaomi")) return "xiaomi_mimo";
  return null;
}

function concreteModelProviderForModel(
  provider: string | null | undefined,
  modelName: string,
) {
  const normalized = normalizeModelProviderId(provider);
  if (!normalized) return null;
  if (!GENERIC_MODEL_PROVIDER_IDS.has(normalized)) return normalized;
  return inferredProviderIdForModelName(modelName);
}

function normalizeModelProviderId(provider: string | null | undefined) {
  const normalized = provider?.trim().toLowerCase();
  if (!normalized) return null;
  switch (normalized) {
    case "timi":
    case "timi-ai":
    case "timi_ai":
      return "timiai";
    case "command-code":
    case "command_code":
      return "commandcode";
    case "kimi":
    case "kimi-code":
      return "kimi_code";
    case "mimo":
    case "xiaomi-mimo":
      return "xiaomi_mimo";
    case "codebuddy":
    case "code-buddy":
    case "code_buddy":
    case "codebuddy-proxy":
    case "codebuddy_proxy":
      return "codebuddy";
    default:
      return normalized;
  }
}

function decodeProviderModelValue(value: string) {
  const slashMatch = value.match(/^kodex-provider\/([^/]+)\/(.+)$/i);
  if (slashMatch) {
    const provider = normalizeModelProviderId(slashMatch[1]) ?? slashMatch[1];
    const model = slashMatch[2];
    if (provider === "byok") {
      const sourceMatch = model.match(/^([^/]+)\/(.+)$/);
      const sourceProvider = normalizeModelProviderId(sourceMatch?.[1]);
      if (sourceMatch && sourceProvider && isByokSourceModelProviderId(sourceProvider)) {
        return { provider: sourceProvider, model: sourceMatch[2] };
      }
    }
    return { provider, model };
  }

  const colonMatch = value.match(/^kodex-provider:([^:]+):(.+)$/i);
  if (colonMatch) {
    return { provider: normalizeModelProviderId(colonMatch[1]) ?? colonMatch[1], model: colonMatch[2] };
  }

  return null;
}


function displayModelValue(value: string) {
  return decodeProviderModelValue(value)?.model ?? value;
}

function displayModelChoiceLabel(choice: SessionConfigControl["choices"][number]) {
  const label = choice.label.trim();
  const labelModel = label ? decodeProviderModelValue(label)?.model : null;
  if (labelModel) return labelModel;
  if (label) return label;
  return displayModelValue(choice.id);
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
          displayName: file.name,
          mimeType: file.type || "application/octet-stream",
          data,
          text: null,
          uri: null,
          kind: isImage ? "image" : "file",
          path: null,
          startLine: null,
          endLine: null,
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

async function attachmentFromReference(
  request: ComposerReferenceRequest,
  workspaceRoot: string,
  imageInputEnabled: boolean,
): Promise<Attachment> {
  const range = referenceRange(request);
  const uri = workspaceFileUri(workspaceRoot, request.path, range);
  const name = referenceDisplayName(request.path, range);
  const displayName = referenceMentionDisplayName(request.path, range);

  if (!range && imageInputEnabled && isLikelyImagePath(request.path)) {
    const file = await editorGetContent(request.path);
    if ((file.kind ?? "text") !== "image") {
      return workspaceReferenceAttachment(request.path, range, uri, name, displayName);
    }
    const { data, mimeType } = parseDataUrl(file.content, file.mime_type ?? "application/octet-stream");
    const thumbnail = imageInputEnabled ? await createImageThumbnail(file.content).catch(() => null) : null;
    return {
      id: `ref:${uri}`,
      name,
      displayName,
      mimeType,
      data,
      text: null,
      uri,
      kind: imageInputEnabled ? "image" : "file",
      path: null,
      startLine: null,
      endLine: null,
      previewUrl: imageInputEnabled ? file.content : null,
      thumbnailData: thumbnail?.data ?? null,
      thumbnailMimeType: thumbnail?.mimeType ?? null,
    };
  }

  return workspaceReferenceAttachment(request.path, range, uri, name, displayName);
}

function workspaceReferenceAttachment(
  path: string,
  range: { startLine: number; endLine: number } | null,
  uri: string,
  name: string,
  displayName: string,
): Attachment {
  return {
    id: `ref:${uri}`,
    name,
    displayName,
    mimeType: mimeTypeForPath(path),
    data: null,
    text: null,
    uri,
    kind: "workspace_file",
    path,
    startLine: range?.startLine ?? null,
    endLine: range?.endLine ?? null,
    previewUrl: null,
    thumbnailData: null,
    thumbnailMimeType: null,
  };
}

function referenceRange(request: ComposerReferenceRequest) {
  const start = Number(request.startLine);
  const end = Number(request.endLine ?? request.startLine);
  if (!Number.isFinite(start) || start <= 0) return null;
  return {
    startLine: Math.floor(start),
    endLine: Number.isFinite(end) && end > 0 ? Math.floor(end) : Math.floor(start),
  };
}

function referenceDisplayName(
  path: string,
  range: { startLine: number; endLine: number } | null,
) {
  const filename = path.replace(/\\/g, "/").split("/").pop() || path;
  if (!range) return filename;
  return range.startLine === range.endLine
    ? `${filename}#L${range.startLine}`
    : `${filename}#L${range.startLine}-L${range.endLine}`;
}

function referenceMentionDisplayName(
  path: string,
  range: { startLine: number; endLine: number } | null,
) {
  const normalizedPath = path.replace(/\\/g, "/").replace(/^[\\/]+/, "");
  if (!range) return normalizedPath;
  return range.startLine === range.endLine
    ? `${normalizedPath}#L${range.startLine}`
    : `${normalizedPath}#L${range.startLine}-L${range.endLine}`;
}

function workspaceFileUri(
  workspaceRoot: string,
  path: string,
  range: { startLine: number; endLine: number } | null,
) {
  const root = workspaceRoot.replace(/[\\/]+$/, "");
  const relative = path.replace(/^[\\/]+/, "");
  const absolutePath = `${root}/${relative}`.replace(/\\/g, "/");
  const fragment = range
    ? `#L${range.startLine}${range.endLine !== range.startLine ? `-L${range.endLine}` : ""}`
    : "";
  return `${pathToFileUri(absolutePath)}${fragment}`;
}

function pathToFileUri(path: string) {
  let normalized = path.replace(/\\/g, "/");
  if (/^[A-Za-z]:\//.test(normalized)) {
    const [drive, ...segments] = normalized.split("/");
    return `file:///${drive}/${segments.map(encodeURIComponent).join("/")}`;
  }
  if (!normalized.startsWith("/")) {
    normalized = `/${normalized}`;
  }
  return `file://${normalized
    .split("/")
    .map((segment, index) => (index === 0 ? "" : encodeURIComponent(segment)))
    .join("/")}`;
}

function isLikelyImagePath(path: string) {
  return /\.(apng|avif|bmp|gif|jpe?g|png|webp)$/i.test(path);
}

function mimeTypeForPath(path: string) {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "md":
    case "mdx":
      return "text/markdown";
    case "html":
      return "text/html";
    case "css":
      return "text/css";
    case "json":
      return "application/json";
    case "ts":
    case "tsx":
    case "js":
    case "jsx":
    case "mjs":
    case "cjs":
      return "text/javascript";
    case "rs":
    case "py":
    case "toml":
    case "yaml":
    case "yml":
    case "txt":
      return "text/plain";
    default:
      return "text/plain";
  }
}

function parseDataUrl(dataUrl: string, fallbackMimeType: string) {
  const comma = dataUrl.indexOf(",");
  if (comma < 0) {
    return { data: dataUrl, mimeType: fallbackMimeType };
  }
  const header = dataUrl.slice(0, comma);
  const data = dataUrl.slice(comma + 1);
  const mimeType = header.match(/^data:([^;,]+)/)?.[1] ?? fallbackMimeType;
  return { data, mimeType };
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
  const rootRef = useRef<HTMLDivElement>(null);
  const choices = useMemo(() => dedupeControlChoices(control.choices), [control.choices]);

  useEffect(() => {
    if (!open) return;

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && rootRef.current?.contains(target)) return;
      onOpenChange(false);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onOpenChange(false);
      }
    };

    document.addEventListener("pointerdown", handlePointerDown, true);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown, true);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [onOpenChange, open]);

  if (choices.length === 0) return null;

  const unavailable = disabled || !control.enabled || pending;
  const selected =
    choices.find((choice) => choice.id === control.current_value_id) ??
    choices[0];
  const label = displayControlLabel(control);
  const selectedLabel = displayChoiceLabel(control, selected);

  return (
    <div
      ref={rootRef}
      className={`composer-control-select ${open ? "is-open" : ""}`}
      data-control-id={control.id}
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
        <span className="composer-control-value">{pending ? "更新中" : selectedLabel}</span>
        <span className="composer-control-chevron">
          <ChevronDownIcon />
        </span>
      </button>
      {open && !unavailable && (
        <div className="composer-control-menu" role="listbox">
          {choices.map((choice) => {
            const selectedChoice = choice.id === control.current_value_id;
            const choiceLabel = displayChoiceLabel(control, choice);
            return (
              <button
                key={`${choice.provider ?? ""}:${choice.id}`}
                className={`composer-control-option ${selectedChoice ? "is-selected" : ""}`}
                type="button"
                role="option"
                aria-selected={selectedChoice}
                title={choiceLabel === choice.label ? undefined : choice.label}
                onClick={() => {
                  onChange(control, choice.id);
                  onOpenChange(false);
                }}
              >
                <span>{choiceLabel}</span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

function SendIcon() {
  return (
    <svg className="composer-send-icon" viewBox="0 0 16 16" aria-hidden="true">
      <path d="M8 13V3" />
      <path d="M4.25 6.75 8 3l3.75 3.75" />
    </svg>
  );
}

function PaperclipIcon() {
  return (
    <svg className="composer-control-icon" viewBox="0 0 16 16" aria-hidden="true">
      <path d="m6.25 8.45 3.38-3.38a2.1 2.1 0 0 1 2.97 2.97l-4.24 4.24a3 3 0 0 1-4.24-4.24l4.6-4.6" />
    </svg>
  );
}

function ChevronDownIcon() {
  return (
    <svg className="composer-chevron-icon" viewBox="0 0 16 16" aria-hidden="true">
      <path d="m4.5 6.25 3.5 3.5 3.5-3.5" />
    </svg>
  );
}

function dedupeControlChoices(controlChoices: SessionConfigControl["choices"]) {
  const seen = new Set<string>();
  return controlChoices.filter((choice) => {
    const choiceId = choice.id.trim();
    if (!choiceId) return false;
    const id = `${choice.provider ?? ""}:${choiceId}`;
    if (seen.has(id)) return false;
    seen.add(id);
    return true;
  });
}

function displayControlLabel(control: SessionConfigControl) {
  if (control.category === "Model" || control.category === "Mode") return null;
  return control.label;
}

function displayChoiceLabel(
  control: SessionConfigControl,
  choice: SessionConfigControl["choices"][number],
) {
  if (control.category !== "Model") return choice.label;
  return displayModelChoiceLabel(choice);
}
