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
    presses_carry_the_point_text_index_at_reads(cx);
    selections_carry_the_caret_and_first_line_rectangles(cx);
    keys_are_reported_before_the_field_acts_and_bindings_take_them_first(cx);
    an_embedded_surface_takes_the_keyboard_when_its_app_focuses_a_field(cx);
    each_view_s_functions_read_the_context_its_options_gave_its_engine(cx);
    a_view_given_its_own_ui_sets_serves_those_and_not_the_process_s(cx);
    an_embedded_view_that_owns_its_window_sets_its_title_and_menu(cx);
    quit_and_close_requests_go_to_the_handler_instead_of_acting(cx);
    an_auto_hidden_menu_bar_shows_on_alt_alone_and_while_a_menu_is_open(cx);
    asset_uris_resolve_in_the_first_directory_that_has_the_file(cx);
    menu_shortcuts_leave_a_focused_field_the_keys_it_uses(cx);
    the_app_s_shortcuts_reach_it_while_a_native_component_has_the_keyboard(cx);
    js_and_rust_view_shortcuts_fire_together_in_either_order(cx);
    text_typed_as_a_field_mounts_reaches_it(cx);
    text_typed_with_no_action_does_not_reach_a_later_field(cx);
    text_typed_after_a_shortcut_reaches_a_field_mounted_later(cx);
    text_typed_after_a_shortcut_is_dropped_once_the_second_passes(cx);
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

/// A Rust app: a line of ten "m"s inside 8 points of padding, over a
/// focused field. The field first reads the width of one "m" and the
/// line's height; a press on the line then writes where it was in the
/// line and in the window, and the caret offset `text_index_at` finds
/// there.
mod press_point {
    use nsh_core::{EventKind, EventPayload, PressEvent, TextLine, ui::prelude::*};

    #[component]
    pub(super) fn PressPoint() -> impl IntoElement {
        let report = signal(String::from("none"));
        let line = element_ref();
        view()
            .flex_1()
            .p(px(8.0))
            .child(
                text("mmmmmmmmmm")
                    .focusable(false)
                    .element_ref(&line)
                    .on_text_layout(move |lines: &[TextLine]| {
                        if let Some(first) = lines.first() {
                            report.set(format!(
                                "m {:.2} line {:.2}",
                                first.width.0 / 10.0,
                                first.height.0
                            ));
                        }
                    })
                    .on(EventKind::Press, move |p: &EventPayload| {
                        let EventPayload::Press(PressEvent {
                            position: Some(at), ..
                        }) = *p
                        else {
                            report.set("no position".into());
                            return;
                        };
                        spawn(async move {
                            let index = line.text_index_at(at.location).await;
                            let (l, g) = (at.location, at.page);
                            report.set(format!(
                                "at {:.1} {:.1} in {:.1} {:.1} index {index:?}",
                                l.x.0, l.y.0, g.x.0, g.y.0
                            ));
                        });
                    }),
            )
            .child(
                input()
                    .auto_focus(true)
                    .value(move || report.get())
                    .h(px(30.0)),
            )
    }
}

fn presses_carry_the_point_text_index_at_reads(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| AppView::new(AppOptions::default(), window, cx));
    view.update(cx, |view, _| {
        view.load(App::Rust(crate::Root::of::<press_point::PressPoint>()))
    });
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    let metrics = wait_until(&view, cx, |t| t.starts_with("m "));
    let n: Vec<f32> = metrics.split(' ').filter_map(|w| w.parse().ok()).collect();
    let [m, line] = n[..] else {
        panic!("{metrics:?}")
    };
    assert!(m > 1.0 && line > 1.0, "{metrics}");
    // A quarter into the third "m", half way down the line: the line's
    // point is the window's less the padding, and the caret offset there
    // is the third character's (2).
    let (x, y) = (2.25 * m, line / 2.0);
    cx.simulate_click(
        gpui::point(gpui::px(8.0 + x), gpui::px(8.0 + y)),
        gpui::Modifiers::default(),
    );
    wait_for(
        &view,
        cx,
        &format!(
            "at {x:.1} {y:.1} in {:.1} {:.1} index Some(2)",
            8.0 + x,
            8.0 + y
        ),
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

/// The part of the embedding app a view reaches: its engine's context.
struct Desk(&'static str);

/// Which desk the engine running the call was given.
#[nsh_core::ui::module]
struct Whose;

#[nsh_core::ui::module]
impl Whose {
    /// In the JS call.
    pub(crate) fn now() -> String {
        desk()
    }

    /// As a task, after it waited.
    pub(crate) async fn later() -> String {
        nsh_core::ui::prelude::timer(Duration::from_millis(5)).await;
        desk()
    }
}

fn desk() -> String {
    nsh_core::ui::engine_context::<Desk>()
        .map_or_else(|| "none".to_owned(), |desk| desk.0.to_owned())
}

/// A field showing which desk `Whose` names, in the call and in a task.
const WHOSE: &str = r#"
import { Input, mount, signal, View } from "nsh";
import { nativeModule } from "nsh/native";

interface WhoseApi {
  now(): string;
  later(): Promise<string>;
}
const { Whose } = nativeModule<{ Whose: WhoseApi }>("test/context");

function App() {
  const text = signal("waiting");
  setTimeout(async () => text.set(`${Whose.now()} ${await Whose.later()}`), 0);
  return (
    <View style={{ flex: 1 }}>
      <Input autoFocus value={text()} onChange={(t) => text.set(t)} style={{ height: 30 }} />
    </View>
  );
}
mount(App);
"#;

/// Two surfaces side by side in one window.
struct Pair {
    a: Entity<AppView>,
    b: Entity<AppView>,
}

impl gpui::Render for Pair {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        use gpui::ParentElement as _;
        div()
            .size_full()
            .flex()
            .child(div().flex_1().child(self.a.clone()))
            .child(div().flex_1().child(self.b.clone()))
    }
}

fn each_view_s_functions_read_the_context_its_options_gave_its_engine(cx: &mut TestAppContext) {
    let mut context = nsh_core::ui::Registry::ui_set("test/context");
    context.module::<Whose>();
    crate::set_ui_sets(vec![nsh_base::registry(), context]);
    let code = nsh_bundler::compile_module("App.tsx", WHOSE, &[]).unwrap();
    use gpui::AppContext as _;
    let (pair, cx) = cx.add_window_view(|window, cx| {
        let view = |desk: &'static str, window: &mut Window, cx: &mut gpui::Context<Pair>| {
            let options = AppOptions {
                context: nsh_engine::EngineContext::default().with(Desk(desk)),
                ..AppOptions::default()
            };
            cx.new(|cx| AppView::new(options, window, cx))
        };
        Pair {
            a: view("A", window, cx),
            b: view("B", window, cx),
        }
    });
    let (a, b) = pair.read_with(cx, |pair, _| (pair.a.clone(), pair.b.clone()));
    for view in [&a, &b] {
        view.update(cx, |view, _| {
            view.load(App::Js(Bundle::source(code.clone())))
        });
    }
    wait_for(&a, cx, "A A");
    wait_for(&b, cx, "B B");
}

/// A field showing whether `test/context` exists for this engine.
const REACHES: &str = r#"
import { Input, mount, signal, View } from "nsh";
import { nativeModule } from "nsh/native";

function App() {
  const text = signal("waiting");
  setTimeout(() => {
    try {
      const { Whose } = nativeModule<{ Whose: { now(): string } }>("test/context");
      text.set(`reaches ${Whose.now()}`);
    } catch {
      text.set("no set");
    }
  }, 0);
  return (
    <View style={{ flex: 1 }}>
      <Input autoFocus value={text()} onChange={(t) => text.set(t)} style={{ height: 30 }} />
    </View>
  );
}
mount(App);
"#;

fn a_view_given_its_own_ui_sets_serves_those_and_not_the_process_s(cx: &mut TestAppContext) {
    let mut context = nsh_core::ui::Registry::ui_set("test/context");
    context.module::<Whose>();
    crate::set_ui_sets(vec![nsh_base::registry(), context.clone()]);
    let code = nsh_bundler::compile_module("App.tsx", REACHES, &[]).unwrap();
    use gpui::AppContext as _;
    let (pair, cx) = cx.add_window_view(|window, cx| {
        let view = |sets: Option<Vec<nsh_core::ui::Registry>>,
                    window: &mut Window,
                    cx: &mut gpui::Context<Pair>| {
            let options = AppOptions {
                context: nsh_engine::EngineContext::default().with(Desk("own")),
                ui_sets: sets,
                ..AppOptions::default()
            };
            cx.new(|cx| AppView::new(options, window, cx))
        };
        Pair {
            // The process's sets, `test/context` among them.
            a: view(None, window, cx),
            // Its own: `nsh/base` alone.
            b: view(Some(vec![nsh_base::registry()]), window, cx),
        }
    });
    let (a, b) = pair.read_with(cx, |pair, _| (pair.a.clone(), pair.b.clone()));
    for view in [&a, &b] {
        view.update(cx, |view, _| {
            view.load(App::Js(Bundle::source(code.clone())))
        });
    }
    wait_for(&a, cx, "reaches own");
    wait_for(&b, cx, "no set");
}

/// A view the window's root holds, as an embedding app's frame does.
struct Framed(Entity<AppView>);

impl gpui::Render for Framed {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        use gpui::ParentElement as _;
        div().size_full().child(self.0.clone())
    }
}

/// Opens a window whose root holds an [`AppView`] that owns the window,
/// running `code`, with the keyboard in the view.
fn owned_view(
    cx: &mut TestAppContext,
    code: String,
    setup: impl FnOnce(&AppView),
) -> (Entity<AppView>, &mut VisualTestContext) {
    use gpui::AppContext as _;
    let (framed, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let view = AppView::new(AppOptions::default(), window, cx);
            view.own_window(window, cx);
            view
        });
        Framed(view)
    });
    let view = framed.read_with(cx, |framed, _| framed.0.clone());
    view.update(cx, |view, _| {
        setup(view);
        view.load(App::Js(Bundle::source(code)));
    });
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    (view, cx)
}

/// Runs the window until `done` holds.
fn wait_until_window(
    cx: &mut VisualTestContext,
    what: &str,
    done: impl Fn(&mut VisualTestContext) -> bool,
) {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "never: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The window's content height, in points.
fn viewport_height(cx: &mut VisualTestContext) -> f32 {
    cx.update(|window, _| f32::from(window.viewport_size().height))
}

/// A field that reads the app's height (`h <points>`, the window less the
/// chrome around the app) and ` new` once New was chosen, under a title
/// and an application menu: File with New, Quit (the `quit` role, with a
/// shortcut of its own) and Close (the `close` role).
const OWNED_WINDOW: &str = r#"
import { Input, mount, signal, View } from "nsh";
import { Menu, Window } from "nsh/desktop";

function App() {
  const height = signal(-1);
  const chosen = signal("");
  Window.setTitle("Owned");
  Menu.setApplicationMenu([
    {
      label: "File",
      submenu: [
        { label: "New", shortcut: "mod+shift+n", onSelect: () => chosen.set(" new") },
        { label: "Quit", role: "quit", shortcut: "mod+shift+q" },
        { label: "Close", role: "close", shortcut: "mod+shift+w" },
      ],
    },
  ]);
  return (
    <View style={{ flex: 1 }} onLayout={(e) => height.set(Math.round(e.height))}>
      <Input autoFocus value={`h ${height()}${chosen()}`} style={{ height: 30 }} />
    </View>
  );
}
mount(App);
"#;

/// The app's height the field reads.
fn app_height(view: &Entity<AppView>, cx: &mut VisualTestContext) -> f32 {
    let text = wait_until(view, cx, |t| t.starts_with("h ") && !t.starts_with("h -1"));
    text.split(' ').nth(1).and_then(|h| h.parse().ok()).unwrap()
}

fn an_embedded_view_that_owns_its_window_sets_its_title_and_menu(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", OWNED_WINDOW, &[]).unwrap();
    let (view, cx) = owned_view(cx, code, |_| {});
    wait_until_window(cx, "the app's title on the window", |cx| {
        cx.window_title().as_deref() == Some("Owned")
    });
    if cfg!(target_os = "linux") {
        // The menu bar is drawn above the app, 28 points tall.
        let window = viewport_height(cx);
        wait_until(&view, cx, |t| t == format!("h {}", window - 28.0));
    }
}

fn quit_and_close_requests_go_to_the_handler_instead_of_acting(cx: &mut TestAppContext) {
    use crate::CloseRequest::{Close, Quit};
    let code = nsh_bundler::compile_module("App.tsx", OWNED_WINDOW, &[]).unwrap();
    let asked = Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = asked.clone();
    let (view, cx) = owned_view(cx, code, move |view| {
        view.on_close_request(move |request, _, _| log.borrow_mut().push(request))
    });
    app_height(&view, cx);
    let primary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let mut expected = Vec::new();
    for (keys, request) in [
        // The reserved quit key, Ctrl+W, and the menu's roles by their
        // shortcuts.
        (format!("{primary}-q"), Quit),
        (format!("{primary}-w"), Close),
        (format!("shift-{primary}-q"), Quit),
        (format!("shift-{primary}-w"), Close),
    ] {
        cx.simulate_keystrokes(&keys);
        expected.push(request);
        wait_until_window(cx, &format!("{keys} asks {request:?}"), |_| {
            *asked.borrow() == expected
        });
    }
    // The window's close button: the window stays.
    assert!(!cx.simulate_close(), "the close button closed the window");
    expected.push(Close);
    wait_until_window(cx, "the close button asks Close", |_| {
        *asked.borrow() == expected
    });
    // Still open, still running: the menu's other items work.
    let height = app_height(&view, cx);
    cx.simulate_keystrokes(&format!("shift-{primary}-n"));
    wait_for(&view, cx, &format!("h {height} new"));
}

/// The app's height under an auto-hidden menu bar (and ` new` once File
/// > New was chosen).
const AUTO_HIDDEN_BAR: &str = r#"
import { Input, mount, signal, View } from "nsh";
import { Menu, Window } from "nsh/desktop";

function App() {
  const height = signal(-1);
  const chosen = signal("");
  Window.setMenuBar("autoHide");
  Menu.setApplicationMenu([
    { label: "File", submenu: [{ label: "New", onSelect: () => chosen.set(" new") }] },
  ]);
  return (
    <View style={{ flex: 1 }} onLayout={(e) => height.set(Math.round(e.height))}>
      <Input autoFocus value={`h ${height()}${chosen()}`} style={{ height: 30 }} />
    </View>
  );
}
mount(App);
"#;

fn an_auto_hidden_menu_bar_shows_on_alt_alone_and_while_a_menu_is_open(cx: &mut TestAppContext) {
    // macOS's menu bar is the system's.
    if !cfg!(target_os = "linux") && !cfg!(windows) {
        return;
    }
    let code = nsh_bundler::compile_module("App.tsx", AUTO_HIDDEN_BAR, &[]).unwrap();
    let (view, cx) = owned_view(cx, code, |_| {});
    let full = viewport_height(cx);
    let (hidden, shown) = (format!("h {full}"), format!("h {}", full - 28.0));
    wait_for(&view, cx, &hidden);
    let alt_alone = |cx: &mut VisualTestContext| {
        cx.simulate_modifiers_change(gpui::Modifiers::alt());
        cx.simulate_modifiers_change(gpui::Modifiers::none());
    };
    alt_alone(cx);
    wait_for(&view, cx, &shown);
    alt_alone(cx);
    wait_for(&view, cx, &hidden);
    // Alt with a key or another modifier is a shortcut: Alt + F opens File
    // (the bar shows while it is open), and the bar hides again as it
    // closes.
    cx.simulate_modifiers_change(gpui::Modifiers::alt());
    cx.simulate_keystrokes("alt-f");
    cx.simulate_modifiers_change(gpui::Modifiers::none());
    wait_for(&view, cx, &shown);
    cx.simulate_keystrokes("escape");
    wait_for(&view, cx, &hidden);
    cx.simulate_modifiers_change(gpui::Modifiers::alt());
    cx.simulate_modifiers_change(gpui::Modifiers {
        alt: true,
        shift: true,
        ..Default::default()
    });
    cx.simulate_modifiers_change(gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        field_text(&view, cx),
        Some(hidden.clone()),
        "Alt + Shift showed the bar"
    );
    // F10 opens the first menu; Return chooses its first item, and the bar
    // hides again.
    cx.simulate_keystrokes("f10");
    wait_for(&view, cx, &shown);
    cx.simulate_keystrokes("enter");
    wait_for(&view, cx, &format!("{hidden} new"));
}

/// Two images from two asset directories: each reports its size, or why
/// it did not load.
const TWO_ASSET_ROOTS: &str = r#"
import { Input, mount, signal, View } from "nsh";
import { Image } from "nsh/image";

function App() {
  const text = signal("waiting");
  const seen: string[] = [];
  const report = (name: string, what: string) => {
    seen.push(`${name} ${what}`);
    if (seen.length === 2) text.set(seen.sort().join("|"));
  };
  return (
    <View style={{ flex: 1 }}>
      <Image
        src="asset:/app.png"
        onLoad={(e) => report("app", `${e.width}x${e.height}`)}
        onError={(e) => report("app", e.message)}
        style={{ width: 10, height: 10 }}
      />
      <Image
        src="asset:/bytes/cover.png"
        onLoad={(e) => report("cover", `${e.width}x${e.height}`)}
        onError={(e) => report("cover", e.message)}
        style={{ width: 10, height: 10 }}
      />
      <Input autoFocus value={text()} onChange={(t) => text.set(t)} style={{ height: 30 }} />
    </View>
  );
}
mount(App);
"#;

fn asset_uris_resolve_in_the_first_directory_that_has_the_file(cx: &mut TestAppContext) {
    let root = std::env::temp_dir().join(format!("nsh-asset-roots-{}", std::process::id()));
    let (app, archive) = (root.join("app"), root.join("archive"));
    std::fs::create_dir_all(archive.join("bytes")).unwrap();
    std::fs::create_dir_all(&app).unwrap();
    let png = |path: std::path::PathBuf, w: u32, h: u32| {
        image::RgbaImage::new(w, h).save(path).unwrap();
    };
    png(app.join("app.png"), 2, 3);
    png(archive.join("bytes/cover.png"), 4, 5);
    // The archive's own `app.png` is not the one shown: the app's comes
    // first.
    png(archive.join("app.png"), 7, 7);
    let code = nsh_bundler::compile_module("App.tsx", TWO_ASSET_ROOTS, &[]).unwrap();
    let (view, cx) =
        cx.add_window_view(|window, cx| AppView::new(AppOptions::default(), window, cx));
    view.update(cx, |view, _| {
        view.set_asset_base(Some(crate::AssetBase::Directories(vec![app, archive])));
        view.load(App::Js(Bundle::source(code)));
    });
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    let report = wait_until(&view, cx, |t| t.contains('|'));
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(report, "app 2x3|cover 4x5");
}

/// A field that keeps its focus on Return, under a menu whose items add
/// their names to it: three that leave a focused field the keys it uses
/// (`when: "auto"`) and one that takes its key everywhere (the default).
const MENU_OVER_FIELD: &str = r#"
import { Input, mount, signal, View } from "nsh";
import { Menu } from "nsh/desktop";

function App() {
  const text = signal("abcd");
  const chose = (name: string) => () => text.set(`${text()} ${name}`);
  Menu.setApplicationMenu([
    {
      label: "Edit",
      submenu: [
        { label: "Revert", shortcut: "mod+z", when: "auto", onSelect: chose("revert") },
        { label: "Remove", shortcut: "delete", when: "auto", onSelect: chose("remove") },
        { label: "Send", shortcut: "mod+enter", when: "auto", onSelect: chose("send") },
        { label: "Purge", shortcut: "shift+delete", onSelect: chose("purge") },
      ],
    },
  ]);
  return (
    <View style={{ flex: 1 }}>
      <Input
        autoFocus
        submitBehavior="submit"
        value={text()}
        onChange={(t) => text.set(t)}
        style={{ height: 30 }}
      />
    </View>
  );
}
mount(App);
"#;

fn menu_shortcuts_leave_a_focused_field_the_keys_it_uses(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", MENU_OVER_FIELD, &[]).unwrap();
    let (view, cx) = owned_view(cx, code, |_| {});
    let primary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    wait_for(&view, cx, "abcd");
    cx.simulate_keystrokes("end");
    cx.simulate_input("e");
    wait_for(&view, cx, "abcde");
    cx.simulate_keystrokes(&format!("{primary}-z"));
    wait_for(&view, cx, "abcd");
    cx.simulate_keystrokes("home delete");
    wait_for(&view, cx, "bcd");
    cx.simulate_keystrokes(&format!("{primary}-enter"));
    cx.run_until_parked();
    cx.simulate_keystrokes("shift-delete");
    wait_for(&view, cx, "bcd purge");
    cx.simulate_keystrokes("escape");
    wait_until_window(cx, "Escape takes the focus from the field", |cx| {
        field_text(&view, cx).is_none()
    });
    cx.simulate_keystrokes(&format!("{primary}-z delete {primary}-enter"));
    cx.run_until_parked();
    let bar = if cfg!(target_os = "macos") { 0.0 } else { 28.0 };
    cx.simulate_click(
        gpui::point(gpui::px(20.0), gpui::px(bar + 15.0)),
        gpui::Modifiers::default(),
    );
    wait_for(&view, cx, "bcd purge revert remove send");
}

/// A native component that takes the keyboard and records each key down
/// it sees, letting every key go on; it shares its focus handle once drawn.
struct KeyLog {
    keys: Rc<std::cell::RefCell<Vec<String>>>,
    focus: Rc<std::cell::RefCell<Option<gpui::FocusHandle>>>,
}

impl NativeComponent for KeyLog {
    fn render(&mut self, _: &mut Window, cx: &mut GpuiApp) -> Option<AnyElement> {
        use gpui::InteractiveElement as _;
        let focus = self
            .focus
            .borrow_mut()
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        let keys = self.keys.clone();
        Some(
            div()
                .size_full()
                .track_focus(&focus)
                .on_key_down(move |e, _, _| keys.borrow_mut().push(e.keystroke.unparse()))
                .into_any_element(),
        )
    }

    fn focus_handle(&self) -> Option<gpui::FocusHandle> {
        self.focus.borrow().clone()
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

/// A field showing which of the app's shortcuts fired, above a component
/// that takes the keyboard: a binding that fires everywhere (`mod+k`), one
/// that leaves text editing its keys (`j`), a menu item that does too
/// (`mod+z`) and one that takes its key everywhere (`mod+shift+n`).
const SHORTCUTS_OVER_COMPONENT: &str = r#"
import { Input, mount, signal, View } from "nsh";
import { Menu } from "nsh/desktop";
import { keymap } from "nsh/keymap";
import { nativeComponent } from "nsh/native";

const KeyLog = nativeComponent<{}, {}>("test.keylog", { props: [], events: [] });

function App() {
  const fired = signal("fired");
  const fire = (name: string) => () => fired.set(`${fired()} ${name}`);
  keymap.bind("mod+k", fire("bound"), { when: "always" });
  keymap.bind("j", fire("j"));
  Menu.setApplicationMenu([
    {
      label: "Edit",
      submenu: [
        { label: "Revert", shortcut: "mod+z", when: "auto", onSelect: fire("revert") },
        { label: "New", shortcut: "mod+shift+n", onSelect: fire("new") },
      ],
    },
  ]);
  return (
    <View style={{ flex: 1 }}>
      <Input value={fired()} style={{ height: 30 }} />
      <KeyLog style={{ height: 40 }} />
    </View>
  );
}
mount(App);
"#;

fn the_app_s_shortcuts_reach_it_while_a_native_component_has_the_keyboard(cx: &mut TestAppContext) {
    use std::cell::RefCell;
    let code = nsh_bundler::compile_module("App.tsx", SHORTCUTS_OVER_COMPONENT, &[]).unwrap();
    let keys = Rc::new(RefCell::new(Vec::new()));
    let focus = Rc::new(RefCell::new(None));
    let asked = Rc::new(RefCell::new(Vec::new()));
    let (component_keys, component_focus, log) = (keys.clone(), focus.clone(), asked.clone());
    let (view, cx) = owned_view(cx, code, move |view| {
        view.register_component(
            "test.keylog",
            Rc::new(move |_| {
                Box::new(KeyLog {
                    keys: component_keys.clone(),
                    focus: component_focus.clone(),
                })
            }),
        );
        view.on_close_request(move |request, _, _| log.borrow_mut().push(request));
    });
    wait_until_window(cx, "the component is drawn", |_| focus.borrow().is_some());
    let handle = focus.borrow().clone().unwrap();
    cx.update(|window, cx| window.focus(&handle, cx));
    cx.run_until_parked();
    let primary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.simulate_keystrokes(&format!("a j {primary}-z {primary}-k"));
    // GPUI's test window sends no releases: K's comes to the keymap while
    // the component has the focus, so the next Ctrl+K is no repeat.
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse(&format!("{primary}-k")).unwrap(),
    });
    cx.simulate_keystrokes(&format!("{primary}-k shift-{primary}-n {primary}-q"));
    wait_until_window(cx, "the quit key asks Quit", |_| {
        *asked.borrow() == [crate::CloseRequest::Quit]
    });
    // The component saw every key first, the typed ones and Undo among
    // them; its focus never moved.
    assert_eq!(
        keys.borrow()[..4],
        ["a", "j", &format!("{primary}-z"), &format!("{primary}-k")],
    );
    cx.update(|window, _| assert!(handle.is_focused(window), "the component lost the focus"));
    // Only the shortcuts that fire while text is edited fired: both Ctrl+K
    // presses (a released key is not held), and the menu's New.
    let bar = if cfg!(target_os = "macos") { 0.0 } else { 28.0 };
    cx.simulate_click(
        gpui::point(gpui::px(20.0), gpui::px(bar + 15.0)),
        gpui::Modifiers::default(),
    );
    wait_for(&view, cx, "fired bound bound new");
}

/// A Rust view (`test/keys`) that binds a keystroke of its own.
mod rust_keys {
    use nsh_core::ui::{Registry, prelude::*};

    /// The set the app imports `Shortcut` from.
    pub(super) fn registry() -> Registry {
        let mut registry = Registry::ui_set("test/keys");
        registry.register::<Shortcut>();
        registry
    }

    #[derive(Debug, Clone, PartialEq, Props)]
    pub(super) struct ShortcutProps {
        /// The keystroke, as `on_key` takes it.
        pub(super) keys: Prop<SharedString>,
    }

    #[derive(Debug, Clone, PartialEq, Event)]
    pub(super) enum ShortcutEvent {
        /// The keystroke was pressed.
        Fired,
    }

    /// Binds `keys` (in text fields too) and reports each press.
    #[component]
    pub(super) fn Shortcut(props: ShortcutProps, cx: Scope<ShortcutEvent>) -> impl IntoElement {
        let options = KeyOptions {
            when: nsh_core::keymap::When::Always,
            ..KeyOptions::default()
        };
        on_key_with(&props.keys.get(), options, move |_| {
            cx.emit(ShortcutEvent::Fired)
        });
        view()
    }
}

/// A field logging Escape (JS's binding), Shift+Enter (a Rust view's) and
/// Mod+T (JS's, which shows and hides the view). `VIEW_FIRST` shows the
/// view from the start and binds Escape once it mounted; else Escape is
/// bound before any Rust view exists.
fn js_and_rust_view(view_first: bool) -> String {
    format!(
        r#"
import {{ Input, mount, signal, View }} from "nsh";
import {{ keymap }} from "nsh/keymap";
import {{ Shortcut }} from "test/keys";

const VIEW_FIRST = {view_first};

function App() {{
  const fired = signal("fired");
  const shown = signal(VIEW_FIRST);
  const fire = (name: string) => fired.set(`${{fired()}} ${{name}}`);
  const escape = () => keymap.bind("escape", () => fire("escape"), {{ when: "always" }});
  keymap.bind("mod+t", () => {{ shown.set(!shown()); fire(shown() ? "shown" : "hidden"); }}, {{ when: "always" }});
  if (VIEW_FIRST) setTimeout(() => {{ escape(); fire("bound"); }}, 0); else escape();
  return (
    <View style={{{{ flex: 1 }}}}>
      <Input autoFocus value={{fired()}} style={{{{ height: 30 }}}} />
      <If when={{shown()}}>
        <Shortcut keys="shift+enter" onFired={{() => fire("shift")}} style={{{{ height: 10 }}}} />
      </If>
    </View>
  );
}}
mount(App);
"#
    )
}

/// Dispatches one key and does not park. The following input stays in the
/// same turn as a shortcut that focuses a field on the next one.
fn type_key(cx: &mut VisualTestContext, key: &str) {
    let keystroke = gpui::Keystroke::parse(key).unwrap();
    cx.update(|window, cx| {
        window.dispatch_keystroke(keystroke, cx);
    });
}

/// No field yet. `n` mounts and focuses one on the next turn. `abc` is
/// typed in that same turn, with no wait between the two.
const FIELD_ON_THE_NEXT_TURN: &str = r#"
import { Input, View, mount, signal } from "nsh";
import { keymap } from "nsh/keymap";
import { Window } from "nsh/desktop";

function App() {
  const shown = signal(false);
  Window.setTitle("ready");
  keymap.bind("n", () => {
    setTimeout(() => shown.set(true), 0);
  }, { when: "always" });
  return (
    <View style={{ flex: 1 }}>
      <If when={shown()}>
        <Input autoFocus style={{ height: 30 }} />
      </If>
    </View>
  );
}
mount(App);
"#;

fn text_typed_as_a_field_mounts_reaches_it(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", FIELD_ON_THE_NEXT_TURN, &[]).unwrap();
    let (view, cx) = owned_view(cx, code, |_| {});
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        if cx.window_title().as_deref() == Some("ready") {
            cx.run_until_parked();
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the window never became ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    type_key(cx, "n");
    cx.simulate_input("abc");
    let start = Instant::now();
    loop {
        if field_text(&view, cx).as_deref() == Some("abc") {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "text typed as the field mounts, saw {:?}",
            field_text(&view, cx)
        );
        cx.run_until_parked();
    }
}

/// A field is on screen and not focused. Nothing arms the queue.
const UNFOCUSED_FIELD: &str = r#"
import { Input, View, mount } from "nsh";

function App() {
  return (
    <View style={{ flex: 1 }}>
      <Input style={{ height: 30 }} />
    </View>
  );
}
mount(App);
"#;

fn text_typed_with_no_action_does_not_reach_a_later_field(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", UNFOCUSED_FIELD, &[]).unwrap();
    let (view, cx) = owned_view(cx, code, |_| {});
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        let laid_out = view.read_with(cx, |view, _| {
            fn height(view: &crate::view::NshView) -> f32 {
                if view.state().input.is_some() {
                    return view.state().frame.size.height.0;
                }
                let mut found = 0.0_f32;
                view.with_children(|children| {
                    for child in children {
                        found = found.max(height(child));
                    }
                });
                found
            }
            height(&view.surface().root)
        });
        if laid_out > 0.0 {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the field never mounted"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    type_key(cx, "x");
    let place = view.read_with(cx, |view, _| {
        fn find(view: &crate::view::NshView) -> Option<(f32, f32, f32, f32)> {
            if view.state().input.is_some() {
                let frame = view.state().frame;
                return Some((
                    frame.origin.x.0,
                    frame.origin.y.0,
                    frame.size.width.0,
                    frame.size.height.0,
                ));
            }
            let mut found = None;
            view.with_children(|children| {
                for child in children {
                    if found.is_none() {
                        found = find(child);
                    }
                }
            });
            found
        }
        let origin = view.surface().origin.get();
        (origin, find(&view.surface().root))
    });
    let bar = if cfg!(target_os = "macos") { 0.0 } else { 28.0 };
    let (x, y) = match place.1 {
        Some((fx, fy, _, fh)) => (place.0.0 + fx + 8.0, place.0.1 + fy + fh / 2.0),
        None => (20.0, bar + 15.0),
    };
    cx.simulate_click(
        gpui::point(gpui::px(x), gpui::px(y)),
        gpui::Modifiers::default(),
    );
    assert_eq!(
        field_text(&view, cx).as_deref(),
        Some(""),
        "a letter with no action stays out of the field at {place:?} click {x} {y}"
    );
}

/// `n` mounts the field over several frames. `abc` is typed before any of
/// those frames, and the field still receives it.
const FIELD_AFTER_SEVERAL_TURNS: &str = r#"
import { Input, View, mount, signal } from "nsh";
import { keymap } from "nsh/keymap";
import { Window } from "nsh/desktop";

function App() {
  const shown = signal(false);
  Window.setTitle("later");
  keymap.bind("n", () => {
    let left = 3;
    const step = () => {
      left -= 1;
      if (left === 0) shown.set(true);
      else requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }, { when: "always" });
  return (
    <View style={{ flex: 1 }}>
      <If when={shown()}>
        <Input autoFocus style={{ height: 30 }} />
      </If>
    </View>
  );
}
mount(App);
"#;

fn text_typed_after_a_shortcut_reaches_a_field_mounted_later(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", FIELD_AFTER_SEVERAL_TURNS, &[]).unwrap();
    let (view, cx) = owned_view(cx, code, |_| {});
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        if cx.window_title().as_deref() == Some("later") {
            cx.run_until_parked();
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the window never became ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    type_key(cx, "n");
    type_key(cx, "a");
    type_key(cx, "b");
    type_key(cx, "c");
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        if field_text(&view, cx).as_deref() == Some("abc") {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the field reads {:?}, not abc",
            field_text(&view, cx)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The same shortcut and text, then the surface clock passes the wait
/// before the field mounts. The field stays empty.
fn text_typed_after_a_shortcut_is_dropped_once_the_second_passes(cx: &mut TestAppContext) {
    let code = nsh_bundler::compile_module("App.tsx", FIELD_ON_THE_NEXT_TURN, &[]).unwrap();
    let (view, cx) = owned_view(cx, code, |_| {});
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        if cx.window_title().as_deref() == Some("ready") {
            cx.run_until_parked();
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the window never became ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    type_key(cx, "n");
    type_key(cx, "a");
    type_key(cx, "b");
    type_key(cx, "c");
    view.read_with(cx, |view, _| view.surface().advance_clock(3.0));
    cx.run_until_parked();
    assert_eq!(
        field_text(&view, cx).as_deref(),
        Some(""),
        "text older than a few seconds is not inserted"
    );
}

fn js_and_rust_view_shortcuts_fire_together_in_either_order(cx: &mut TestAppContext) {
    crate::set_ui_sets(vec![nsh_base::registry(), rust_keys::registry()]);
    let primary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let sets = [rust_keys::registry().into()];

    let code = nsh_bundler::compile_module("App.tsx", &js_and_rust_view(false), &sets).unwrap();
    let (view, window) = owned_view(cx, code, |_| {});
    wait_for(&view, window, "fired");
    window.simulate_keystrokes(&format!("{primary}-t"));
    wait_for(&view, window, "fired shown");
    window.simulate_keystrokes("escape shift-enter");
    wait_for(&view, window, "fired shown escape shift");
    // The view goes, and only its binding with it.
    window.simulate_keystrokes(&format!("{primary}-t"));
    wait_for(&view, window, "fired shown escape shift hidden");
    window.simulate_keystrokes("escape");
    wait_for(&view, window, "fired shown escape shift hidden escape");

    let code = nsh_bundler::compile_module("App.tsx", &js_and_rust_view(true), &sets).unwrap();
    let (view, window) = owned_view(cx, code, |_| {});
    wait_for(&view, window, "fired bound");
    window.simulate_keystrokes("escape shift-enter");
    wait_for(&view, window, "fired bound escape shift");
    window.simulate_keystrokes(&format!("{primary}-t"));
    wait_for(&view, window, "fired bound escape shift hidden");
    window.simulate_keystrokes("escape");
    wait_for(&view, window, "fired bound escape shift hidden escape");
}
