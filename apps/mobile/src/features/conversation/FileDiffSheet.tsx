import { memo, useCallback, useEffect, useRef, useState } from "react";
import {
  Animated,
  Easing,
  Modal,
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  View,
  type TextStyle,
} from "react-native";
import type { SessionFileChange } from "../../types";
import type { AppController } from "../../app/services";
import {
  computeLineDiff,
  numberRows,
  segmentHunks,
  type NumberedDiffRow,
} from "../tooling/line-diff";
import { DIFF_CONTEXT_LINES } from "../tooling/compact-diff";
import { colors, spacing, radius } from "../theme";

// Full-screen diff viewer opened by tapping a file in the turn-changes bar —
// the mobile counterpart of the desktop review panel's multi-file diff:
//   • one collapsible section per file (tapped file open, others collapsed)
//   • per-file diffs fetched on demand (GetFileDiff control op) and cached
//   • by default only changed hunks render, each changed line surrounded by
//     DIFF_CONTEXT_LINES of context (overlapping windows merged)
//   • unchanged runs collapse into a gray "⋯ N 行未更改 ⋯" block that expands
//     in place when tapped
//   • two line-number gutters (old/new), unified +/- coloring

export interface FileDiffSection {
  path: string;
  changeType: string;
  addedLines: number;
  removedLines: number;
}

export interface FileDiffTurn {
  messageId: string;
  sections: FileDiffSection[];
  /** The file the user tapped — the only section open by default. */
  initialPath: string;
}

interface FileDiffSheetProps {
  turn: FileDiffTurn | null;
  controller: AppController;
  onClose: () => void;
}

interface FileViewState {
  change?: SessionFileChange | null;
  error?: string;
}

export const FileDiffSheet = memo(function FileDiffSheet({ turn, controller, onClose }: FileDiffSheetProps) {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [views, setViews] = useState<Map<string, FileViewState>>(new Map());
  const [expandedGaps, setExpandedGaps] = useState<Set<string>>(new Set());
  // Real monospace digit width (measured once, shared by every file) so the
  // number gutter exactly fits the digits instead of using a guessed metric.
  const [digitWidth, setDigitWidth] = useState(5.6);

  // Reset per-open state whenever a new turn is opened.
  useEffect(() => {
    if (!turn) return;
    setCollapsed(new Set(turn.sections.filter((s) => s.path !== turn.initialPath).map((s) => s.path)));
    setViews(new Map());
    setExpandedGaps(new Set());
  }, [turn]);

  const loadFile = useCallback(
    (messageId: string, path: string) => {
      setViews((prev) => {
        if (prev.has(path)) return prev; // cached or already loading
        const next = new Map(prev);
        next.set(path, {});
        return next;
      });
      controller
        .getFileDiff(messageId, path)
        .then((change) => {
          setViews((prev) => new Map(prev).set(path, { change: change ?? null }));
        })
        .catch((e: Error) => {
          setViews((prev) => new Map(prev).set(path, { error: e.message }));
        });
    },
    [controller],
  );

  // Fetch the initially-open file as soon as the sheet opens.
  useEffect(() => {
    if (turn) loadFile(turn.messageId, turn.initialPath);
  }, [turn, loadFile]);

  if (!turn) return null;
  const close = () => onClose();

  const toggleSection = (path: string) => {
    const wasCollapsed = collapsed.has(path);
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (wasCollapsed) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
    if (wasCollapsed) loadFile(turn.messageId, path);
  };

  const toggleGap = (key: string) => {
    setExpandedGaps((prev) => new Set(prev).add(key));
  };

  return (
    <Modal visible transparent animationType="slide" onRequestClose={close}>
      <View style={sheetStyles.screen}>
        {/* Digit-width calibrator: same font as the number cells, 8 digits so
            one-off kerning noise averages out. Invisible + absolute. */}
        <Text
          style={[diffStyles.calib, sheetStyles.offscreen]}
          onLayout={(e) => setDigitWidth(e.nativeEvent.layout.width / 8)}
        >
          00000000
        </Text>
        <View style={sheetStyles.header}>
          <Pressable onPress={close} hitSlop={8} accessibilityRole="button" accessibilityLabel="关闭 diff">
            <Text style={sheetStyles.back}>{"\u2190 返回"}</Text>
          </Pressable>
          <Text style={sheetStyles.headerTitle}>本轮改动</Text>
          <View style={{ width: 52 }} />
        </View>
        <ScrollView style={sheetStyles.body} nestedScrollEnabled>
          {turn.sections.map((section) => {
            const isCollapsed = collapsed.has(section.path);
            const view = views.get(section.path);
            return (
              <View key={section.path} style={sheetStyles.section}>
                <Pressable
                  style={({ pressed }) => [
                    sheetStyles.sectionHeader,
                    pressed ? sheetStyles.sectionHeaderPressed : null,
                  ]}
                  onPress={() => toggleSection(section.path)}
                  accessibilityRole="button"
                  accessibilityState={{ expanded: !isCollapsed }}
                  accessibilityLabel={`${isCollapsed ? "展开" : "折叠"} ${section.path}`}
                >
                  <Text style={[sheetStyles.chevron, isCollapsed ? null : sheetStyles.chevronOpen]}>
                    {"\u203A"}
                  </Text>
                  <MarqueeText text={section.path} style={sheetStyles.sectionPath} />
                  <Text style={[sheetStyles.count, { color: colors.success }]}>{`+${section.addedLines}`}</Text>
                  <Text style={[sheetStyles.count, { color: colors.danger }]}>{`\u2212${section.removedLines}`}</Text>
                </Pressable>
                {!isCollapsed ? (
                  <FileBody
                    view={view}
                    changeType={section.changeType}
                    pathKey={section.path}
                    expandedGaps={expandedGaps}
                    onToggleGap={toggleGap}
                    digitWidth={digitWidth}
                  />
                ) : null}
              </View>
            );
          })}
        </ScrollView>
      </View>
    </Modal>
  );
});

const FileBody = memo(function FileBody({
  view,
  changeType,
  pathKey,
  expandedGaps,
  onToggleGap,
  digitWidth,
}: {
  view: FileViewState | undefined;
  changeType: string;
  pathKey: string;
  expandedGaps: Set<string>;
  onToggleGap: (key: string) => void;
  digitWidth: number;
}) {
  if (!view || view.change === undefined) {
    return <Text style={bodyStyles.placeholder}>加载 diff 中…</Text>;
  }
  if (view.error) {
    return <Text style={[bodyStyles.placeholder, { color: colors.danger }]}>{`加载失败：${view.error}`}</Text>;
  }
  if (!view.change) {
    return <Text style={bodyStyles.placeholder}>该改动已不在桌面端的实时窗口内(桌面端重启后仅保留最近 30 轮)。</Text>;
  }
  if (changeType === "Deleted") {
    return <Text style={bodyStyles.placeholder}>文件已删除，无内容对比。</Text>;
  }

  const rows = numberRows(computeLineDiff(view.change.old_text, view.change.new_text));
  const segments = segmentHunks(rows, DIFF_CONTEXT_LINES);
  if (segments.length === 0) {
    return <Text style={bodyStyles.placeholder}>（无内容差异）</Text>;
  }
  // Single line-number gutter (aligned with the desktop review panel): old
  // number on removed lines, new number on added/context lines. Width fits
  // the file's widest displayed number, using the MEASURED digit width.
  const digits = rows.reduce((max, row) => {
    const no = row.kind === "del" ? row.oldNo : row.newNo;
    return Math.max(max, no == null ? 0 : String(no).length);
  }, 1);
  const noWidth = Math.max(18, digits * digitWidth + 6);
  // Horizontal scroll like the desktop review diff: code lines NEVER wrap —
  // a line wider than the screen is reached by dragging. The content View is
  // sized by the widest row (column stretch) with minWidth covering narrow
  // files so gap blocks and tints still span the full visible width.
  return (
    <ScrollView
      horizontal
      showsHorizontalScrollIndicator
      nestedScrollEnabled
      contentContainerStyle={bodyStyles.hContent}
    >
      <View>
        {segments.map((segment, index) =>
          segment.kind === "hunk" ? (
            <View key={index}>
              <Text style={bodyStyles.heading}>{segment.heading}</Text>
              {segment.rows.map((row, rowIndex) => (
                <DiffLine key={rowIndex} row={row} noWidth={noWidth} />
              ))}
            </View>
          ) : expandedGaps.has(`${pathKey}:${index}`) ? (
            segment.rows.map((row, rowIndex) => (
              <DiffLine key={`${index}-${rowIndex}`} row={row} noWidth={noWidth} />
            ))
          ) : (
            <Pressable
              key={index}
              style={bodyStyles.gap}
              onPress={() => onToggleGap(`${pathKey}:${index}`)}
              accessibilityRole="button"
              accessibilityLabel={`展开 ${segment.count} 行未更改`}
            >
              <Text style={bodyStyles.gapText}>{`⋯ ${segment.count} 行未更改 ⋯`}</Text>
            </Pressable>
          ),
        )}
      </View>
    </ScrollView>
  );
});

const DiffLine = memo(function DiffLine({ row, noWidth }: { row: NumberedDiffRow; noWidth: number }) {
  // Mirrors the desktop review diff's cell structure: a NEUTRAL buffer strip
  // left of the number (always app-bg, marking where the colored region
  // starts), the number cell with a STRONGER tint on changed rows, and the
  // sign+content area with the lighter row tint.
  const isChanged = row.kind !== "same";
  const numberBg = isChanged
    ? row.kind === "add"
      ? diffStyles.cellAdd
      : diffStyles.cellDel
    : null;
  const contentBg = isChanged
    ? row.kind === "add"
      ? diffStyles.lineAdd
      : diffStyles.lineDel
    : null;
  const sign = row.kind === "add" ? "+" : row.kind === "del" ? "\u2212" : " ";
  const no = row.kind === "del" ? row.oldNo : row.newNo;
  return (
    <View style={diffStyles.line}>
      <View style={diffStyles.buffer} />
      <Text style={[diffStyles.no, numberBg, { width: noWidth }]}>{no ?? ""}</Text>
      <View style={[diffStyles.content, contentBg]}>
        <Text style={diffStyles.sign}>{sign}</Text>
        <Text style={diffStyles.text} selectable>
          {row.text === "" ? " " : row.text}
        </Text>
      </View>
    </View>
  );
});

/** Horizontal auto-scroll (marquee) for overflowing text.
 *
 * Measuring the long text directly does not work in Yoga: an absolute
 * shrink-to-fit child is still CLAMPED to the containing block, so a long
 * path measures as container-width and no overflow is ever detected. Instead
 * calibrate the MONOSPACE character width once with a short string (which
 * always fits) and derive the natural width as `chars × charWidth` — exact
 * for a monospace font. The visible copy then gets that explicit width, so
 * translateX genuinely reveals the full string. */
const CALIBRATION_TEXT = "0123456789";

function MarqueeText({ text, style }: { text: string; style: TextStyle }) {
  const [containerWidth, setContainerWidth] = useState(0);
  const [charWidth, setCharWidth] = useState(7.2);
  const offset = useRef(new Animated.Value(0)).current;

  const naturalWidth = Math.ceil(text.length * charWidth) + 2;
  const overflow = naturalWidth - containerWidth;

  useEffect(() => {
    offset.setValue(0);
    if (overflow <= 2) return;
    const distance = overflow + 12;
    const duration = Math.min(6500, Math.max(1800, distance * 24));
    const loop = Animated.loop(
      Animated.sequence([
        Animated.delay(900),
        Animated.timing(offset, {
          toValue: -distance,
          duration,
          easing: Easing.inOut(Easing.quad),
          useNativeDriver: true,
        }),
        Animated.delay(1100),
        Animated.timing(offset, {
          toValue: 0,
          duration,
          easing: Easing.inOut(Easing.quad),
          useNativeDriver: true,
        }),
        Animated.delay(1100),
      ]),
    );
    loop.start();
    return () => loop.stop();
  }, [overflow, offset]);

  return (
    <View
      style={marqueeStyles.mask}
      onLayout={(e) => setContainerWidth(e.nativeEvent.layout.width)}
    >
      {/* Short calibration string: fits without clamping, so its measured
          width / 10 is the true monospace char width. */}
      <Text
        style={[style, marqueeStyles.measurer]}
        onLayout={(e) => setCharWidth(e.nativeEvent.layout.width / CALIBRATION_TEXT.length)}
      >
        {CALIBRATION_TEXT}
      </Text>
      <Animated.Text
        numberOfLines={1}
        ellipsizeMode="clip"
        style={[
          style,
          marqueeStyles.animated,
          { width: Math.max(naturalWidth, containerWidth), transform: [{ translateX: offset }] },
        ]}
      >
        {text}
      </Animated.Text>
    </View>
  );
}

const marqueeStyles = StyleSheet.create({
  mask: {
    flex: 1,
    overflow: "hidden",
    justifyContent: "center",
  },
  // Absolute + short content ⇒ measured without clamping; invisible.
  measurer: {
    position: "absolute",
    left: 0,
    top: 0,
    opacity: 0,
  },
  animated: {
    minWidth: "100%",
  },
});

const sheetStyles = StyleSheet.create({
  screen: {
    flex: 1,
    backgroundColor: colors.bg,
    paddingTop: spacing.xl,
    paddingHorizontal: spacing.sm,
  },
  header: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    paddingVertical: spacing.xs,
    paddingHorizontal: spacing.sm,
  },
  back: {
    color: colors.accent,
    fontSize: 14,
    fontWeight: "600",
    width: 52,
  },
  headerTitle: {
    color: colors.text,
    fontSize: 14,
    fontWeight: "600",
  },
  offscreen: {
    position: "absolute",
    left: 0,
    top: 0,
    opacity: 0,
  },
  body: {
    flex: 1,
    marginTop: spacing.xs,
  },
  section: {
    marginBottom: spacing.sm,
    borderRadius: radius.md,
    overflow: "hidden",
  },
  sectionHeader: {
    flexDirection: "row",
    alignItems: "center",
    gap: spacing.xs + 2,
    paddingVertical: spacing.sm,
    paddingHorizontal: spacing.sm,
    backgroundColor: colors.surface,
  },
  sectionHeaderPressed: {
    backgroundColor: colors.surfaceAlt,
  },
  chevron: {
    color: colors.textFaint,
    fontSize: 13,
  },
  chevronOpen: {
    transform: [{ rotate: "90deg" }],
  },
  sectionPath: {
    color: colors.textDim,
    fontSize: 12,
    fontFamily: "monospace",
  },
  count: {
    fontSize: 12,
    fontWeight: "600",
    fontVariant: ["tabular-nums"],
  },
});

const bodyStyles = StyleSheet.create({
  placeholder: {
    color: colors.textDim,
    fontSize: 13,
    padding: spacing.md,
  },
  hContent: {
    minWidth: "100%",
  },
  heading: {
    color: colors.textFaint,
    fontSize: 10,
    fontFamily: "monospace",
    paddingHorizontal: spacing.xs + 2,
    paddingTop: spacing.xs,
  },
  gap: {
    paddingVertical: spacing.xs,
    paddingHorizontal: spacing.xs + 2,
    backgroundColor: colors.surfaceAlt,
    borderRadius: 4,
    marginVertical: 1,
    marginHorizontal: spacing.xs + 2,
  },
  gapText: {
    color: colors.textFaint,
    fontSize: 11,
    textAlign: "center",
    fontFamily: "monospace",
  },
});

const diffStyles = StyleSheet.create({
  line: {
    flexDirection: "row",
    alignItems: "flex-start",
    paddingVertical: 1,
  },
  // Neutral strip left of the numbers — always app-bg, matching the desktop
  // gutter-buffer so the colored diff region has a visible left edge.
  buffer: {
    width: 7,
    backgroundColor: colors.surface,
  },
  cellAdd: {
    backgroundColor: "rgba(46,160,67,0.32)",
  },
  cellDel: {
    backgroundColor: "rgba(248,81,73,0.32)",
  },
  lineAdd: {
    backgroundColor: "rgba(46,160,67,0.16)",
  },
  lineDel: {
    backgroundColor: "rgba(248,81,73,0.16)",
  },
  no: {
    paddingHorizontal: 3,
    color: colors.textFaint,
    fontSize: 10,
    lineHeight: 16,
    fontFamily: "monospace",
    textAlign: "right",
    fontVariant: ["tabular-nums"],
  },
  // Calibration twin of `no` WITHOUT padding/horizontal chrome: measuring
  // 8 digits of this gives the exact per-digit advance of the number font.
  calib: {
    color: "transparent",
    fontSize: 10,
    fontFamily: "monospace",
    fontVariant: ["tabular-nums"],
  },
  content: {
    flexDirection: "row",
    alignItems: "flex-start",
  },
  sign: {
    width: 12,
    color: colors.textDim,
    fontSize: 11,
    lineHeight: 16,
    fontFamily: "monospace",
    textAlign: "center",
  },
  text: {
    color: colors.text,
    fontSize: 11,
    lineHeight: 16,
    fontFamily: "monospace",
  },
});
// end of file
