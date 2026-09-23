import { useState } from "react";
import {
  View,
  Text,
  Image,
  TextInput,
  Pressable,
  ActivityIndicator,
  Modal,
  ScrollView,
  Vibration,
  StyleSheet,
} from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import * as ImagePicker from "expo-image-picker";
import type { SessionConfigChoice, SessionConfigControl, UserPromptContent } from "../../types";
import { colors, spacing, radius } from "../theme";

interface PickedImage {
  id: string;
  /** 相册返回的本地 URI，仅用于缩略图预览。 */
  uri: string;
  base64: string;
  mimeType: string;
  name: string;
}

let pickedImageSeq = 0;

function toPromptImage(image: PickedImage): UserPromptContent {
  return {
    type: "image",
    data: image.base64,
    mime_type: image.mimeType,
    name: image.name,
    display_url: null,
    thumbnail_data: null,
    thumbnail_mime_type: null,
  };
}

interface Props {
  onSend: (content: UserPromptContent[]) => void | Promise<void>;
  disabled?: boolean;
  error?: string | null;
  /** True while the session is mid-turn (Streaming / WaitingForTool): shows
   * a stop button next to send. Sending stays available so the turn can be
   * steered without stopping it first. */
  streaming?: boolean;
  onCancel?: () => void | Promise<void>;
  /** The session's model config control (from the snapshot's
   *  `session_config`), when the PC hydrated one. Renders the model pill
   *  above the input; null/absent hides it (e.g. agents without a model
   *  picker). */
  modelControl?: SessionConfigControl | null;
  /** Apply a model choice through the PC's `set_config_control` path. The
   *  refreshed config arrives back as a snapshot patch; a thrown error is
   *  shown inside the picker. */
  onSelectModel?: (
    controlId: string,
    valueId: string,
    provider: string | null,
  ) => void | Promise<void>;
  /** 会话支持图片输入（`prompt_capabilities.image`）时显示附件入口。 */
  imageCapable?: boolean;
}

// Prompt input + send (+ stop while a turn is running) + image attach（相册
// 选图，随消息以 base64 图片块发送；不支持图片的会话不显示入口）。
//
// No keyboard math lives here on purpose: the conversation screen owns a single
// measured lift for the whole column (see useKeyboardAvoidance), so a second,
// platform-specific pad here used to stack on top of Android's `adjustResize`
// and float the input a keyboard away from the keyboard.
export function Composer({
  onSend,
  disabled,
  error,
  streaming,
  onCancel,
  modelControl,
  onSelectModel,
  imageCapable,
}: Props) {
  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<PickedImage[]>([]);
  const [attachError, setAttachError] = useState<string | null>(null);
  const [providerStep, setProviderStep] = useState<string | null>(null);
  // 输入态聚焦时只保留输入框 + 发送：⋮ / ＋ / 停止 都让位，输入框独占整行，
  // 打字不再被一圈按钮挤到。
  const [inputFocused, setInputFocused] = useState(false);
  const [sending, setSending] = useState(false);
  const [canceling, setCanceling] = useState(false);
  const [inputHeight, setInputHeight] = useState(38);
  const [modelPickerOpen, setModelPickerOpen] = useState(false);
  const [modelError, setModelError] = useState<string | null>(null);
  const [switchingModel, setSwitchingModel] = useState(false);
  const insets = useSafeAreaInsets();

  const canSend =
    (text.trim().length > 0 || attachments.length > 0) && !disabled && !sending;

  // The PC rejects config changes outside Idle ("会话控件只能在会话空闲时
  // 更改"), so the pill is inert while a turn runs — same gating as the
  // desktop composer.
  const modelSwitchable = !!modelControl?.enabled && !streaming && !!onSelectModel;

  const handleSelectModel = async (valueId: string, provider: string | null) => {
    if (!modelControl || !onSelectModel || switchingModel) return;
    setSwitchingModel(true);
    setModelError(null);
    try {
      await onSelectModel(modelControl.id, valueId, provider);
      setModelPickerOpen(false);
    } catch (e) {
      setModelError(e instanceof Error ? e.message : String(e));
    } finally {
      setSwitchingModel(false);
    }
  };

  const handleSend = async () => {
    if (!canSend) return;
    const value = text.trim();
    const content: UserPromptContent[] = [
      ...attachments.map(toPromptImage),
      ...(value ? [{ type: "text", text: value } as UserPromptContent] : []),
    ];
    setSending(true);
    try {
      await onSend(content);
      Vibration.vibrate(8);
      setText("");
      setAttachments([]);
    } catch {
      // The parent surfaces the error and keeps the input so the user can retry.
    } finally {
      setSending(false);
    }
  };

  const handleAttach = async () => {
    if (!imageCapable || disabled) return;
    setAttachError(null);
    try {
      const permission = await ImagePicker.requestMediaLibraryPermissionsAsync();
      if (!permission.granted) {
        setAttachError("需要相册权限才能添加图片");
        return;
      }
      const result = await ImagePicker.launchImageLibraryAsync({
        mediaTypes: ["images"],
        base64: true,
        quality: 0.8,
      });
      if (result.canceled || result.assets.length === 0) return;
      const asset = result.assets[0];
      const base64 = asset.base64;
      if (!base64) {
        setAttachError("读取图片数据失败，请重试");
        return;
      }
      pickedImageSeq += 1;
      setAttachments((current) => [
        ...current,
        {
          id: `picked-${pickedImageSeq}`,
          uri: asset.uri,
          base64,
          mimeType: asset.mimeType ?? "image/png",
          name: asset.fileName ?? `image-${pickedImageSeq}.png`,
        },
      ]);
    } catch (e) {
      setAttachError(e instanceof Error ? e.message : String(e));
    }
  };

  const handleCancel = async () => {
    if (!onCancel || !streaming || canceling) return;
    setCanceling(true);
    try {
      await onCancel();
    } finally {
      setCanceling(false);
    }
  };

  // 级联菜单数据：第一层 Provider（按 choice.provider 归组），第二层该
  // Provider 的模型。
  const providerGroups: { key: string; label: string; choices: SessionConfigChoice[] }[] = [];
  for (const choice of modelControl?.choices ?? []) {
    const key = choice.provider ?? "";
    let group = providerGroups.find((entry) => entry.key === key);
    if (!group) {
      group = {
        key,
        label: choice.provider_label ?? choice.provider ?? "默认",
        choices: [],
      };
      providerGroups.push(group);
    }
    group.choices.push(choice);
  }
  const currentChoice = (modelControl?.choices ?? []).find(
    (choice) =>
      choice.id === modelControl?.current_value_id ||
      choice.label === modelControl?.current_value_label,
  );
  const currentModelText = modelControl
    ? modelControl.current_value_label +
      (currentChoice?.provider_label ? `（${currentChoice.provider_label}）` : "")
    : "";

  return (
    <View
      style={{
        backgroundColor: colors.bg,
        paddingBottom: insets.bottom > 0 ? 0 : 8,
      }}
    >
      {attachments.length > 0 ? (
        <View style={composerStyles.attachStrip}>
          {attachments.map((image) => (
            <View key={image.id} style={composerStyles.attachChip}>
              <Image source={{ uri: image.uri }} style={composerStyles.attachThumb} />
              <Pressable
                onPress={() =>
                  setAttachments((current) => current.filter((entry) => entry.id !== image.id))
                }
                accessibilityRole="button"
                accessibilityLabel={`移除图片 ${image.name}`}
                hitSlop={8}
              >
                <Text style={composerStyles.attachRemove}>{"\u00d7"}</Text>
              </Pressable>
            </View>
          ))}
        </View>
      ) : null}
      {attachError ? (
        <Text style={composerStyles.error} numberOfLines={2}>
          {attachError}
        </Text>
      ) : null}
      {error ? (
        <Text style={composerStyles.error} numberOfLines={2}>
          {error}
        </Text>
      ) : null}
      <View style={composerStyles.row}>
        {modelControl && !inputFocused ? (
          <Pressable
            style={({ pressed }) => [
              composerStyles.circle,
              composerStyles.circleAttach,
              { opacity: pressed ? 0.7 : 1 },
            ]}
            onPress={() => {
              setProviderStep(null);
              setModelError(null);
              setModelPickerOpen(true);
            }}
            accessibilityRole="button"
            accessibilityLabel={`切换模型，当前 ${modelControl.current_value_label}`}
          >
            <Text style={composerStyles.dotsGlyph}>{"\u22ee"}</Text>
          </Pressable>
        ) : null}
        {imageCapable && !inputFocused ? (
          <Pressable
            style={({ pressed }) => [
              composerStyles.circle,
              composerStyles.circleAttach,
              { opacity: disabled ? 0.5 : pressed ? 0.7 : 1 },
            ]}
            disabled={disabled}
            onPress={() => void handleAttach()}
            accessibilityRole="button"
            accessibilityLabel="添加图片"
          >
            <Text style={composerStyles.attachPlus}>{"\uff0b"}</Text>
          </Pressable>
        ) : null}
        <TextInput
          style={[
            composerStyles.input,
            {
              height: Math.max(44, Math.min(inputHeight + 8, 140)),
              borderRadius: inputHeight <= 46 ? 22 : radius.lg,
            },
          ]}
          placeholder={"给智能体发消息\u2026"}
          placeholderTextColor={colors.textFaint}
          value={text}
          onChangeText={setText}
          onFocus={() => setInputFocused(true)}
          onBlur={() => setInputFocused(false)}
          onContentSizeChange={(event) => setInputHeight(event.nativeEvent.contentSize.height)}
          multiline
          editable={!disabled}
        />
        {streaming && onCancel && !inputFocused ? (
          <Pressable
            style={({ pressed }) => [composerStyles.circle, composerStyles.circleStop, { opacity: pressed ? 0.8 : 1 }]}
            disabled={canceling}
            onPress={handleCancel}
            accessibilityRole="button"
            accessibilityLabel="停止智能体"
          >
            {canceling ? (
              <ActivityIndicator color={colors.text} size="small" />
            ) : (
              <View style={composerStyles.stopSquare} />
            )}
          </Pressable>
        ) : null}
        <Pressable
          style={({ pressed }) => [
            composerStyles.circle,
            canSend ? composerStyles.circleSend : composerStyles.circleDisabled,
            { opacity: pressed && canSend ? 0.85 : 1 },
          ]}
          disabled={!canSend}
          onPress={handleSend}
          accessibilityRole="button"
          accessibilityLabel="发送消息"
        >
          {sending ? (
            <ActivityIndicator color={colors.bg} size="small" />
          ) : (
            <Text style={[composerStyles.arrow, { color: canSend ? colors.bg : colors.textFaint }]}>{"\u2191"}</Text>
          )}
        </Pressable>
      </View>

      <Modal
        visible={modelPickerOpen}
        transparent
        animationType="fade"
        onRequestClose={() => {
          if (!switchingModel) setModelPickerOpen(false);
        }}
      >
        <View style={composerStyles.pickerBackdrop}>
          <Pressable
            style={StyleSheet.absoluteFill}
            onPress={() => {
              if (!switchingModel) setModelPickerOpen(false);
            }}
            accessibilityLabel="关闭模型选择"
          />
          <View style={composerStyles.pickerCard}>
            <Text style={composerStyles.pickerTitle}>切换模型</Text>
            {/* 顶部展示当前选择：模型名（provider 名） */}
            {currentModelText ? (
              <Text style={composerStyles.pickerCurrent} numberOfLines={1}>
                {currentModelText}
              </Text>
            ) : null}
            <ScrollView nestedScrollEnabled showsVerticalScrollIndicator={false}>
              {providerStep === null ? (
                // 级联第一层：Provider
                providerGroups.map((group) => {
                  const isCurrentGroup =
                    (currentChoice?.provider ?? "") === group.key ||
                    (!currentChoice && group.key === "");
                  return (
                    <Pressable
                      key={group.key}
                      style={({ pressed }) => [
                        composerStyles.pickerRow,
                        isCurrentGroup && composerStyles.pickerRowCurrent,
                        { opacity: pressed && !switchingModel ? 0.7 : 1 },
                      ]}
                      disabled={switchingModel}
                      onPress={() => setProviderStep(group.key)}
                      accessibilityRole="button"
                      accessibilityLabel={`选择提供商 ${group.label}`}
                    >
                      <View style={{ flex: 1, minWidth: 0 }}>
                        <Text
                          style={[
                            composerStyles.pickerRowLabel,
                            isCurrentGroup && { color: colors.accentBright },
                          ]}
                          numberOfLines={1}
                        >
                          {group.label}
                        </Text>
                        <Text style={composerStyles.pickerRowMeta} numberOfLines={1}>
                          {`${group.choices.length} 个模型`}
                        </Text>
                      </View>
                      <Text style={composerStyles.pickerRowCheck}>
                        {isCurrentGroup ? "\u2713" : "\u203a"}
                      </Text>
                    </Pressable>
                  );
                })
              ) : (
                // 级联第二层：该 Provider 的模型
                (() => {
                  const group = providerGroups.find((entry) => entry.key === providerStep);
                  return (
                    <>
                      <Pressable
                        style={({ pressed }) => [
                          composerStyles.pickerRow,
                          { opacity: pressed && !switchingModel ? 0.7 : 1 },
                        ]}
                        disabled={switchingModel}
                        onPress={() => setProviderStep(null)}
                        accessibilityRole="button"
                        accessibilityLabel="返回提供商列表"
                      >
                        <Text style={composerStyles.pickerRowLabel}>{"\u2039 返回"}</Text>
                      </Pressable>
                      <Text style={composerStyles.pickerGroupLabel}>
                        {group?.label ?? providerStep}
                      </Text>
                      {(group?.choices ?? []).map((choice) => {
                        const current =
                          choice.id === modelControl?.current_value_id ||
                          choice.label === modelControl?.current_value_label;
                        return (
                          <Pressable
                            key={`${choice.provider ?? ""}:${choice.id}`}
                            style={({ pressed }) => [
                              composerStyles.pickerRow,
                              current && composerStyles.pickerRowCurrent,
                              { opacity: pressed && !switchingModel ? 0.7 : 1 },
                            ]}
                            disabled={switchingModel}
                            onPress={() => void handleSelectModel(choice.id, choice.provider ?? null)}
                            accessibilityRole="radio"
                            accessibilityState={{ selected: current }}
                          >
                            <View style={{ flex: 1, minWidth: 0 }}>
                              <Text
                                style={[
                                  composerStyles.pickerRowLabel,
                                  current && { color: colors.accentBright },
                                ]}
                                numberOfLines={1}
                              >
                                {choice.label}
                              </Text>
                            </View>
                            <Text style={composerStyles.pickerRowCheck}>
                              {current ? "\u2713" : ""}
                            </Text>
                          </Pressable>
                        );
                      })}
                    </>
                  );
                })()
              )}
              {!modelSwitchable ? (
                <Text style={composerStyles.pickerRowMeta}>{"会话空闲时才能更改"}</Text>
              ) : null}
              {modelError ? <Text style={composerStyles.error}>{modelError}</Text> : null}
            </ScrollView>
          </View>
        </View>
      </Modal>
    </View>
  );
}

// Chat-style composer: one filled rounded field plus two circular buttons.
// No top divider and no boxed input — the field's fill already separates it
// from the timeline, and a border on top of that read as heavy chrome.
const composerStyles = StyleSheet.create({
  row: {
    flexDirection: "row",
    alignItems: "flex-end",
    paddingHorizontal: spacing.md,
    paddingTop: spacing.sm,
    gap: spacing.sm,
  },
  input: {
    flex: 1,
    color: colors.text,
    backgroundColor: colors.surfaceAlt,
    paddingHorizontal: spacing.lg,
    paddingTop: spacing.sm + 2,
    paddingBottom: spacing.sm + 2,
    fontSize: 15,
  },
  circle: {
    width: 44,
    height: 44,
    borderRadius: 22,
    alignItems: "center",
    justifyContent: "center",
  },
  circleSend: { backgroundColor: colors.text },
  circleStop: { backgroundColor: colors.surfaceRaised },
  circleDisabled: { backgroundColor: colors.surfaceAlt },
  // 模型入口：输入行里的 ⋮ 三点按钮（不再占一整行）。弹层顶部展示当前
  // 模型名（provider 名），主体是 Provider → 模型 的两级级联。
  dotsGlyph: {
    color: colors.textDim,
    fontSize: 20,
    lineHeight: 22,
    fontWeight: "700",
  },
  pickerCurrent: {
    color: colors.accentBright,
    fontSize: 13,
    fontWeight: "600",
    marginBottom: spacing.sm,
  },
  pickerGroupLabel: {
    color: colors.textFaint,
    fontSize: 12,
    fontWeight: "600",
    paddingHorizontal: spacing.md,
    paddingTop: spacing.sm,
  },
  pickerBackdrop: {
    flex: 1,
    backgroundColor: colors.scrim,
    alignItems: "center",
    justifyContent: "center",
    padding: spacing.lg,
  },
  pickerCard: {
    width: "100%",
    maxWidth: 420,
    maxHeight: "70%",
    backgroundColor: colors.surfaceRaised,
    borderRadius: radius.xl,
    padding: spacing.lg,
  },
  pickerTitle: {
    color: colors.text,
    fontSize: 16,
    fontWeight: "800",
    marginBottom: spacing.md,
  },
  pickerRow: {
    flexDirection: "row",
    alignItems: "center",
    paddingVertical: spacing.sm + 2,
    paddingHorizontal: spacing.md,
    borderRadius: radius.md,
    gap: spacing.md,
  },
  pickerRowCurrent: {
    backgroundColor: colors.surfaceAlt,
  },
  pickerRowLabel: {
    color: colors.text,
    fontSize: 14,
    fontWeight: "500",
  },
  pickerRowMeta: {
    color: colors.textFaint,
    fontSize: 12,
    marginTop: 2,
  },
  pickerRowCheck: {
    color: colors.textFaint,
    fontSize: 12,
    minWidth: 24,
    textAlign: "right",
  },
  arrow: { fontSize: 18, fontWeight: "700", lineHeight: 20 },
  stopSquare: { width: 12, height: 12, borderRadius: 3, backgroundColor: colors.text },
  attachStrip: {
    flexDirection: "row",
    flexWrap: "wrap",
    gap: spacing.sm,
    paddingHorizontal: spacing.md,
    paddingTop: spacing.sm,
  },
  attachChip: {
    flexDirection: "row",
    alignItems: "center",
    backgroundColor: colors.surfaceAlt,
    borderRadius: radius.md,
    paddingLeft: 4,
    paddingRight: spacing.sm,
    paddingVertical: 4,
    gap: spacing.xs,
  },
  attachThumb: {
    width: 40,
    height: 40,
    borderRadius: radius.md,
    backgroundColor: colors.surface,
  },
  attachRemove: {
    color: colors.textDim,
    fontSize: 18,
    lineHeight: 20,
    paddingHorizontal: 4,
  },
  circleAttach: {
    backgroundColor: colors.surfaceAlt,
    alignItems: "center",
    justifyContent: "center",
  },
  attachPlus: {
    color: colors.textDim,
    fontSize: 22,
    fontWeight: "600",
    lineHeight: 24,
  },
  error: {
    color: colors.danger,
    fontSize: 12,
    lineHeight: 17,
    paddingHorizontal: spacing.lg,
    paddingTop: spacing.sm,
  },
});
// end of file