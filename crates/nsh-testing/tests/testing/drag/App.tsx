// Views a drag acts on: a handle that takes the touches through the
// responder events and logs them, a card that pans natively with its
// `pan` value, a scrubber whose `panActivation` makes it the responder
// only past 10 points across, and rows reordered by dragging their
// handles (a `PanResponder`).
import { mount, signal, Text, View } from "nsh";
import { createAnimatedValue } from "nsh/animated";
import { PanResponder } from "nsh/gesture";
import { List } from "nsh/list";

const ROW = 40;

function Rows() {
  const rows = signal(["A", "B", "C", "D"]);
  let from = 0;
  // A recycled cell shows other rows over time: its handle reads which.
  const handle = (row: () => string) =>
    PanResponder.create({
      onStartShouldSetPanResponder: () => true,
      onPanResponderGrant: () => {
        from = rows().indexOf(row());
      },
      onPanResponderRelease: (_, g) => {
        const list = rows().filter((r) => r !== row());
        const to = Math.max(0, Math.min(list.length, from + Math.round(g.dy / ROW)));
        list.splice(to, 0, row());
        rows.set(list);
      },
    }).panHandlers;
  return (
    <View>
      <Text>{`Order: ${rows().join("")}`}</Text>
      <List each={rows()} style={{ height: ROW * 4 }}>
        {(row) => (
          <View testID={`row-${row()}`} style={{ height: ROW, flexDirection: "row" }}>
            <View testID={`handle-${row()}`} style={{ width: 30 }} {...handle(row)} />
            <Text>{row()}</Text>
          </View>
        )}
      </List>
    </View>
  );
}

function App() {
  const log = signal<string[]>([]);
  const note = (line: string) => log.update((l) => [...l, line]);
  const panned = signal("none");
  const scrub = signal("none");
  const x = createAnimatedValue(5);
  return (
    <View style={{ flex: 1, paddingTop: 20 }}>
      <View
        testID="handle"
        style={{ width: 100, height: 40 }}
        onStartShouldSetResponder={() => true}
        onResponderGrant={(e) => note(`grant ${e.locationX},${e.locationY}`)}
        onResponderMove={(e) => note(`move ${e.locationX},${e.locationY}`)}
        onResponderRelease={(e) => note(`release ${e.locationX},${e.locationY}`)}
      />
      <Text>{`Log: ${log().join(" | ")}`}</Text>
      <View
        testID="card"
        pan={{ x }}
        style={{ width: 100, height: 40 }}
        onPanStart={(e) => panned.set(`start ${e.translationX} at ${e.valueX}`)}
        onPanEnd={(e) => panned.set(`${panned()}; end ${e.translationX},${e.translationY} at ${e.valueX}`)}
      />
      <Text>{`Pan: ${panned()}`}</Text>
      <View
        testID="scrubber"
        style={{ width: 200, height: 40 }}
        panActivation={{ activeOffsetX: 10, failOffsetY: 10 }}
        onResponderGrant={(e) => scrub.set(`grant at ${e.locationX}`)}
        onResponderRelease={(e) => scrub.set(`${scrub()}; release at ${e.locationX}`)}
      />
      <Text>{`Scrub: ${scrub()}`}</Text>
      <Rows />
    </View>
  );
}

mount(App);
