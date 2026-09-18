import { memo, useCallback, useMemo, useState } from "react";
import { StyleSheet, View } from "react-native";
import { WebView } from "react-native-webview";
import type { WebViewMessageEvent } from "react-native-webview";
import { colors } from "../theme";
import {
  MATH_FALLBACK_HEIGHT,
  buildMathDocument,
  readReportedHeight,
  renderMathHtml,
} from "./math-html";

// One display formula, typeset by KaTeX inside a WebView.
//
// Why a WebView at all: React Native cannot lay out TeX. `react-native-svg`
// would mean reimplementing KaTeX's absolutely-positioned vlist model, and
// there is no inline-block, so a native math view could never sit inside a
// paragraph anyway. The WebView gives the exact desktop rendering for the
// construct that actually needs it.
//
// Scrolling stays with the parent list (`scrollEnabled={false}`): the document
// is measured and the view sized to it, so there is nothing to scroll. A
// formula wider than the column is clipped at the edges rather than swallowing
// the timeline's vertical gestures.
export const MathFormula = memo(function MathFormula({ tex }: { tex: string }) {
  const html = useMemo(() => renderMathHtml(tex), [tex]);
  const document = useMemo(() => buildMathDocument(html, colors.textDim), [html]);
  const [height, setHeight] = useState(MATH_FALLBACK_HEIGHT);

  const handleMessage = useCallback((event: WebViewMessageEvent) => {
    const next = readReportedHeight(event.nativeEvent.data);
    if (next !== null) setHeight(next);
  }, []);

  return (
    <View style={[mathStyles.wrap, { height }]}>
      <WebView
        source={{ html: document }}
        originWhitelist={["*"]}
        scrollEnabled={false}
        javaScriptEnabled
        setSupportMultipleWindows={false}
        // Transparent so the formula sits on the timeline background, not on a
        // white rectangle.
        style={mathStyles.webview}
        containerStyle={mathStyles.webview}
        onMessage={handleMessage}
      />
    </View>
  );
});

const mathStyles = StyleSheet.create({
  wrap: {
    width: "100%",
    marginTop: 8,
    marginBottom: 12,
    overflow: "hidden",
  },
  webview: {
    flex: 1,
    backgroundColor: "transparent",
  },
});
// end of file
