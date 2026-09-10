import { useState } from "react";
import { View, Text, TextInput, Pressable, Modal, ScrollView, StyleSheet } from "react-native";
import { useAppController, useSnapshot, usePendingApprovals } from "../../app/AppServicesContext";
import { isDestructive, allowOptionId } from "../../session/permission";
import type { PermissionInputResponse } from "../../types";
import { styles, colors, spacing, radius, shadows } from "../theme";

// Default-deny permission approval. The phone is the SOLE approval gate for
// destructive remote operations: no "allow" is preselected, and destructive
// ops require an explicit second confirmation before ResolvePermission is
// sent. Non-destructive (read-only) ops can be approved in one step.
//
// Harness `user_question` requests render in FULL: every question shows its
// options (radio for single-select, checkboxes for multi_select, each with
// its description) and `is_other`/free questions get a text input. Answers go
// out as `answers[question.id] = [...selectedLabels, customText?]` — the
// backend partitions label values into `selected` and the rest into `custom`.
// Harness option labels are English ("Submit"/"Cancel"); the action row
// displays them in Chinese.

const OPTION_LABEL_ZH: Record<string, string> = {
  submit: "提交",
  cancel: "取消",
  allow: "允许",
  deny: "拒绝",
};

function optionDisplayLabel(label: string): string {
  return OPTION_LABEL_ZH[label.trim().toLowerCase()] ?? label;
}

export function PermissionApprovalSheet() {
  const controller = useAppController();
  const snapshot = useSnapshot();
  const pending = usePendingApprovals();
  const [confirming, setConfirming] = useState<string | null>(null);
  const [picked, setPicked] = useState<string | null>(null);
  const [selected, setSelected] = useState<Record<string, string[]>>({});
  const [textInputs, setTextInputs] = useState<Record<string, string>>({});

  const toolById = new Map((snapshot?.tools ?? []).map((tool) => [tool.call_id, tool]));
  const approval = pending[0];

  if (!approval) return null;
  const tool = approval.toolCallId ? toolById.get(approval.toolCallId) : undefined;
  const destructive = tool ? isDestructive(tool) : true;
  const options = tool?.permission_options ?? [];
  const questions = approval.request?.questions ?? [];
  // A plain allow/deny approval (Codex's run/apply_patch prompt, Codebuddy's
  // bash prompt) has no questions: its choices ARE the tool's permission
  // options. They must be picked explicitly, because `option_id: null` reads as
  // a rejection on the PC — inferring one silently denies the call.
  const optionChoice = questions.length === 0 ? options : [];
  const allowOption = options.find(
    (option) => option.id === allowOptionId(options),
  );
  const effectiveOptionId =
    optionChoice.length > 0 ? picked : (allowOption?.id ?? null);
  const canConfirm =
    questions.length > 0 || optionChoice.length === 0 || picked !== null;

  const toggleOption = (questionId: string, label: string, multiSelect: boolean) => {
    setSelected((prev) => {
      const current = prev[questionId] ?? [];
      if (!multiSelect) {
        // Single-select: re-tapping the active option clears it.
        return { ...prev, [questionId]: current.includes(label) ? [] : [label] };
      }
      const next = current.includes(label)
        ? current.filter((value) => value !== label)
        : [...current, label];
      return { ...prev, [questionId]: next };
    });
  };

  const resolve = async (optionId: string | null) => {
    const answers: Record<string, string[]> = {};
    for (const question of questions) {
      const values = [...(selected[question.id] ?? [])];
      const custom = textInputs[question.id]?.trim();
      if (custom) values.push(custom);
      if (values.length > 0) answers[question.id] = values;
    }
    const inputResponse: PermissionInputResponse | null =
      Object.keys(answers).length > 0 ? { answers } : null;
    if (inputResponse) {
      await controller.approvePermissionWithInput(approval.permissionRequestId, optionId, null, inputResponse);
    } else {
      await controller.approvePermission(approval.permissionRequestId, optionId);
    }
    setConfirming(null);
    setPicked(null);
    setSelected({});
    setTextInputs({});
  };

  const requireConfirm = destructive && confirming !== approval.permissionRequestId;

  return (
    <Modal visible transparent animationType="slide" onRequestClose={() => controller.denyPermission(approval.permissionRequestId)}>
      <View style={{ flex: 1, justifyContent: "flex-end", backgroundColor: colors.scrim }}>
        <View style={{ backgroundColor: colors.surface, borderTopLeftRadius: radius.xl, borderTopRightRadius: radius.xl, maxHeight: "85%", borderWidth: 1, borderColor: colors.border, ...shadows.raised }}>
          <View style={{ alignSelf: "center", width: 40, height: 5, borderRadius: 3, backgroundColor: colors.borderStrong, marginTop: spacing.sm }} />
          <ScrollView contentContainerStyle={{ padding: spacing.lg, paddingTop: spacing.md }}>
            <View style={styles.rowBetween}>
              <Text style={[styles.text, { fontWeight: "800", fontSize: 17 }]}>请求授权</Text>
              {destructive ? (
                <View style={[styles.chip, { backgroundColor: colors.dangerTint, borderColor: colors.danger }]}>
                  <Text style={{ color: colors.danger, fontSize: 10, fontWeight: "700", textTransform: "uppercase", letterSpacing: 0.5 }}>Destructive</Text>
                </View>
              ) : null}
            </View>

            {tool ? (
              <>
                <View style={[styles.row, { marginTop: spacing.md }]}>
                  <Text style={[styles.mono, { color: colors.accent, fontSize: 12 }]}>{tool.name}</Text>
                </View>
                {tool.summary ? <Text style={[styles.text, { marginTop: spacing.xs, fontSize: 14, lineHeight: 20 }]}>{tool.summary}</Text> : null}
                {tool.detail_text ? <Text style={[styles.textDim, { marginTop: spacing.xs }]}>{tool.detail_text}</Text> : null}
              </>
            ) : (
              <Text style={[styles.textDim, { marginTop: spacing.sm }]}>{approval.toolName}</Text>
            )}

            {questions.map((question) => {
              const chosen = selected[question.id] ?? [];
              return (
                <View key={question.id} style={{ marginTop: spacing.md }}>
                  <Text style={[styles.text, { fontSize: 13, fontWeight: "600", lineHeight: 19 }]}>
                    {question.question}
                  </Text>
                  {question.options.length > 0 ? (
                    <View style={{ marginTop: spacing.xs }}>
                      {question.options.map((option) => {
                        const checked = chosen.includes(option.label);
                        return (
                          <Pressable
                            key={option.label}
                            onPress={() => toggleOption(question.id, option.label, question.multi_select)}
                            style={({ pressed }) => [
                              sheetStyles.optionRow,
                              pressed ? sheetStyles.optionRowPressed : null,
                              checked ? sheetStyles.optionRowChecked : null,
                            ]}
                            accessibilityRole={question.multi_select ? "checkbox" : "radio"}
                            accessibilityState={{ checked }}
                          >
                            <View
                              style={[
                                sheetStyles.check,
                                checked ? (question.multi_select ? sheetStyles.checkMulti : sheetStyles.checkRadio) : null,
                              ]}
                            >
                              {checked ? <View style={sheetStyles.checkDot} /> : null}
                            </View>
                            <View style={{ flex: 1, minWidth: 0 }}>
                              <Text style={[styles.text, { fontSize: 13, fontWeight: "600" }]}>{option.label}</Text>
                              {option.description ? (
                                <Text style={[styles.textDim, { fontSize: 11, lineHeight: 16, marginTop: 2 }]}>
                                  {option.description}
                                </Text>
                              ) : null}
                            </View>
                          </Pressable>
                        );
                      })}
                    </View>
                  ) : null}
                  {question.is_other || question.options.length === 0 ? (
                    <TextInput
                      style={[styles.input, question.options.length > 0 ? { marginTop: spacing.xs } : { marginTop: spacing.xs + 2 }]}
                      placeholder={question.is_secret ? "输入密钥…" : "或输入自定义回答…"}
                      placeholderTextColor={colors.textFaint}
                      secureTextEntry={question.is_secret}
                      value={textInputs[question.id] ?? ""}
                      onChangeText={(value) => setTextInputs((prev) => ({ ...prev, [question.id]: value }))}
                    />
                  ) : null}
                  {question.multi_select && question.options.length > 0 ? (
                    <Text style={[styles.textFaint, { fontSize: 10, marginTop: 4 }]}>可多选</Text>
                  ) : null}
                </View>
              );
            })}

            {requireConfirm ? (
              <View style={{ marginTop: spacing.lg }}>
                <View style={[styles.chip, { backgroundColor: colors.dangerTint, borderColor: colors.danger, alignSelf: "flex-start" }]}>
                  <Text style={{ color: colors.danger, fontSize: 12 }}>此操作可能修改你的工作区,确认后放行。</Text>
                </View>
                {optionChoice.length > 0 ? (
                  <View style={{ marginTop: spacing.sm }}>
                    {optionChoice.map((option) => {
                      const isPicked = picked === option.id;
                      return (
                        <Pressable
                          key={option.id}
                          onPress={() => setPicked(option.id)}
                          style={({ pressed }) => [
                            sheetStyles.optionRow,
                            pressed ? sheetStyles.optionRowPressed : null,
                            isPicked ? sheetStyles.optionRowChecked : null,
                          ]}
                          accessibilityRole="radio"
                          accessibilityState={{ checked: isPicked }}
                        >
                          <View style={[sheetStyles.check, isPicked ? sheetStyles.checkRadio : null]}>
                            {isPicked ? <View style={sheetStyles.checkDot} /> : null}
                          </View>
                          <View style={{ flex: 1, minWidth: 0 }}>
                            <Text style={[styles.text, { fontSize: 13, fontWeight: "600" }]}>
                              {optionDisplayLabel(option.label)}
                            </Text>
                          </View>
                        </Pressable>
                      );
                    })}
                  </View>
                ) : null}
                <View style={[styles.row, { marginTop: spacing.md }]}>
                  <Pressable
                    disabled={!canConfirm}
                    style={({ pressed }) => [
                      styles.buttonDanger,
                      { flex: 1, marginRight: spacing.sm, opacity: !canConfirm ? 0.5 : pressed ? 0.9 : 1 },
                    ]}
                    onPress={() => resolve(effectiveOptionId)}
                  >
                    <Text style={styles.buttonText}>确认放行</Text>
                  </Pressable>
                  <Pressable
                    style={({ pressed }) => [styles.buttonGhost, { flex: 1, opacity: pressed ? 0.7 : 1 }]}
                    onPress={() => controller.denyPermission(approval.permissionRequestId)}
                  >
                    <Text style={[styles.text, { fontWeight: "600" }]}>拒绝</Text>
                  </Pressable>
                </View>
                {!canConfirm ? (
                  <Text style={[styles.textFaint, { fontSize: 11, marginTop: spacing.xs }]}>
                    请先选择要放行的选项。
                  </Text>
                ) : null}
              </View>
            ) : (
              <View style={[styles.row, { flexWrap: "wrap", marginTop: spacing.lg }]}>
                {options.map((option) => {
                  const isDeny = /deny|no|cancel|block|reject/i.test(option.id + option.label);
                  const isPrimary = option.id === allowOption?.id;
                  return (
                    <Pressable
                      key={option.id}
                      style={({ pressed }) => [
                        isPrimary ? styles.buttonDanger : styles.buttonGhost,
                        { marginRight: spacing.xs, marginBottom: spacing.xs, paddingVertical: spacing.sm + 1, opacity: pressed ? 0.85 : 1 },
                      ]}
                      onPress={() => (destructive && isPrimary ? setConfirming(approval.permissionRequestId) : resolve(option.id))}
                    >
                      <Text style={[styles.text, { fontWeight: "600" }, isPrimary && { color: "#fff" }]}>
                        {optionDisplayLabel(option.label)}
                      </Text>
                    </Pressable>
                  );
                })}
                <Pressable
                  style={({ pressed }) => [styles.buttonGhost, { paddingVertical: spacing.sm + 1, opacity: pressed ? 0.7 : 1 }]}
                  onPress={() => controller.denyPermission(approval.permissionRequestId)}
                >
                  <Text style={[styles.text, { color: colors.danger, fontWeight: "600" }]}>拒绝</Text>
                </Pressable>
              </View>
            )}
          </ScrollView>
        </View>
      </View>
    </Modal>
  );
}

const sheetStyles = StyleSheet.create({
  optionRow: {
    flexDirection: "row",
    alignItems: "flex-start",
    gap: spacing.sm,
    paddingVertical: spacing.xs + 1,
    paddingHorizontal: spacing.sm,
    borderRadius: 8,
    borderWidth: 1,
    borderColor: colors.border,
    marginTop: spacing.xs,
  },
  optionRowPressed: {
    backgroundColor: colors.surfaceAlt,
  },
  optionRowChecked: {
    borderColor: colors.accent,
    backgroundColor: colors.surfaceAlt,
  },
  check: {
    width: 16,
    height: 16,
    borderRadius: 8,
    borderWidth: 1.5,
    borderColor: colors.borderStrong,
    marginTop: 2,
    alignItems: "center",
    justifyContent: "center",
  },
  checkMulti: {
    borderRadius: 4,
  },
  checkRadio: {
    borderRadius: 8,
  },
  checkDot: {
    width: 8,
    height: 8,
    borderRadius: 4,
    backgroundColor: colors.accent,
  },
});
// end of file
