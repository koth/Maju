import { NavigationContainer } from "@react-navigation/native";
import { createNativeStackNavigator } from "@react-navigation/native-stack";
import { StatusBar } from "expo-status-bar";
import { SafeAreaProvider, SafeAreaView } from "react-native-safe-area-context";
import { PairingScreen } from "../features/pairing/PairingScreen";
import { SessionListScreen } from "../features/session-list/SessionListScreen";
import { ConversationScreen } from "../features/conversation/ConversationScreen";
import { SettingsScreen } from "../features/settings/SettingsScreen";
import { AppServicesProvider, useConnectionState } from "./AppServicesContext";
import { colors } from "../features/theme";

export type RootStackParamList = {
  Sessions: undefined;
  Conversation: { sessionId: string; title: string };
  Settings: undefined;
};

const Stack = createNativeStackNavigator<RootStackParamList>();

function MainStack() {
  return (
    <Stack.Navigator
      screenOptions={{
        headerStyle: { backgroundColor: colors.surface },
        headerTintColor: colors.text,
        headerTitleStyle: { color: colors.text },
        contentStyle: { backgroundColor: colors.bg },
      }}
    >
      <Stack.Screen name="Sessions" options={{ title: "Maju" }}>
        {({ navigation }) => (
          <SessionListScreen
            onOpenSession={(sessionId, title) => navigation.navigate("Conversation", { sessionId, title })}
          />
        )}
      </Stack.Screen>
      <Stack.Screen
        name="Conversation"
        options={({ route }) => ({ title: route.params.title })}
      >
        {({ route, navigation }) => (
          <ConversationScreen sessionId={route.params.sessionId} title={route.params.title} onBack={() => navigation.navigate("Sessions")} />
        )}
      </Stack.Screen>
      <Stack.Screen name="Settings" options={{ title: "Settings" }}>
        {({ navigation }) => (
          <SettingsScreen onRescan={() => navigation.navigate("Sessions")} />
        )}
      </Stack.Screen>
    </Stack.Navigator>
  );
}

// Root: shows pairing until connected, then the main session stack. The driver
// bootstrap (identity load) runs from the AppServicesProvider on mount.
function Root() {
  const connState = useConnectionState();
  const connected = connState === "connected";
  return (
    <SafeAreaView style={{ flex: 1, backgroundColor: colors.bg }}>
      {connected ? <MainStack /> : <PairingScreen />}
    </SafeAreaView>
  );
}

export function Navigation() {
  return (
    <SafeAreaProvider>
      <NavigationContainer>
        <StatusBar style="light" />
        <AppServicesProvider>
          <Root />
        </AppServicesProvider>
      </NavigationContainer>
    </SafeAreaProvider>
  );
}
// end of file
