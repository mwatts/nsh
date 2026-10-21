import { mount, signal, Text, View } from "nsh";

const pressed = signal("nothing");
const marked = signal(false);

mount(() => (
  <View>
    <Text testID="note" onPress={() => pressed.set("note")}>
      Read the{" "}
      <Text
        testID="docs"
        style={{ color: "#2563eb", textDecorationLine: "underline" }}
        onPress={() => pressed.set("docs")}
      >
        docs <Text testID="deep" style={{ fontWeight: "700" }} onPress={() => pressed.set("deep")}>here</Text>
      </Text>{" "}
      or <Text testID="mark" style={marked() ? { backgroundColor: "#fde68a" } : {}}>skip</Text> it.
    </Text>
    <Text>{`Pressed: ${pressed()}`}</Text>
    <Text testID="toggle" onPress={() => marked.set(!marked())}>
      Mark
    </Text>
  </View>
));
