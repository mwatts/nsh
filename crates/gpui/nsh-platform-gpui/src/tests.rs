//! Tests in windows of GPUI's test platform (`TestAppContext`; feature
//! `shot`, which turns on GPUI's test support): a surface drawn by GPUI
//! and given GPUI's input, with the engine running on its own thread.

use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App as GpuiApp, Entity, Focusable, IntoElement, Styled, TestAppContext,
    VisualTestContext, Window, div,
};

use crate::{App, AppOptions, AppView, Bundle, NativeComponent};

/// A native component that only draws a box.
struct Square;

impl NativeComponent for Square {
    fn render(&mut self, _: &mut Window, _: &mut GpuiApp) -> Option<AnyElement> {
        Some(div().size_full().into_any_element())
    }

    fn set_prop(
        &mut self,
        _: &str,
        _: Option<&serde_json::Value>,
        _: &mut Window,
        _: &mut GpuiApp,
    ) {
    }
}

/// A component and, after it, a focused field.
const FIELD_AFTER_COMPONENT: &str = r#"
import { Input, mount, signal, View } from "nsh";
import { nativeComponent } from "nsh/native";

const Square = nativeComponent<{}, {}>("test.square", { props: [], events: [] });

function App() {
  const text = signal("abcd");
  return (
    <View style={{ flex: 1, padding: 8, gap: 8 }}>
      <Square style={{ width: 40, height: 20 }} />
      <Input autoFocus value={text()} onChange={(t) => text.set(t)} style={{ height: 30 }} />
    </View>
  );
}
mount(App);
"#;

/// A focused field that logs its input method's compositions and sets
/// values while they are in progress: "held" on "ni" (applied once the
/// composition is cancelled), "stale" on "hao" (dropped, as "好" is
/// committed after it); the log becomes the text when "hao" ends.
const COMPOSING_FIELD: &str = r#"
import { Input, mount, signal, View } from "nsh";

function App() {
  const text = signal("abcd");
  const log: string[] = [];
  return (
    <View style={{ flex: 1, padding: 8 }}>
      <Input
        autoFocus
        value={text()}
        onChange={(t) => {
          log.push("change " + t);
          text.set(t);
        }}
        onCompositionChange={(e) => {
          log.push(e.composing ? "compose " + e.text : "end");
          if (e.text === "ni") text.set("held");
          if (e.text === "hao") text.set("stale");
          if (!e.composing && log.length > 2) text.set(log.join("|"));
        }}
        style={{ height: 30 }}
      />
    </View>
  );
}
mount(App);
"#;

/// A pressable strip over a focused field that shows what each press
/// carried: the field reads the modifier keys and click count of the last
/// press.
const PRESS_MODIFIERS: &str = r#"
import { Input, mount, signal, View } from "nsh";

function App() {
  const text = signal("none");
  return (
    <View style={{ flex: 1 }}>
      <View
        focusable={false}
        onPress={(e) =>
          text.set(
            e
              ? `ctrl ${e.ctrlKey} shift ${e.shiftKey} alt ${e.altKey} meta ${e.metaKey} clicks ${e.clickCount}`
              : "no event",
          )
        }
        style={{ height: 40 }}
      />
      <Input autoFocus value={text()} onChange={(t) => text.set(t)} style={{ height: 30 }} />
    </View>
  );
}
mount(App);
"#;

/// A field 40 points tall with 8 of padding under a 40-point strip; a
/// press on the strip writes what the field's last selection event said
/// into it: the range, then the caret's and the first line's left edge,
/// vertical middle and width, rounded (`-` for none).
const SELECTION_RECTS: &str = r#"
import { Input, mount, signal, View } from "nsh";

type Rect = { x: number; y: number; width: number; height: number };
const shown = (r?: Rect) =>
  r ? `${Math.round(r.x)} ${Math.round(r.y + r.height / 2)} ${Math.round(r.width)}` : "-";

function App() {
  const text = signal("abcd");
  let last = "none";
  return (
    <View style={{ flex: 1 }}>
      <View focusable={false} onPress={() => text.set(last)} style={{ height: 40 }} />
      <Input
        autoFocus
        value={text()}
        onChange={(t) => text.set(t)}
        onSelectionChange={(e) => {
          last = `${e.start} ${e.end} caret ${shown(e.caret)} first ${shown(e.firstRect)}`;
        }}
        style={{ height: 40, padding: 8 }}
      />
    </View>
  );
}
mount(App);
"#;

/// A field that logs its key presses (with Shift and Control) and caret
/// moves, under keymap bindings that take Control+Up and Tab wherever the
/// focus is; a press on the strip above writes the log into the field.
const KEYS_AND_BINDINGS: &str = r#"
import { Input, mount, signal, View } from "nsh";
import { keymap } from "nsh/keymap";

function App() {
  const text = signal("abcd");
  const log: string[] = [];
  keymap.bind("ctrl+up", () => log.push("bound"), { when: "always" });
  keymap.bind("tab", () => log.push("bound tab"), { when: "always" });
  return (
    <View style={{ flex: 1 }}>
      <View focusable={false} onPress={() => text.set(log.join("|"))} style={{ height: 40 }} />
      <Input
        autoFocus
        value={text()}
        onChange={(t) => text.set(t)}
        onKeyPress={(e) => log.push(e.key + (e.shiftKey ? "+shift" : "") + (e.ctrlKey ? "+ctrl" : ""))}
        onSelectionChange={(e) => log.push(`sel ${e.start}`)}
        style={{ height: 40 }}
      />
    </View>
  );
}
mount(App);
"#;

/// A host view around a surface that holds the window's focus itself,
/// as an embedding app's own widgets do.
struct Host {
    view: Entity<AppView>,
    focus: gpui::FocusHandle,
}

impl gpui::Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        use gpui::{InteractiveElement as _, ParentElement as _};
        div()
            .size_full()
            .track_focus(&self.focus)
            .child(self.view.clone())
    }
}

/// The focused field's text.
fn field_text(view: &Entity<AppView>, cx: &mut VisualTestContext) -> Option<String> {
    view.read_with(cx, |view, _| {
        crate::text_input::focused(view.surface())
            .and_then(|field| crate::text_input::accessible_value(&field))
            .map(|value| value.text)
    })
}

/// The value the focused field holds until its composition ends.
fn field_pending(view: &Entity<AppView>, cx: &mut VisualTestContext) -> Option<String> {
    view.read_with(cx, |view, _| {
        crate::text_input::focused(view.surface())
            .and_then(|field| crate::text_input::pending_value(&field))
    })
}

/// Runs the window until the focused field holds `text` for after its
/// composition.
fn wait_for_pending(view: &Entity<AppView>, cx: &mut VisualTestContext, text: &str) {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        let now = field_pending(view, cx);
        if now.as_deref() == Some(text) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the field holds {now:?}, not {text:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// An input method's step on the focused field.
fn ime(
    view: &Entity<AppView>,
    cx: &mut VisualTestContext,
    f: impl FnOnce(&Rc<crate::surface::Surface>),
) {
    view.update(cx, |view, _| f(view.surface()));
}

/// Runs the window (and waits for the engine's thread) until the focused
/// field reads `text`.
fn wait_for(view: &Entity<AppView>, cx: &mut VisualTestContext, text: &str) {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        let now = field_text(view, cx);
        if now.as_deref() == Some(text) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the field reads {now:?}, not {text:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The tests in windows, one after the other in one app: the main
/// thread's queue (`crate::rt`) is made once per process, for the first
/// app, so every window's engine must post to that app.
#[gpui::test]
fn windows(cx: &mut TestAppContext) {
    // The engine (and the settings portal) run on threads of their own.
    cx.executor().allow_parking();
    keys_reach_the_fields_of_a_surface_showing_a_native_component(cx);
    compositions_are_reported_and_hold_values_until_they_end(cx);
    presses_carry_the_modifier_keys_and_click_count(cx);
    selections_carry_the_caret_and_first_line_rectangles(cx);
    keys_are_reported_before_the_field_acts_and_bindings_take_them_first(cx);
    an_embedded_surface_takes_the_keyboard_when_its_app_focuses_a_field(cx);
}

/// Runs the window until the focused field's text satisfies `done`, and
/// returns it.
fn wait_until(
    view: &Entity<AppView>,
    cx: &mut VisualTestContext,
    done: impl Fn(&str) -> bool,
) -> String {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        let now = field_text(view, cx).unwrap_or_default();
        if done(&now) {
            return now;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the field reads {now:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn keys_reach_the_fields_of_a_surface_showing_a_native_component(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", FIELD_AFTER_COMPONENT, &[]).unwrap();
    let (view, cx) =
        cx.add_window_view(|window, cx| AppView::new(AppOptions::default(), window, cx));
    view.update(cx, |view, _| {
        view.register_component("test.square", Rc::new(|_| Box::new(Square)));
        view.load(App::Js(Bundle::source(code)));
    });
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    wait_for(&view, cx, "abcd");
    // Painting the component's element must not take the surface's key
    // listeners with it: deletions and moves come as key events.
    cx.simulate_keystrokes("end backspace");
    wait_for(&view, cx, "abc");
    cx.simulate_keystrokes("home delete");
    wait_for(&view, cx, "bc");
    cx.simulate_keystrokes("right shift-right backspace");
    wait_for(&view, cx, "b");
}

fn compositions_are_reported_and_hold_values_until_they_end(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", COMPOSING_FIELD, &[]).unwrap();
    let (view, cx) =
        cx.add_window_view(|window, cx| AppView::new(AppOptions::default(), window, cx));
    view.update(cx, |view, _| view.load(App::Js(Bundle::source(code))));
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    wait_for(&view, cx, "abcd");
    cx.simulate_keystrokes("end");
    // The same composition twice is reported once; the value JS sets
    // meanwhile waits, the text keeps what the input method composes in.
    ime(&view, cx, |s| crate::text_input::mark(s, "ni"));
    ime(&view, cx, |s| crate::text_input::mark(s, "ni"));
    wait_for_pending(&view, cx, "held");
    assert_eq!(field_text(&view, cx).as_deref(), Some("abcd"));
    // Cancelled: the value held is applied as the composition ends.
    ime(&view, cx, |s| crate::text_input::mark(s, ""));
    assert_eq!(field_text(&view, cx).as_deref(), Some("held"));
    assert_eq!(field_pending(&view, cx), None);
    // Committed: the commit's `onChange` comes before the end, and the
    // value set before the commit is stale.
    ime(&view, cx, |s| crate::text_input::mark(s, "hao"));
    wait_for_pending(&view, cx, "stale");
    ime(&view, cx, |s| crate::text_input::char_input(s, "好"));
    assert_eq!(field_text(&view, cx).as_deref(), Some("held好"));
    wait_for(&view, cx, "compose ni|end|compose hao|change held好|end");
}

fn presses_carry_the_modifier_keys_and_click_count(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", PRESS_MODIFIERS, &[]).unwrap();
    let (view, cx) =
        cx.add_window_view(|window, cx| AppView::new(AppOptions::default(), window, cx));
    view.update(cx, |view, _| view.load(App::Js(Bundle::source(code))));
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    wait_for(&view, cx, "none");
    let strip = gpui::point(gpui::px(20.0), gpui::px(20.0));
    cx.simulate_click(strip, gpui::Modifiers::control());
    wait_for(
        &view,
        cx,
        "ctrl true shift false alt false meta false clicks 1",
    );
    // A double click is two presses, the second counted as such.
    click(cx, strip, 1);
    click(cx, strip, 2);
    wait_for(
        &view,
        cx,
        "ctrl false shift false alt false meta false clicks 2",
    );
}

/// A left click at `position` that is click `count` of a series.
fn click(cx: &mut VisualTestContext, position: gpui::Point<gpui::Pixels>, count: usize) {
    let (button, modifiers) = (gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_event(gpui::MouseDownEvent {
        position,
        button,
        modifiers,
        click_count: count,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position,
        button,
        modifiers,
        click_count: count,
    });
}

fn selections_carry_the_caret_and_first_line_rectangles(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", SELECTION_RECTS, &[]).unwrap();
    let (view, cx) =
        cx.add_window_view(|window, cx| AppView::new(AppOptions::default(), window, cx));
    view.update(cx, |view, _| view.load(App::Js(Bundle::source(code))));
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    wait_for(&view, cx, "abcd");
    let strip = gpui::point(gpui::px(20.0), gpui::px(20.0));
    // Whichever end the caret started at, it moves and ends at the start:
    // at the padding, in the middle of the 40-point field, one point wide.
    cx.simulate_keystrokes("end");
    cx.simulate_keystrokes("home");
    cx.run_until_parked();
    cx.simulate_click(strip, gpui::Modifiers::default());
    wait_for(&view, cx, "0 0 caret 8 20 1 first -");
    // All of it selected: the first line starts at the padding, and the
    // caret, at the end, where the line ends.
    let all = if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    };
    cx.simulate_keystrokes(all);
    cx.run_until_parked();
    cx.simulate_click(strip, gpui::Modifiers::default());
    let report = wait_until(&view, cx, |t| t.starts_with("0 24 caret"));
    let n: Vec<f32> = report.split(' ').filter_map(|w| w.parse().ok()).collect();
    let [_, _, caret_x, caret_y, caret_w, first_x, first_y, first_w] = n[..] else {
        panic!("{report:?}");
    };
    assert_eq!((caret_y, caret_w), (20.0, 1.0), "{report}");
    assert_eq!((first_x, first_y), (8.0, 20.0), "{report}");
    assert!(first_w > 60.0, "{report}");
    assert!((first_x + first_w - caret_x).abs() <= 1.0, "{report}");
}

fn keys_are_reported_before_the_field_acts_and_bindings_take_them_first(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", KEYS_AND_BINDINGS, &[]).unwrap();
    let (view, cx) =
        cx.add_window_view(|window, cx| AppView::new(AppOptions::default(), window, cx));
    view.update(cx, |view, _| view.load(App::Js(Bundle::source(code))));
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    wait_for(&view, cx, "abcd");
    for keys in ["end", "left", "shift-left", "delete", "ctrl-up", "tab"] {
        cx.simulate_keystrokes(keys);
        cx.run_until_parked();
    }
    cx.simulate_click(
        gpui::point(gpui::px(20.0), gpui::px(20.0)),
        gpui::Modifiers::default(),
    );
    // Each key before what the field did with it: the caret moved, the
    // selection deleted (Backspace reports the deletion); the bound keys
    // never reached the field.
    let expected = "ArrowLeft|sel 3|ArrowLeft+shift|sel 2|Delete|Backspace|sel 2|bound|bound tab";
    let log = wait_until(&view, cx, |t| t.contains("bound tab"));
    assert!(log.ends_with(expected), "{log}");
}

fn an_embedded_surface_takes_the_keyboard_when_its_app_focuses_a_field(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", FIELD_AFTER_COMPONENT, &[]).unwrap();
    use gpui::AppContext as _;
    let (host, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| AppView::new(AppOptions::default(), window, cx));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Host { view, focus }
    });
    let view = host.read_with(cx, |host, _| host.view.clone());
    view.update(cx, |view, _| {
        view.register_component("test.square", Rc::new(|_| Box::new(Square)));
        view.load(App::Js(Bundle::source(code)));
    });
    // The app's `autoFocus` field, with the window's focus still on the
    // host's own element until the surface takes it.
    wait_for(&view, cx, "abcd");
    cx.simulate_keystrokes("end backspace");
    wait_for(&view, cx, "abc");
}
