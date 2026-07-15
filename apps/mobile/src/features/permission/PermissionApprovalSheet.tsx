import { useState } from "react";
import { View, Text, TextInput, Pressable, Modal, ScrollView } from "react-native";
import { useAppController, useSnapshot, usePendingApprovals } from "../../app/AppServicesContext";
import { isDestructive } from "../../session/permission";
import type { PermissionInputResponse } from "../../types";
import { styles, colors, spacing } from "../theme";

// Default-deny permission approval. The phone is the SOLE approval gate for
// destructive remote operations: no "allow" is preselected, and destructive
// ops require an explicit second confirmation before ResolvePermission is
// sent. Non-destructive (read-only) ops can be approved in one step.
export function PermissionApprovalSheet() {
  const controller = useAppController();
  const snapshot = useSnapshot();
  const pending = usePendingApprovals();
  const [confirming, setConfirming] = useState<string | null>(null);
  const [textInputs, setTextInputs] = useState<Record<string, string>>({});

  const toolById = new Map((snapshot?.tools ?? []).map((tool) => [tool.call_id, tool]));
  const approval = pending[0];

  if (!approval) return null;
  const tool = approval.toolCallId ? toolById.get(approval.toolCallId) : undefined;
  const destructive = tool ? isDestructive(tool) : true;
  const options = tool?.permission_options ?? [];
  const inputQuestions = (approval.request?.questions ?? []).filter((q) => q.is_secret || q.is_other);

  const resolve = async (optionId: string | null) => {
    const answers: Record<string, string[]> = {};
    for (const question of inputQuestions) {
      const value = textInputs[question.id]?.trim();
      if (value) answers[question.id] = [value];
    }
    const inputResponse: PermissionInputResponse | null =
      Object.keys(answers).length > 0 ? { answers } : null;
    if (inputResponse) {
      await controller.approvePermissionWithInput(approval.permissionRequestId, optionId, null, inputResponse);
    } else {
      await controller.approvePermission(approval.permissionRequestId, optionId);
    }
    setConfirming(null);
    setTextInputs({});
  };

  const requireConfirm = destructive && confirming !== approval.permissionRequestId;
  const allowOption = options.find((option) => /allow|yes|approve|once/i.test(option.label));

  return (
    <Modal visible transparent animationType="slide" onRequestClose={() => controller.denyPermission(approval.permissionRequestId)}>
      <View style={{ flex: 1, justifyContent: "flex-end", backgroundColor: "rgba(0,0,0,0.6)" }}>
        <View style={{ backgroundColor: colors.surface, borderTopLeftRadius: 16, borderTopRightRadius: 16, maxHeight: "85%" }}>
          <ScrollView contentContainerStyle={{ padding: spacing.md }}>
            <View style={styles.rowBetween}>
              <Text style={[styles.text, { fontWeight: "700" }]}>Permission requested</Text>
              {destructive ? (
                <Text style={[styles.badge, { backgroundColor: colors.danger }]}>
                  <Text style={{ color: "#fff" }}>Destructive</Text>
                </Text>
              ) : null}
            </View>

            {tool ? (
              <>
                <Text style={[styles.mono, { marginTop: spacing.sm, color: colors.textDim }]}>{tool.name}</Text>
                {tool.summary ? <Text style={[styles.text, { marginTop: spacing.xs }]}>{tool.summary}</Text> : null}
                {tool.detail_text ? <Text style={[styles.textDim, { marginTop: spacing.xs }]}>{tool.detail_text}</Text> : null}
              </>
            ) : (
              <Text style={[styles.textDim, { marginTop: spacing.sm }]}>{approval.toolName}</Text>
            )}

            {inputQuestions.length > 0 ? (
              <View style={{ marginTop: spacing.md }}>
                {inputQuestions.map((question) => (
                  <View key={question.id} style={{ marginTop: spacing.xs }}>
                    <Text style={[styles.text, { fontSize: 13 }]}>{question.question}</Text>
                    <TextInput
                      style={[styles.input, { marginTop: spacing.xs }]}
                      placeholder={question.is_secret ? "secret input" : "free text"}
                      placeholderTextColor={colors.textDim}
                      secureTextEntry={question.is_secret}
                      value={textInputs[question.id] ?? ""}
                      onChangeText={(value) => setTextInputs((prev) => ({ ...prev, [question.id]: value }))}
                    />
                  </View>
                ))}
              </View>
            ) : null}

            {requireConfirm ? (
              <View style={{ marginTop: spacing.md }}>
                <Text style={[styles.text, { color: colors.danger, textAlign: "center" }]}>
                  This operation can modify your workspace. Confirm to allow.
                </Text>
                <View style={[styles.row, { marginTop: spacing.sm }]}>
                  <Pressable style={[styles.buttonDanger, { flex: 1, marginRight: spacing.xs }]} onPress={() => resolve(allowOption?.id ?? null)}>
                    <Text style={styles.buttonText}>Confirm allow</Text>
                  </Pressable>
                  <Pressable style={[styles.buttonGhost, { flex: 1 }]} onPress={() => controller.denyPermission(approval.permissionRequestId)}>
                    <Text style={styles.text}>Deny</Text>
                  </Pressable>
                </View>
              </View>
            ) : (
              <View style={[styles.row, { flexWrap: "wrap", marginTop: spacing.md }]}>
                {options.map((option) => {
                  const isAllow = /allow|yes|approve|once/i.test(option.label);
                  const isDeny = /deny|no|cancel|block/i.test(option.label);
                  return (
                    <Pressable
                      key={option.id}
                      style={[
                        isDeny ? styles.buttonDanger : styles.buttonGhost,
                        { marginRight: spacing.xs, marginBottom: spacing.xs, paddingVertical: spacing.sm },
                      ]}
                      onPress={() => (destructive && isAllow ? setConfirming(approval.permissionRequestId) : resolve(option.id))}
                    >
                      <Text style={[styles.text, isDeny && { color: "#fff" }]}>{option.label}</Text>
                    </Pressable>
                  );
                })}
                <Pressable style={[styles.buttonGhost, { paddingVertical: spacing.sm }]} onPress={() => controller.denyPermission(approval.permissionRequestId)}>
                  <Text style={[styles.text, { color: colors.danger }]}>Deny</Text>
                </Pressable>
              </View>
            )}
          </ScrollView>
        </View>
      </View>
    </Modal>
  );
}
// end of file
