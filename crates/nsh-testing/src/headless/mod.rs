//! Headless runs: an app on an engine with no platform, driven by steps a
//! user's hands would take, its mount batches seen as they come.
//!
//! [`Headless`] is the driver: it starts an app on an engine with display
//! frames at 60 Hz ([`start`]), applies its batches to a model of the
//! native views ([`Screen`]) and takes steps on what they show, named by
//! selectors (`id:save`, `label:Close`, `text:+1`, `view:12`, or a bare
//! string): [`Headless::press`], [`Headless::type_text`],
//! [`Headless::submit`], [`Headless::scroll`], [`Headless::drag`], [`Headless::wait`],
//! [`Headless::wait_for`]. Each step delivers the events a platform
//! would, in its order, and fails with what is on screen when it cannot
//! be taken or when the app throws. An observer ([`Headless::observe`])
//! sees every batch with the step it came in ([`print_batch`] prints it as
//! `nsh test --trace` does). [`StubNetwork`] and [`MemoryStorage`] stand in for
//! the device's network and storage.
//!
//! On a manual clock (`EngineConfig::clock`) the app's time moves only as
//! the driver moves it: display frames come at once, one frame's time
//! apart, while the app asks for them or a native animation runs (so steps
//! finish animations without waiting for them), [`Headless::wait`] and a
//! long press move the clock by their time, and [`Headless::run_for`] /
//! [`Headless::fast_forward`] move it as a test says. What the driver
//! waits for (a step's quiet, a `waitFor`'s timeout) stays real time.
//!
//! `nsh test` runs apps this way (feature `js`: [`crate::run_file`]),
//! printing the batches under their steps with `--trace`; so do Rust tests
//! of an app's crate, which need no QuickJS for a Rust root.

mod animation;
mod drag;
#[cfg(feature = "live")]
mod live;
mod net;
pub(crate) mod screen;
pub(crate) mod selector;
mod stub;

use std::{
    io::Write,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender};
use nsh_core::{
    ApproxTextEngine, Command, Dialog, DialogButtonStyle, EventKind, EventPayload, MountBatch,
    MountItem, NodeId, PayloadValue, Point, Points, PressEvent, PressPosition, ROOT_ID, Size,
    SubmitBehavior,
    geometry::{point, px},
    keymap::Modifiers,
};
use nsh_engine::{
    AppError, AppErrorKind, Clock, Engine, EngineConfig, EngineControl, Environment, MountSink,
};

#[cfg(feature = "live")]
pub use self::live::UreqNetwork;
use self::{
    animation::{Animator, FRAME},
    screen::{Back, Pick},
    selector::Selector,
};
pub use self::{
    drag::DragTo,
    net::DirectoryAssets,
    screen::Screen,
    stub::{
        MemoryStorage, RecordingNetwork, StubHandler, StubNetwork, StubReply, StubResponse,
        StubRoute,
    },
};

/// How long [`Headless::wait_for`] and [`Headless::wait_until`] wait by
/// default, and a first render at most.
pub const WAIT: Duration = Duration::from_secs(5);
/// How long the app must render nothing after a step for it to be done.
const QUIET: Duration = Duration::from_millis(100);
/// How long a step waits for the app to be quiet at most: an app with a
/// running timer or animation never is.
const SETTLE: Duration = Duration::from_secs(2);
/// How long [`Headless::idle`] may take at most.
const MAX_IDLE: Duration = Duration::from_secs(10);
/// How far a scroll moves per display frame, in points.
const SCROLL_STEP: Points = Points(10.0);

struct ChannelSink {
    batches: Sender<MountBatch>,
    errors: Sender<AppError>,
    /// Set once the engine runs; drives `requestAnimationFrame` at 60 Hz.
    control: Arc<OnceLock<EngineControl>>,
    /// On a manual clock: the engine asked for a frame, which the driver
    /// gives it when it moves the clock ([`Headless`]).
    wanted: Option<Arc<AtomicBool>>,
}

impl MountSink for ChannelSink {
    fn submit(&self, batch: MountBatch) {
        let _ = self.batches.send(batch);
    }

    fn report(&self, error: &AppError) {
        let _ = self.errors.send(error.clone());
    }

    fn request_frame(&self) {
        if let Some(wanted) = &self.wanted {
            wanted.store(true, Ordering::Relaxed);
            return;
        }
        let control = self.control.clone();
        std::thread::spawn(move || {
            std::thread::sleep(FRAME);
            if let Some(control) = control.get() {
                control.frame();
            }
        });
    }
}

/// Starts `config`'s app on an engine with no platform that gets display
/// frames at 60 Hz while it asks for them, as on screen (timers,
/// animations and what waits for a frame run), laying text out with
/// [`ApproxTextEngine`]: the engine, and the batches it sends.
pub fn start(config: EngineConfig) -> std::io::Result<(Engine, Receiver<MountBatch>)> {
    launch(config).map(|launched| (launched.engine, launched.batches))
}

/// A started engine and what it sends.
struct Launched {
    engine: Engine,
    batches: Receiver<MountBatch>,
    errors: Receiver<AppError>,
    /// On a manual clock: it asked for a frame.
    wanted: Option<Arc<AtomicBool>>,
}

fn launch(config: EngineConfig) -> std::io::Result<Launched> {
    let (batches, batches_rx) = crossbeam_channel::unbounded();
    let (errors, errors_rx) = crossbeam_channel::unbounded();
    let control = Arc::new(OnceLock::new());
    let wanted = config.clock.is_manual().then(Arc::default);
    let sink = ChannelSink {
        batches,
        errors,
        control: control.clone(),
        wanted: wanted.clone(),
    };
    let engine = Engine::start(config, || ApproxTextEngine, sink)?;
    let _ = control.set(engine.control());
    Ok(Launched {
        engine,
        batches: batches_rx,
        errors: errors_rx,
        wanted,
    })
}

/// Sees each batch a [`Headless`] applies, with the step it came in.
type Observer = Box<dyn FnMut(&str, &MountBatch) + Send>;

/// Answers a dialog ([`Headless::on_dialog`]).
type DialogHandler = Box<dyn FnMut(&Dialog) -> DialogAnswer + Send>;

/// The answer to a dialog (`AlertDialog.show`, an action sheet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogAnswer {
    /// Press the button with this text.
    Press(String),
    /// Close it without a button (tapping outside, the back button).
    Dismiss,
}

/// Who answers the dialogs a step shows.
enum Dialogs {
    /// [`default_answer`].
    Default,
    /// A test's handler.
    Handler(DialogHandler),
    /// The caller, after the step ([`Headless::answer_dialog`]).
    Caller,
}

/// What [`Headless::pump`] saw.
enum Pumped {
    /// A batch, applied.
    Batch,
    /// The app threw.
    Error,
    /// The deadline passed, or a frame on the manual clock brought
    /// nothing.
    Timeout,
    /// The app left: nothing more comes.
    Exited,
}

/// An app on a headless engine ([`start`]) and what it shows ([`Screen`]),
/// driven by steps: press views, type into inputs, scroll, go back, wait
/// for what it shows.
///
/// ```ignore
/// let mut app = Headless::start(EngineConfig::new(Root::of::<my_app::App>())).unwrap();
/// app.wait_for("Count: 0")?;
/// app.press("Tap")?;
/// app.wait_for("Count: 1")?;
/// ```
///
/// A step takes the batches that came since the last one first (they
/// belong to it), then names itself the step of the batches that follow:
/// what [`Headless::observe`] sees. Steps that act on the app return once
/// it rendered nothing for 100 ms and no native animation runs (2 s at
/// most), and fail when the app threw meanwhile (an exception that
/// escaped a callback, a bundle that did not load, a VM that was stopped)
/// or asked what only a real platform answers (an image's size).
///
/// It plays the platform's part the NSH protocol defines and nothing
/// else: the native animation graph runs frame by frame (nsh-core's `animation`),
/// a back pops screens and closes modals, dialogs are answered (by the
/// test, else [`default_answer`]), the clipboard holds text. Images are
/// not loaded: no `onLoad` comes.
pub struct Headless {
    engine: Engine,
    batches: Receiver<MountBatch>,
    errors: Receiver<AppError>,
    /// The app's errors seen while waiting, for the next check.
    seen: Vec<AppError>,
    screen: Screen,
    animator: Animator,
    dialogs: Dialogs,
    step: String,
    applied: u64,
    /// The app left (`process.exit`, `Command::ExitApp`): no more batches
    /// come, and waits end.
    exited: bool,
    /// The surface's environment and size, as the platform last reported.
    environment: Environment,
    viewport: Size<Points>,
    observer: Option<Observer>,
    /// The app's clock.
    clock: Clock,
    /// On a manual clock: the engine asked for a frame.
    wanted: Option<Arc<AtomicBool>>,
}

impl Headless {
    /// Starts `config`'s app ([`start`]).
    pub fn start(config: EngineConfig) -> std::io::Result<Headless> {
        let environment = config.environment;
        let viewport = config.viewport;
        let clock = config.clock.clone();
        let Launched {
            engine,
            batches,
            errors,
            wanted,
        } = launch(config)?;
        Ok(Headless {
            engine,
            batches,
            errors,
            seen: Vec::new(),
            screen: Screen::default(),
            animator: Animator::new(clock.clone()),
            dialogs: Dialogs::Default,
            step: "open".to_owned(),
            applied: 0,
            exited: false,
            environment,
            viewport,
            observer: None,
            clock,
            wanted,
        })
    }

    /// The app's clock (`EngineConfig::clock`).
    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    /// The step `clock.runFor(<ms>)`: moves the app's manual clock by
    /// `time`, a display frame at a time (timers, frames and native
    /// animations run as they come due), then settles.
    pub fn run_for(&mut self, time: Duration) -> Result<(), String> {
        self.manual_clock("runFor")?;
        self.step(format!("clock.runFor({})", time.as_millis()));
        self.elapse(time);
        self.done()
    }

    /// The step `clock.fastForward(<ms>)`: moves the app's manual clock by
    /// `time` at once, as a device waking from sleep does: the timers due
    /// meanwhile run once each, then the app settles.
    pub fn fast_forward(&mut self, time: Duration) -> Result<(), String> {
        self.manual_clock("fastForward")?;
        self.step(format!("clock.fastForward({})", time.as_millis()));
        self.clock.advance_to(self.clock.now() + time);
        self.advance(Duration::ZERO);
        self.done()
    }

    /// Sets the wall time of the app's manual clock (`Date.now()`) to `at`,
    /// without moving its timers.
    pub fn set_system_time(&mut self, at: std::time::SystemTime) -> Result<(), String> {
        self.manual_clock("setSystemTime")?;
        self.clock.set_system_time(at);
        Ok(())
    }

    /// An error unless the app's clock is manual.
    fn manual_clock(&self, what: &str) -> Result<(), String> {
        if self.clock.is_manual() {
            Ok(())
        } else {
            Err(format!(
                "clock.{what} needs a manual clock: open the app with one (`open(entry, {{ clock: {{}} }})`)"
            ))
        }
    }

    /// Moves the manual clock by `by` and waits until the engine got
    /// there (its batches sent).
    fn advance(&self, by: Duration) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.engine.control().advance_clock(by, move || {
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(MAX_IDLE);
    }

    /// One display frame on the manual clock, `by` after the last: the
    /// clock moves, then the frame comes if the engine asked for one, and
    /// the native animations take it.
    fn frame(&mut self, by: Duration) {
        self.advance(by);
        if self
            .wanted
            .as_ref()
            .is_some_and(|w| w.swap(false, Ordering::Relaxed))
        {
            self.engine.control().frame();
            self.advance(Duration::ZERO);
        }
        self.animator.tick();
        self.report_finished();
    }

    /// Whether, on a manual clock, the app wants frames: it asked for one,
    /// or a native animation runs.
    fn wants_frames(&self) -> bool {
        self.wanted
            .as_ref()
            .is_some_and(|w| w.load(Ordering::Relaxed))
            || (self.clock.is_manual() && self.animator.running())
    }

    /// Lets the app's time pass by `time`: real time on the system's clock,
    /// the manual clock moved a frame at a time otherwise; batches are
    /// applied as they come.
    fn elapse(&mut self, time: Duration) {
        if !self.clock.is_manual() {
            let end = Instant::now() + time;
            while Instant::now() < end {
                if let Pumped::Exited = self.pump(end) {
                    return;
                }
            }
            return;
        }
        let end = self.clock.now() + time;
        while !self.exited && self.clock.now() < end {
            let by = end.saturating_duration_since(self.clock.now()).min(FRAME);
            self.frame(by);
            self.drain();
        }
    }

    /// The step `setViewport(<w>, <h>)`: the surface becomes `size` (a
    /// rotation, a resized window), as a platform reports it.
    pub fn set_viewport(&mut self, size: Size<Points>) -> Result<(), String> {
        self.step(format!("setViewport({}, {})", size.width, size.height));
        self.viewport = size;
        self.environment.window = size;
        self.engine.control().resize(size);
        self.engine.control().set_environment(self.environment);
        self.done()
    }

    /// The step `setEnvironment`: the surface's environment changes as
    /// `change` says (dark mode, the locale), as a platform reports it.
    pub fn set_environment(
        &mut self,
        what: &str,
        change: impl FnOnce(&mut Environment),
    ) -> Result<(), String> {
        self.step(what);
        change(&mut self.environment);
        self.engine.control().set_environment(self.environment);
        self.done()
    }

    /// The engine: [`Engine::control`] sends it input.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// What is on screen, from the batches applied so far.
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// The engine and the batches not applied yet, for a model of the
    /// test's own.
    pub fn into_parts(self) -> (Engine, Receiver<MountBatch>) {
        (self.engine, self.batches)
    }

    /// Calls `observer` with each batch applied from now on and the step
    /// it came in (`open` until the first step).
    pub fn observe(&mut self, observer: impl FnMut(&str, &MountBatch) + Send + 'static) {
        self.observer = Some(Box::new(observer));
    }

    /// Answers the dialogs steps show with `handler`, instead of
    /// [`default_answer`].
    pub fn on_dialog(&mut self, handler: impl FnMut(&Dialog) -> DialogAnswer + Send + 'static) {
        self.dialogs = Dialogs::Handler(Box::new(handler));
    }

    /// Leaves the dialogs steps show to the caller: a step returns with
    /// one shown ([`Headless::pending_dialog`]) until
    /// [`Headless::answer_dialog`] answers it.
    pub fn answer_dialogs_myself(&mut self) {
        self.dialogs = Dialogs::Caller;
    }

    /// The oldest dialog shown and not answered yet.
    pub fn pending_dialog(&self) -> Option<&Dialog> {
        self.screen.pending_dialog()
    }

    /// Answers the oldest dialog not answered yet with `answer`, as the
    /// user would, then waits until the app is quiet; an error for a
    /// button it has not, or a dialog that cannot be dismissed.
    pub fn answer_dialog(&mut self, answer: DialogAnswer) -> Result<(), String> {
        self.answer(answer)?;
        self.done()
    }

    /// Puts `text` on the clipboard (`None`: nothing), as the user copying.
    pub fn set_clipboard(&mut self, text: Option<String>) {
        self.screen.set_clipboard(text);
    }

    /// Takes the batches that came, then names the step of those that
    /// follow.
    pub fn step(&mut self, name: impl Into<String>) {
        self.drain();
        self.step = name.into();
    }

    /// The batches applied so far.
    pub fn applied(&self) -> u64 {
        self.applied
    }

    fn apply(&mut self, batch: &MountBatch) {
        self.screen.apply(batch);
        self.exited |= batch.items.iter().any(|item| {
            matches!(
                item,
                MountItem::Command {
                    command: Command::ExitApp,
                    ..
                }
            )
        });
        for item in &batch.items {
            self.animator.apply(item);
        }
        self.applied += 1;
        if let Some(observer) = &mut self.observer {
            observer(&self.step, batch);
        }
        self.react();
    }

    /// Does what a platform does after a batch: reports where commands
    /// moved scroll views, tells modals they show, answers clipboard reads,
    /// goes back when the app asked, reports finished animations.
    fn react(&mut self) {
        let control = self.engine.control();
        for id in self.screen.take_scrolled() {
            if let Some(scroll) = self.screen.scroll(id) {
                let (x, y) = (scroll.x, scroll.y);
                control.scroll(id, point(x, y), scroll.applied);
                self.animator.scrolled(id, point(x, y));
            }
        }
        for modal in self.screen.take_presented() {
            self.send(modal, EventKind::Show, EventPayload::None);
        }
        for (id, has_only) in self.screen.take_clipboard_reads() {
            let text = self.screen.clipboard().map(str::to_owned);
            let mut fields = vec![
                ("id", PayloadValue::Number(f64::from(id))),
                ("has", PayloadValue::Bool(text.is_some())),
            ];
            if let Some(text) = text.filter(|_| !has_only) {
                fields.push(("text", PayloadValue::Str(text)));
            }
            control.dispatch_event(ROOT_ID, EventKind::Clipboard, EventPayload::Map(fields));
        }
        if self.screen.take_back_asked() {
            self.go_back();
        }
        self.report_finished();
    }

    fn report_finished(&mut self) {
        let finished = self.animator.take_finished();
        if !finished.is_empty() {
            self.engine.control().animations_finished(finished);
        }
    }

    /// Applies the batches that came, without waiting: the screen as it
    /// is now.
    pub fn catch_up(&mut self) {
        self.drain();
    }

    /// Applies the batches that came, without waiting.
    fn drain(&mut self) {
        while let Ok(batch) = self.batches.try_recv() {
            self.apply(&batch);
        }
        self.animator.tick();
        self.report_finished();
    }

    /// Applies the next batch, waiting until `deadline` at most, while the
    /// native animations run frame by frame; the app's errors are kept for
    /// the next check.
    fn pump(&mut self, deadline: Instant) -> Pumped {
        loop {
            if self.exited {
                return Pumped::Exited;
            }
            // On a manual clock a wanted frame comes at once; with no
            // batch from it, the caller looks again (and bounds the frames).
            let framed = self.wants_frames();
            if framed {
                self.frame(FRAME);
            }
            let wake = match self.animator.next_frame() {
                _ if framed => Instant::now(),
                Some(frame) if !self.clock.is_manual() => frame.min(deadline),
                _ => deadline,
            };
            let timeout = wake.saturating_duration_since(Instant::now());
            crossbeam_channel::select_biased! {
                recv(self.errors) -> error => {
                    if let Ok(error) = error {
                        self.seen.push(error);
                        return Pumped::Error;
                    }
                }
                recv(self.batches) -> batch => match batch {
                    Ok(batch) => {
                        self.apply(&batch);
                        return Pumped::Batch;
                    }
                    Err(_) => return Pumped::Timeout,
                },
                default(timeout) => {}
            }
            if framed {
                return Pumped::Timeout;
            }
            self.animator.tick();
            self.report_finished();
            if Instant::now() >= deadline {
                return Pumped::Timeout;
            }
        }
    }

    /// Applies the next batch, waiting `timeout` at most: the batch, or
    /// `None` when none came.
    pub fn next_batch(&mut self, timeout: Duration) -> Option<MountBatch> {
        if self.exited {
            return None;
        }
        let deadline = Instant::now() + timeout;
        let wait = deadline.saturating_duration_since(Instant::now());
        let batch = self.batches.recv_timeout(wait).ok()?;
        self.apply(&batch);
        Some(batch)
    }

    /// Applies batches until none comes for `quiet` and no native
    /// animation runs, or for `limit` at most: false if the limit ended it.
    fn quiet(&mut self, quiet: Duration, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        // Frames on a manual clock come at once: the limit is its time too.
        let clock_end = self.clock.now() + limit;
        let mut last = Instant::now();
        loop {
            if self.clock.is_manual() && self.clock.now() >= clock_end {
                return false;
            }
            match self.pump((last + quiet).min(end)) {
                Pumped::Batch => last = Instant::now(),
                Pumped::Error | Pumped::Exited => return true,
                Pumped::Timeout => {
                    let now = Instant::now();
                    if !self.animator.running() && now >= last + quiet {
                        return true;
                    }
                    if now >= end {
                        return false;
                    }
                }
            }
        }
    }

    /// Applies batches until none comes for 100 ms and no native animation
    /// runs, or for 2 s at most (an app updating all the time never goes
    /// quiet): false if the limit ended it.
    pub fn settle(&mut self) -> bool {
        self.quiet(QUIET, SETTLE)
    }

    /// The step `idle`: applies batches until none comes for `quiet`, or
    /// for 10 s at most (false then). Long enough, it lets the engine's
    /// idle collection run (every second at most), so what the app keeps
    /// alive shows.
    pub fn idle(&mut self, quiet: Duration) -> Result<bool, String> {
        self.step("idle");
        let quiet = self.quiet(quiet, MAX_IDLE);
        self.check().map(|()| quiet)
    }

    /// Fails with the app's first error since the last check (an asset's
    /// failure is the log's only), or with what it asked that only a real
    /// platform answers.
    fn check(&mut self) -> Result<(), String> {
        while let Ok(error) = self.errors.try_recv() {
            self.seen.push(error);
        }
        for error in std::mem::take(&mut self.seen) {
            failure(error)?;
        }
        if let Some(what) = self.screen.take_unsupported().first() {
            return Err(format!(
                "the app asked for {what}, which only a real platform answers: headless runs have none"
            ));
        }
        Ok(())
    }

    /// Settles after a step, answers the dialogs it showed (unless the
    /// caller answers them), and fails if the app threw.
    fn done(&mut self) -> Result<(), String> {
        self.settle();
        self.check()?;
        loop {
            let answer = match (&mut self.dialogs, self.screen.pending_dialog()) {
                (_, None) | (Dialogs::Caller, _) => return Ok(()),
                (Dialogs::Default, Some(dialog)) => default_answer(dialog)?,
                (Dialogs::Handler(handler), Some(dialog)) => handler(dialog),
            };
            self.answer(answer)?;
            self.settle();
            self.check()?;
        }
    }

    /// Answers the oldest dialog not answered yet.
    fn answer(&mut self, answer: DialogAnswer) -> Result<(), String> {
        let dialog = self.screen.pending_dialog().ok_or("no dialog to answer")?;
        let index = match &answer {
            DialogAnswer::Press(text) => dialog
                .buttons
                .iter()
                .position(|b| b.text == *text)
                .ok_or_else(|| {
                    let buttons: Vec<&str> =
                        dialog.buttons.iter().map(|b| b.text.as_str()).collect();
                    format!(
                        "the dialog {} has no button {text:?}: {buttons:?}",
                        describe(dialog)
                    )
                })? as f64,
            DialogAnswer::Dismiss if dialog.cancelable => -1.0,
            DialogAnswer::Dismiss => {
                return Err(format!(
                    "the dialog {} cannot be dismissed: press one of its buttons",
                    describe(dialog)
                ));
            }
        };
        let id = f64::from(dialog.id);
        self.screen.answered();
        self.engine.control().dispatch_event(
            ROOT_ID,
            EventKind::Dialog,
            EventPayload::Map(vec![
                ("id", PayloadValue::Number(id)),
                ("index", PayloadValue::Number(index)),
            ]),
        );
        Ok(())
    }

    /// The step `back`: goes back as the user does (a header's back button,
    /// the back key): in the newest modal, else the whole screen, the
    /// innermost stack that can go back tells its top page (`onBackPress`,
    /// when it listens) and pops it (`onDismiss`) unless the page keeps the
    /// user (`popOnBack={false}`); without one, the newest dialog gets a
    /// close attempt (`onCancel`, reason `"cancel"`) and closes when
    /// `closeOnEscape`. Fails when there is nothing to go back from.
    pub fn back(&mut self) -> Result<(), String> {
        self.step("back");
        if !self.go_back() {
            return Err(format!("nothing to go back from; {}", self.on_screen()));
        }
        self.done()
    }

    /// Goes back as a platform does; false when nothing could.
    fn go_back(&mut self) -> bool {
        let control = self.engine.control();
        match self.screen.back_target() {
            Back::Pop(screen) => {
                // Platforms tell a page that listens of every back press.
                if self.screen.hears(screen, EventKind::BackRequest) {
                    control.dispatch_event(screen, EventKind::BackRequest, EventPayload::None);
                }
                self.screen.pop(screen);
                control.dispatch_event(screen, EventKind::Dismiss, EventPayload::None);
            }
            Back::Ask(screen) => {
                control.dispatch_event(screen, EventKind::BackRequest, EventPayload::None);
            }
            Back::Close {
                modal,
                close_on_escape,
            } => {
                let reason =
                    EventPayload::Map(vec![("reason", PayloadValue::Str("cancel".into()))]);
                control.dispatch_event(modal, EventKind::RequestClose, reason);
                if close_on_escape {
                    self.screen.close(modal);
                    control.dispatch_event(modal, EventKind::Dismiss, EventPayload::None);
                }
            }
            Back::Nothing => return false,
        }
        true
    }

    /// The texts on screen, for errors.
    fn on_screen(&self) -> String {
        format!("on screen: {:?}", self.screen.texts())
    }

    /// The first render: waits for the app's first batch ([`WAIT`] at
    /// most), then until it is quiet.
    pub fn first_render(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + WAIT;
        while self.applied == 0 {
            match self.pump(deadline) {
                Pumped::Batch => {}
                Pumped::Error => self.check()?,
                Pumped::Exited => return self.check(),
                Pumped::Timeout => {
                    if Instant::now() >= deadline {
                        self.check()?;
                        return Err(format!("the app rendered nothing in {} s", WAIT.as_secs()));
                    }
                }
            }
        }
        self.done()
    }

    /// The step `wait(<ms>)`: applies batches for `time` (timers and
    /// animations run).
    pub fn wait(&mut self, time: Duration) -> Result<(), String> {
        self.step(format!("wait({})", time.as_millis()));
        self.elapse(time);
        self.check()
    }

    /// Applies batches until `done` holds for the screen, `timeout` at
    /// most; the error names `what` and the texts on screen.
    pub fn wait_until(
        &mut self,
        what: &str,
        timeout: Duration,
        done: impl Fn(&Screen) -> bool,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        self.drain();
        self.check()?;
        while !done(&self.screen) {
            match self.pump(deadline) {
                Pumped::Batch => {}
                Pumped::Error => self.check()?,
                Pumped::Exited => {
                    return Err(format!(
                        "{what} never happened: the app left; {}",
                        self.on_screen()
                    ));
                }
                Pumped::Timeout => {
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "{what} never happened in {} ms; {}",
                            timeout.as_millis(),
                            self.on_screen()
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// The step `waitFor(<selector>)`: applies batches until a view on
    /// screen matches `selector` ([`Screen::matches`]), for [`WAIT`] at
    /// most.
    pub fn wait_for(&mut self, selector: &str) -> Result<(), String> {
        self.wait_for_within(selector, WAIT)
    }

    /// [`Headless::wait_for`], waiting `timeout` at most.
    pub fn wait_for_within(&mut self, selector: &str, timeout: Duration) -> Result<(), String> {
        Selector::parse(selector)?;
        self.step(format!("waitFor({selector})"));
        self.wait_until(&format!("`{selector}` showing"), timeout, |s| {
            s.matches(selector) == Ok(true)
        })
    }

    /// The step `waitFor(<selector>, gone)`: applies batches until no view
    /// on screen matches `selector`, `timeout` at most.
    pub fn wait_for_gone(&mut self, selector: &str, timeout: Duration) -> Result<(), String> {
        Selector::parse(selector)?;
        self.step(format!("waitFor({selector}, gone)"));
        self.wait_until(&format!("`{selector}` gone"), timeout, |s| {
            s.matches(selector) == Ok(false)
        })
    }

    /// Applies the next batch, or runs the native animations, for `timeout`
    /// at most: what a test polling a condition waits for between looks.
    pub fn next_change(&mut self, timeout: Duration) -> Result<(), String> {
        if let Pumped::Error = self.pump(Instant::now() + timeout) {
            self.check()?;
        }
        self.check()
    }

    /// The one view `pick` gave for `selector`, or the error a step on
    /// `what` fails with.
    fn picked(&self, pick: Pick, selector: &str, what: &str) -> Result<NodeId, String> {
        match pick {
            Pick::One(id) => Ok(id),
            Pick::None => Err(format!(
                "no {what} matches `{selector}`; {}",
                self.on_screen()
            )),
            Pick::Many(n) => Err(format!(
                "`{selector}` matches {n} {what}s: say which (`{selector} >> nth=0`), or look inside \
                 a view around it (`id:list >> {selector}`)"
            )),
            Pick::Disabled => Err(format!("`{selector}` is disabled; {}", self.on_screen())),
        }
    }

    /// Whether a view on screen shows the text `text`.
    pub fn shows(&self, text: &str) -> bool {
        self.screen.shows(text)
    }

    /// The step `press(<selector>)`: presses what `selector` names, as a
    /// tap does ([`Screen::press_target`]: `"Save"` presses the button
    /// whose test id, label or text it is), sending the view's
    /// `onPressDown`, `onPressUp` and `onPress`. They carry a click count
    /// of 1 and the pressed view's center ([`EventPayload::Press`]).
    pub fn press(&mut self, selector: &str) -> Result<(), String> {
        let step = format!("press({selector})");
        self.press_step(selector, step, Modifiers::default(), None)
    }

    /// The step `press(<selector>)` with `modifiers` held, as a single
    /// click with those keys down: each event carries them, a click count
    /// of 1 and the pressed view's center ([`EventPayload::Press`]).
    pub fn press_with(&mut self, selector: &str, modifiers: Modifiers) -> Result<(), String> {
        let step = format!("press({selector}, {modifiers:?})");
        self.press_step(selector, step, modifiers, None)
    }

    /// The step `pressAt(<selector>, <x>, <y>)`: [`Headless::press`] with
    /// the pointer at `at` in the view `selector` names (its top left
    /// corner, as [`nsh_core::PressPosition::location`]): the events tell
    /// the view that hears them where that is in it and in the surface.
    pub fn press_at(&mut self, selector: &str, at: Point<Points>) -> Result<(), String> {
        let step = format!("pressAt({selector}, {}, {})", at.x.0, at.y.0);
        self.press_step(selector, step, Modifiers::default(), Some(at))
    }

    /// A press on what `selector` names with `modifiers` held, the pointer
    /// at `at` in the view it names (else the pressed view's center).
    fn press_step(
        &mut self,
        selector: &str,
        step: String,
        modifiers: Modifiers,
        at: Option<Point<Points>>,
    ) -> Result<(), String> {
        let parsed = Selector::parse(selector)?;
        self.step(step);
        let pick = self.screen.pressable(&parsed);
        let id = self.picked(pick, selector, "pressable view")?;
        let page = match at {
            Some(at) => {
                let view = self.picked(self.screen.view(&parsed), selector, "view")?;
                self.screen.page_bounds(view).map(|b| b.origin + at)
            }
            None => self.screen.page_bounds(id).map(|b| b.center()),
        };
        let event = PressEvent {
            modifiers,
            clicks: 1,
            position: self.press_position(id, page),
        };
        for kind in [EventKind::PressDown, EventKind::PressUp, EventKind::Press] {
            self.send(id, kind, EventPayload::Press(event));
        }
        self.done()
    }

    /// The position of a press at `page` (surface coordinates) in `view`.
    fn press_position(&self, view: NodeId, page: Option<Point<Points>>) -> Option<PressPosition> {
        let page = page?;
        let mut position = PressPosition::default();
        position.location = page - self.screen.page_bounds(view)?.origin;
        position.page = page;
        Some(position)
    }

    /// The step `pressSpan(<selector>)`: presses the characters of a text
    /// view `selector` names ([`Screen::span_style`]'s selectors: `id:` a
    /// nested `<Text>`'s test id, `text:` the characters), as a tap on its
    /// first character does: the innermost pressable span there hears
    /// `onPressDown`, `onPressUp` and `onPress` (the platforms' link hit
    /// testing), else the text view or its nearest pressable ancestor.
    /// The events carry the text view's center, in the view that hears
    /// them (the text view for a span), as the pointer's position.
    pub fn press_span(&mut self, selector: &str) -> Result<(), String> {
        let parsed = Selector::parse(selector)?;
        self.step(format!("pressSpan({selector})"));
        let span = self
            .screen
            .span(&parsed)
            .map_err(|e| format!("{e}; {}", self.on_screen()))?;
        let pick = self.screen.span_press_target(span);
        let id = self.picked(pick, selector, "pressable span")?;
        let page = self.screen.page_bounds(span.view).map(|b| b.center());
        let holder = match self.screen.page_bounds(id) {
            Some(_) => id,
            None => span.view,
        };
        let event = PressEvent {
            clicks: 1,
            position: self.press_position(holder, page),
            ..PressEvent::default()
        };
        for kind in [EventKind::PressDown, EventKind::PressUp, EventKind::Press] {
            self.send(id, kind, EventPayload::Press(event));
        }
        self.done()
    }

    /// The step `longPress(<selector>)`: holds a finger on what `selector`
    /// names (or the nearest view around it that hears long presses or
    /// presses) for its `delayLongPress` (500 ms), as a long press does:
    /// `onPressDown`, `onLongPress`, then `onPressUp`; no `onPress` follows.
    /// They carry the view's center as the pointer's position.
    pub fn long_press(&mut self, selector: &str) -> Result<(), String> {
        let parsed = Selector::parse(selector)?;
        self.step(format!("longPress({selector})"));
        let pick = self.screen.long_pressable(&parsed);
        let id = self.picked(pick, selector, "pressable view")?;
        let delay = self.screen.long_press_delay(id).unwrap_or(500.0).max(0.0);
        let page = self.screen.page_bounds(id).map(|b| b.center());
        let event = EventPayload::Press(PressEvent {
            clicks: 1,
            position: self.press_position(id, page),
            ..PressEvent::default()
        });
        self.send(id, EventKind::PressDown, event.clone());
        self.elapse(Duration::from_secs_f64(f64::from(delay) / 1000.0));
        self.send(id, EventKind::LongPress, event.clone());
        self.send(id, EventKind::PressUp, event);
        self.done()
    }

    /// The step `clear(<selector>)`: empties the input `selector` names, as
    /// selecting all and deleting does: one `onKeyPress` of `Backspace` and
    /// an edit to nothing.
    pub fn clear(&mut self, selector: &str) -> Result<(), String> {
        let id = self.input_step(selector, format!("clear({selector})"))?;
        self.key_press(id, "Backspace");
        if let Some(input) = self.screen.input_mut(id)
            && !input.text.is_empty()
        {
            input.text.clear();
            (input.anchor, input.caret) = (0, 0);
            input.count += 1;
            let payload = EventPayload::Text {
                text: String::new(),
                count: input.count,
            };
            self.engine
                .control()
                .dispatch_event(id, EventKind::ChangeText, payload);
        }
        self.done()
    }

    /// The step `type(<selector>, <text>)`: types `text` at the caret of
    /// the input `selector` names (the matching view, or the first input
    /// in it; `text:` also matches what an input holds or its
    /// placeholder), over its selection: the end of its text unless the
    /// app ([`nsh_core::ui::Input::selected_range`], `setSelection`) or a
    /// [`Headless::select`] step moved it. As a keyboard does: the input
    /// takes the focus, then each character is a key (`onKeyPress`), an
    /// edit (`onChange`) and the caret after it (`onSelectionChange`); a
    /// line break in a single-line input is the return key
    /// ([`Headless::submit`]). A character past `maxLength` is dropped.
    pub fn type_text(&mut self, selector: &str, text: &str) -> Result<(), String> {
        let id = self.input_step(selector, format!("type({selector}, {text:?})"))?;
        for c in text.chars() {
            let multiline = self.screen.input(id).is_some_and(|i| i.config.multiline);
            if c == '\n' && !multiline {
                self.return_key(id);
            } else {
                self.key(id, c);
            }
        }
        self.done()
    }

    /// The step `submit(<selector>)`: the return key in the input
    /// `selector` names, as its `submitBehavior` says: `onSubmit`
    /// (and the focus leaves it, by default in a single line), or a line
    /// break in a multiline input.
    pub fn submit(&mut self, selector: &str) -> Result<(), String> {
        let id = self.input_step(selector, format!("submit({selector})"))?;
        self.return_key(id);
        self.done()
    }

    /// Starts a step on the input `selector` names and focuses it.
    fn input_step(&mut self, selector: &str, step: String) -> Result<NodeId, String> {
        let parsed = Selector::parse(selector)?;
        self.step(step);
        let pick = self.screen.input_view(&parsed);
        let id = self.picked(pick, selector, "input")?;
        if self.screen.input(id).is_some_and(|i| !i.config.editable) {
            return Err(format!("the input `{selector}` is not editable"));
        }
        if self.screen.focused() != Some(id) {
            if let Some(old) = self.screen.focused() {
                self.send(old, EventKind::Blur, EventPayload::None);
            }
            self.screen.set_focused(Some(id));
            self.send(id, EventKind::Focus, EventPayload::None);
        }
        Ok(id)
    }

    /// Sends `kind` to view `id` if it reports it, as platforms do.
    fn send(&self, id: NodeId, kind: EventKind, payload: EventPayload) {
        if self.screen.hears(id, kind) {
            self.engine.control().dispatch_event(id, kind, payload);
        }
    }

    fn key_press(&self, id: NodeId, key: &str) {
        self.send(
            id,
            EventKind::KeyPress,
            EventPayload::Map(vec![("key", PayloadValue::Str(key.to_owned()))]),
        );
    }

    /// The step `select(<selector>, <start>, <end>)`: the user selects
    /// `start..end` (UTF-16 units, a caret when equal; clamped to the
    /// text) of the input `selector` names, which takes the focus:
    /// `onSelectionChange` reports it, and typing replaces it.
    pub fn select(&mut self, selector: &str, start: u32, end: u32) -> Result<(), String> {
        let id = self.input_step(selector, format!("select({selector}, {start}, {end})"))?;
        if let Some(input) = self.screen.input_mut(id) {
            let len = input.len();
            (input.anchor, input.caret) = (start.min(len), end.min(len));
        }
        self.report_selection(id);
        self.done()
    }

    /// The step `key(<keystroke>)`: the user presses `keystroke` (`"mod+z"`,
    /// `"ctrl+shift+up"`, `"left"`, `"delete"`, `"tab"`, `"a"`; the syntax
    /// of `nsh/keymap`), as the desktop platforms take a key: the app's
    /// key bindings first (`nsh/keymap`, [`nsh_core::ui::on_key`]), whose
    /// match fires the root's `KeyCommand` and takes the key; else the
    /// focused input, which reports the arrows, Delete, Escape and Tab
    /// (`onKeyPress`, with the modifiers) and then acts: Left and Right
    /// move the caret a character (Shift extends the selection), Home and
    /// End to the text's edges, Backspace and Delete delete (reported as
    /// `Backspace`), Enter is the return key, `mod+a` selects all, and a
    /// character key types (Shift for a capital). Up and Down only report,
    /// Escape and Tab do not move the focus. With neither, nothing.
    pub fn press_key(&mut self, keystroke: &str) -> Result<(), String> {
        let apple =
            nsh_core::keymap::apple_keys(self.environment.os, self.environment.apple_keyboard);
        let stroke = nsh_core::keymap::Keystroke::parse_with(keystroke, apple)
            .map_err(|e| format!("key({keystroke}): {e}"))?;
        self.step(format!("key({keystroke})"));
        let focused = self
            .screen
            .focused()
            .filter(|&id| self.screen.input(id).is_some());
        let found = self
            .screen
            .keymap()
            .map_or(nsh_core::keymap::KeyMatch::None, |k| {
                k.find(stroke, focused.is_some(), false)
            });
        if let nsh_core::keymap::KeyMatch::Fire(binding) = found {
            self.engine.control().dispatch_event(
                nsh_core::ROOT_ID,
                EventKind::KeyCommand,
                EventPayload::Value(PayloadValue::Number(f64::from(binding))),
            );
        }
        if let (false, Some(id)) = (found.consumed(), focused) {
            self.input_key(id, stroke, apple);
        }
        self.done()
    }

    /// What the focused input `id` does with `stroke` ([`Self::press_key`]).
    fn input_key(&mut self, id: NodeId, stroke: nsh_core::keymap::Keystroke, apple: bool) {
        use nsh_core::keymap::Key;
        let m = stroke.modifiers;
        let command = if apple { m.cmd } else { m.ctrl };
        let named = match stroke.key {
            Key::Left => Some("ArrowLeft"),
            Key::Right => Some("ArrowRight"),
            Key::Up => Some("ArrowUp"),
            Key::Down => Some("ArrowDown"),
            Key::Delete => Some("Delete"),
            Key::Escape => Some("Escape"),
            Key::Tab => Some("Tab"),
            _ => None,
        };
        if let Some(name) = named {
            self.key_press_with(id, name, m);
        }
        let Some(input) = self.screen.input(id) else {
            return;
        };
        let editable = input.config.editable;
        let (start, end) = input.selection();
        let len = input.len();
        match stroke.key {
            Key::Left | Key::Right | Key::Home | Key::End => {
                let forward = matches!(stroke.key, Key::Right | Key::End);
                let caret = match stroke.key {
                    Key::Home => 0,
                    Key::End => len,
                    _ if start != end && !m.shift => {
                        if forward {
                            end
                        } else {
                            start
                        }
                    }
                    _ => input.step(input.caret, forward),
                };
                let anchor = if m.shift { input.anchor } else { caret };
                if let Some(input) = self.screen.input_mut(id) {
                    (input.anchor, input.caret) = (anchor, caret);
                }
                self.report_selection(id);
            }
            Key::Backspace | Key::Delete if editable => {
                let forward = stroke.key == Key::Delete;
                let (from, to) = if start != end {
                    (start, end)
                } else if forward {
                    (start, input.step(start, true))
                } else {
                    (input.step(start, false), start)
                };
                if from < to || len == 0 {
                    self.key_press_with(id, "Backspace", m);
                }
                if from < to {
                    self.edit(id, from, to, "");
                }
            }
            Key::Enter => self.return_key(id),
            Key::Char('a') if command => {
                if let Some(input) = self.screen.input_mut(id) {
                    (input.anchor, input.caret) = (0, len);
                }
                self.report_selection(id);
            }
            Key::Char(c) if !command && !m.alt && editable => {
                let c = if m.shift { c.to_ascii_uppercase() } else { c };
                self.key(id, c);
            }
            _ => {}
        }
    }

    /// Reports input `id`'s selection, if it listens.
    fn report_selection(&self, id: NodeId) {
        let Some((start, end)) = self.screen.input(id).map(screen::Input::selection) else {
            return;
        };
        self.send(
            id,
            EventKind::SelectionChange,
            EventPayload::Map(vec![
                ("start", PayloadValue::Number(f64::from(start))),
                ("end", PayloadValue::Number(f64::from(end))),
            ]),
        );
    }

    /// `onKeyPress` of `key` with `m` held.
    fn key_press_with(&self, id: NodeId, key: &str, m: nsh_core::keymap::Modifiers) {
        self.send(
            id,
            EventKind::KeyPress,
            EventPayload::Map(vec![
                ("key", PayloadValue::Str(key.to_owned())),
                ("altKey", PayloadValue::Bool(m.alt)),
                ("ctrlKey", PayloadValue::Bool(m.ctrl)),
                ("metaKey", PayloadValue::Bool(m.cmd)),
                ("shiftKey", PayloadValue::Bool(m.shift)),
            ]),
        );
    }

    /// The user's edit of input `id`: `from..to` (UTF-16) becomes `text`,
    /// the caret after it; reported as the text (`onChange`) and the
    /// caret.
    fn edit(&mut self, id: NodeId, from: u32, to: u32, text: &str) {
        let Some(input) = self.screen.input_mut(id) else {
            return;
        };
        input.replace(from, to, text);
        input.count += 1;
        let payload = EventPayload::Text {
            text: input.text.clone(),
            count: input.count,
        };
        // Always reported: the engine keeps the text for layout and for
        // dropping stale values, whether JS listens or not.
        self.engine
            .control()
            .dispatch_event(id, EventKind::ChangeText, payload);
        self.report_selection(id);
    }

    /// The key `c` in input `id`: it replaces the selection (at the caret),
    /// within `maxLength`.
    fn key(&mut self, id: NodeId, c: char) {
        self.key_press(
            id,
            &if c == '\n' {
                "Enter".into()
            } else {
                c.to_string()
            },
        );
        let Some(input) = self.screen.input(id) else {
            return;
        };
        let (start, end) = input.selection();
        let length = (input.len() - (end - start)) as usize + c.len_utf16();
        if input
            .config
            .max_length
            .is_some_and(|max| length > max as usize)
        {
            return;
        }
        self.edit(id, start, end, c.encode_utf8(&mut [0; 4]));
    }

    /// The return key in input `id`.
    fn return_key(&mut self, id: NodeId) {
        let Some(input) = self.screen.input(id) else {
            return;
        };
        let behavior = input.config.submit();
        let payload = EventPayload::Text {
            text: input.text.clone(),
            count: input.count,
        };
        if behavior == SubmitBehavior::Newline {
            self.key(id, '\n');
            return;
        }
        self.key_press(id, "Enter");
        self.send(id, EventKind::SubmitEditing, payload);
        if behavior == SubmitBehavior::BlurAndSubmit {
            self.screen.set_focused(None);
            self.send(id, EventKind::Blur, EventPayload::None);
        }
    }

    /// The step `scroll(<selector>, <distance>)`: drags the scroll view
    /// `selector` names (the matching view, the nearest scroll view around
    /// it, else the first in it) by `distance` points along its axis
    /// (negative: back), 10 points per display frame, within its content.
    /// Fails if it cannot move that way at all.
    pub fn scroll(&mut self, selector: &str, distance: Points) -> Result<(), String> {
        let parsed = Selector::parse(selector)?;
        self.step(format!("scroll({selector}, {distance})"));
        let pick = self.screen.scroll_view(&parsed);
        let id = self.picked(pick, selector, "scroll view")?;
        let at = |screen: &Screen| {
            screen
                .scroll(id)
                .map_or(Points::ZERO, |s| if s.horizontal { s.x } else { s.y })
        };
        let start = at(&self.screen);
        let target = (start + distance).clamp(Points::ZERO, self.screen.scroll_end(id));
        if (target - start).abs() < px(0.5) {
            return Err(format!(
                "`{selector}` cannot scroll by {}: it is at {} of {}",
                distance.0,
                start.0,
                self.screen.scroll_end(id).0
            ));
        }
        let offset = |screen: &Screen| {
            let s = screen.scroll(id);
            EventPayload::Scroll(s.map_or(Point::default(), |s| point(s.x, s.y)))
        };
        self.send(id, EventKind::ScrollBeginDrag, offset(&self.screen));
        // The core may shift the offset meanwhile (`ScrollBy`): each frame
        // moves on from where the view is.
        let mut frames = ((target - start).abs() / SCROLL_STEP).ceil() as usize * 2 + 10;
        loop {
            let now = at(&self.screen);
            if (target - now).abs() < px(0.5) || frames == 0 {
                break;
            }
            frames -= 1;
            let next = if target > now {
                (now + SCROLL_STEP).min(target)
            } else {
                (now - SCROLL_STEP).max(target)
            };
            let Some(scroll) = self.screen.scroll_mut(id) else {
                break;
            };
            if scroll.horizontal {
                scroll.x = next;
            } else {
                scroll.y = next;
            }
            let (x, y, applied) = (scroll.x, scroll.y, scroll.applied);
            self.engine.control().scroll(id, point(x, y), applied);
            self.animator.scrolled(id, point(x, y));
            // One display frame per step; a frame that needs nothing
            // produces no batch, and a late one is applied with the next.
            if self.clock.is_manual() {
                self.frame(FRAME);
                self.drain();
                continue;
            }
            let frame = Instant::now() + FRAME;
            while Instant::now() < frame {
                if let Pumped::Batch | Pumped::Exited = self.pump(frame) {
                    break;
                }
            }
        }
        self.send(id, EventKind::ScrollEndDrag, offset(&self.screen));
        self.send(id, EventKind::ScrollEnd, offset(&self.screen));
        self.done()
    }
}

/// How a dialog is answered when the test says nothing, as a user backing
/// out does: its cancel button, else closed when it can be; a dialog that
/// can only be answered by one of its other buttons needs the test's
/// answer ([`Headless::on_dialog`]).
pub fn default_answer(dialog: &Dialog) -> Result<DialogAnswer, String> {
    if let Some(cancel) = dialog
        .buttons
        .iter()
        .find(|b| b.style == DialogButtonStyle::Cancel)
    {
        return Ok(DialogAnswer::Press(cancel.text.clone()));
    }
    if dialog.cancelable {
        return Ok(DialogAnswer::Dismiss);
    }
    Err(format!(
        "the dialog {} needs an answer: say which button to press (`onDialog`)",
        describe(dialog)
    ))
}

/// A dialog, as errors name it: its title, else its message.
fn describe(dialog: &Dialog) -> String {
    format!(
        "{:?}",
        dialog
            .title
            .as_deref()
            .or(dialog.message.as_deref())
            .unwrap_or("")
    )
}

/// The error a step fails with for `error` of the app: none for an
/// asset's failure, which the log shows.
fn failure(error: AppError) -> Result<(), String> {
    match error.kind {
        AppErrorKind::Asset => Ok(()),
        _ => Err(format!("the app threw: {error}")),
    }
}

/// Prints `batch`, which came in step `step`, as `nsh test --trace` does: a line
/// naming the step and the number of items, then one line per item.
pub fn print_batch(out: &mut dyn Write, step: &str, batch: &MountBatch) {
    // A closed output (`| head`) ends nothing: the run goes on unseen.
    let _ = writeln!(out, "── {step}: {} items", batch.items.len());
    for item in &batch.items {
        let _ = match item {
            MountItem::Text {
                id, text, content, ..
            } => writeln!(
                out,
                "   Text {{ id: {id}, text: {:?}, content: {}x{} @ {},{} }}",
                text.text,
                content.size.width.0,
                content.size.height.0,
                content.origin.x.0,
                content.origin.y.0
            ),
            MountItem::Frame { id, frame } => writeln!(
                out,
                "   Frame {{ id: {id}, x: {}, y: {}, w: {}, h: {} }}",
                frame.origin.x.0, frame.origin.y.0, frame.size.width.0, frame.size.height.0
            ),
            other => writeln!(out, "   {other:?}"),
        };
    }
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use nsh_core::ui::{Root, prelude::*};

    use super::*;

    /// A count and a button that adds one.
    #[component]
    fn Tally() -> impl IntoElement {
        let count = signal(0_u32);
        view()
            .child(text(move || format!("Count: {}", count.get())))
            .child(
                view()
                    .on_press(move || count.update(|c| *c += 1))
                    .child(text("Add")),
            )
    }

    #[test]
    fn steps_press_what_a_text_is_in_and_wait_for_what_it_shows() {
        let mut app = Headless::start(EngineConfig::new(Root::of::<Tally>())).unwrap();
        let steps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = steps.clone();
        app.observe(move |step, _| seen.lock().unwrap().push(step.to_owned()));
        app.first_render().unwrap();
        app.wait_for("Count: 0").unwrap();
        // The text is in the pressable view: the press goes to the view.
        app.press("Add").unwrap();
        app.wait_for("text:Count: 1").unwrap();
        app.press("text:Add").unwrap();
        app.wait_for("Count: 2").unwrap();
        assert!(!app.shows("Count: 1"));
        assert_eq!(app.screen().texts(), ["Count: 2", "Add"]);
        assert_eq!(app.screen().press_target("Nothing"), Ok(None));
        assert!(app.screen().press_target("view:x").is_err());
        let error = app.press("Nothing").unwrap_err();
        assert_eq!(
            error,
            "no pressable view matches `Nothing`; on screen: [\"Count: 2\", \"Add\"]"
        );
        let error = app
            .wait_for_within("Count: 9", Duration::from_millis(50))
            .unwrap_err();
        assert!(error.starts_with("`Count: 9` showing never happened in 50 ms; on screen: "));
        let steps = steps.lock().unwrap();
        assert_eq!(steps.first().map(String::as_str), Some("open"));
        assert!(steps.iter().any(|s| s == "press(Add)"), "{steps:?}");
        assert!(steps.iter().any(|s| s == "press(text:Add)"), "{steps:?}");
    }

    /// A button that shows whether Control was held for its last press.
    #[component]
    fn CtrlProbe() -> impl IntoElement {
        let ctrl = signal(None::<bool>);
        view()
            .child(text(move || format!("ctrl: {:?}", ctrl.get())))
            .child(
                view()
                    .on(EventKind::Press, move |p: &EventPayload| {
                        let held = match p {
                            EventPayload::Press(e) => Some(e.modifiers.ctrl),
                            _ => None,
                        };
                        ctrl.set(held);
                    })
                    .child(text("Probe")),
            )
    }

    #[test]
    fn a_press_with_modifiers_carries_them() {
        let mut app = Headless::start(EngineConfig::new(Root::of::<CtrlProbe>())).unwrap();
        app.first_render().unwrap();
        let ctrl = nsh_core::keymap::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        app.press_with("Probe", ctrl).unwrap();
        app.wait_for("ctrl: Some(true)").unwrap();
        app.press("Probe").unwrap();
        app.wait_for("ctrl: Some(false)").unwrap();
    }

    /// A paragraph 40 points square under a 10-point strip that shows,
    /// for its last press, where it was in the paragraph and in the
    /// surface, and the caret offset there.
    #[component]
    fn PressPoint() -> impl IntoElement {
        let seen = signal(String::from("none"));
        let paragraph = element_ref();
        view()
            .child(view().h(px(10.0)))
            .child(
                text("abcdefghij")
                    .w(px(40.0))
                    .h(px(40.0))
                    .test_id("paragraph")
                    .element_ref(&paragraph)
                    .on(EventKind::Press, move |p: &EventPayload| {
                        let EventPayload::Press(PressEvent {
                            position: Some(at), ..
                        }) = *p
                        else {
                            seen.set("no position".into());
                            return;
                        };
                        spawn(async move {
                            let index = paragraph.text_index_at(at.location).await;
                            let (l, g) = (at.location, at.page);
                            seen.set(format!(
                                "{} {} in {} {} at {index:?}",
                                l.x.0, l.y.0, g.x.0, g.y.0
                            ));
                        });
                    }),
            )
            .child(text(move || format!("seen: {}", seen.get())))
    }

    /// The approximate engine's 7.7-point characters and 16.8-point
    /// lines: the paragraph is two lines of five.
    #[test]
    fn presses_carry_the_point_and_text_index_at_reads_it() {
        let mut app = Headless::start(EngineConfig::new(Root::of::<PressPoint>())).unwrap();
        app.first_render().unwrap();
        app.press_at("id:paragraph", point(px(16.0), px(5.0)))
            .unwrap();
        app.wait_for("seen: 16 5 in 16 15 at Some(2)").unwrap();
        app.press("id:paragraph").unwrap();
        app.wait_for("seen: 20 20 in 20 30 at Some(8)").unwrap();
    }

    /// A paragraph narrower than its text, which shows the lines it was
    /// laid out in and, once "Find" is pressed, the caret offsets at four
    /// points of it.
    #[component]
    fn Paragraph() -> impl IntoElement {
        let lines = signal(String::from("none"));
        let found = signal(String::from("none"));
        let paragraph = element_ref();
        let points = [(16.0, 5.0), (16.0, 20.0), (1000.0, 1000.0), (-5.0, -5.0)];
        view()
            .child(
                text("abcdefghij")
                    .w(px(40.0))
                    .element_ref(&paragraph)
                    .on_text_layout(move |l: &[TextLine]| {
                        let shown: Vec<String> = l
                            .iter()
                            .map(|l| format!("{}..{} {} y {:.1}", l.start, l.end, l.text, l.y.0))
                            .collect();
                        lines.set(shown.join(", "));
                    }),
            )
            .child(text(move || format!("lines: {}", lines.get())))
            .child(text(move || format!("at: {}", found.get())))
            .child(
                view()
                    .on_press(move || {
                        spawn(async move {
                            let mut at = Vec::new();
                            for (x, y) in points {
                                at.push(paragraph.text_index_at(point(px(x), px(y))).await);
                            }
                            found.set(format!("{at:?}"));
                        });
                    })
                    .child(text("Find")),
            )
    }

    /// The approximate engine sets 7.7 points a character and 16.8 a
    /// line at the default 14-point font: ten characters in 40 points
    /// are two lines of five.
    #[test]
    fn a_paragraph_reports_its_lines_and_the_offsets_at_points() {
        let mut app = Headless::start(EngineConfig::new(Root::of::<Paragraph>())).unwrap();
        app.first_render().unwrap();
        app.wait_for("lines: 0..5 abcde y 0.0, 5..10 fghij y 16.8")
            .unwrap();
        app.press("Find").unwrap();
        app.wait_for("at: [Some(2), Some(7), Some(10), Some(0)]")
            .unwrap();
    }

    /// A field that logs its keys and selections, with an undo binding
    /// that fires in text fields too and a Left binding that does not.
    #[component]
    fn KeyLog() -> impl IntoElement {
        let value = signal(String::from("abcd"));
        let log = signal(Vec::<String>::new());
        let note = move |entry: String| log.update(|l| l.push(entry));
        let always = KeyOptions {
            when: nsh_core::keymap::When::Always,
            ..KeyOptions::default()
        };
        on_key_with("mod+z", always, move |_| note("undo".into()));
        on_key("left", move |_| note("bound left".into()));
        view()
            .child(
                input()
                    .value(move || value.get())
                    .on_change(move |t: &str| value.set(t.to_owned()))
                    .on_key_press_with_modifiers(move |k: &KeyPress| {
                        let shift = if k.modifiers.shift { "+shift" } else { "" };
                        note(format!("{}{shift}", k.key));
                    })
                    .on_selection_change(move |s: &Selection| {
                        note(format!("sel {}-{}", s.start, s.end))
                    })
                    .test_id("field"),
            )
            .child(text(move || format!("value: {}", value.get())))
            .child(text(move || format!("log: {}", log.get().join("|"))))
    }

    #[test]
    fn keys_go_to_bindings_first_then_the_field_at_its_caret() {
        let mut app = Headless::start(EngineConfig::new(Root::of::<KeyLog>())).unwrap();
        app.first_render().unwrap();
        app.wait_for("value: abcd").unwrap();
        // Typing goes in at the selection, which replaces it.
        app.select("id:field", 1, 3).unwrap();
        app.type_text("id:field", "X").unwrap();
        app.wait_for("value: aXd").unwrap();
        // A Left binding that does not fire in text fields leaves the key
        // to the field: reported, then the caret moves; Shift extends.
        app.press_key("left").unwrap();
        app.press_key("shift+left").unwrap();
        app.press_key("delete").unwrap();
        app.wait_for("value: Xd").unwrap();
        // The undo binding fires in the field, which never sees the key.
        app.press_key("mod+z").unwrap();
        app.press_key("shift+y").unwrap();
        app.wait_for("value: YXd").unwrap();
        app.wait_for(
            "log: sel 1-3|X|sel 2-2|ArrowLeft|sel 1-1|ArrowLeft+shift|sel 0-1|Delete|Backspace|sel 0-0|undo|Y|sel 1-1",
        )
        .unwrap();
    }

    /// A name field that says hello and keeps what was sent.
    #[component]
    fn Form() -> impl IntoElement {
        let name = signal(String::new());
        let sent = signal(String::new());
        let keys = signal(0_u32);
        view()
            .child(
                input()
                    .test_id("name")
                    .placeholder("Your name")
                    .max_length(5_u32)
                    .value(move || name.get())
                    .on_change(move |text: &str| name.set(text.to_owned()))
                    .on_key_press(move |_: &str| keys.update(|k| *k += 1))
                    .on_submit(move |text: &str| {
                        sent.set(text.to_owned());
                        name.set(String::new());
                    }),
            )
            .child(text(move || format!("Hello, {}", name.get())))
            .child(text(move || format!("Sent: {}", sent.get())))
            .child(text(move || format!("Keys: {}", keys.get())))
            .child(input().test_id("note").readonly(true))
    }

    #[test]
    fn typing_edits_an_input_key_by_key_and_submits_it() {
        let mut app = Headless::start(EngineConfig::new(Root::of::<Form>())).unwrap();
        app.first_render().unwrap();
        // By its placeholder, while it is empty.
        app.type_text("text:Your name", "Ada").unwrap();
        app.wait_for("Hello, Ada").unwrap();
        assert_eq!(app.screen().input_text("id:name"), Ok(Some("Ada")));
        // At the end of what it holds, cut at maxLength.
        app.type_text("id:name", " Lovelace").unwrap();
        app.wait_for("Hello, Ada L").unwrap();
        app.wait_for("Keys: 12").unwrap();
        app.submit("id:name").unwrap();
        app.wait_for("Sent: Ada L").unwrap();
        // The app cleared it: the value it set wins.
        app.wait_for("Hello, ").unwrap();
        assert_eq!(app.screen().input_text("id:name"), Ok(Some("")));
        // A line break in a single line is the return key.
        app.type_text("id:name", "Bo\n").unwrap();
        app.wait_for("Sent: Bo").unwrap();
        assert_eq!(
            app.type_text("id:note", "x").unwrap_err(),
            "the input `id:note` is not editable"
        );
        assert!(
            app.type_text("id:none", "x")
                .unwrap_err()
                .starts_with("no input matches `id:none`")
        );
    }

    /// A list of rows taller than the screen, saying where it is.
    #[component]
    fn Rows() -> impl IntoElement {
        let at = signal(0.0_f32);
        view()
            .flex_1()
            .child(text(move || format!("At {}", at.get())))
            .child(
                scroll()
                    .test_id("rows")
                    .flex_1()
                    .on_scroll(move |e: &ScrollEvent| at.set(e.offset.y.0))
                    .children((0..100).map(|i| view().h(px(50.0)).child(text(format!("Row {i}"))))),
            )
    }

    #[test]
    fn scrolling_moves_a_scroll_view_within_its_content() {
        let mut app = Headless::start(EngineConfig::new(Root::of::<Rows>())).unwrap();
        app.first_render().unwrap();
        // A row names the scroll view it is in.
        app.scroll("Row 3", px(1000.0)).unwrap();
        app.wait_for("At 1000").unwrap();
        app.scroll("id:rows", px(-400.0)).unwrap();
        app.wait_for("At 600").unwrap();
        // Not past the end: 100 rows of 50 in what the screen leaves.
        app.scroll("id:rows", px(100_000.0)).unwrap();
        let Pick::One(rows) = app
            .screen()
            .scroll_view(&Selector::parse("id:rows").unwrap())
        else {
            panic!("one list");
        };
        let end = app.screen().scroll_end(rows);
        assert!(end > px(4000.0) && end < px(5000.0), "{end}");
        app.wait_for(&format!("At {}", end.0)).unwrap();
        let error = app.scroll("id:rows", px(10.0)).unwrap_err();
        assert!(
            error.starts_with("`id:rows` cannot scroll by 10: it is at "),
            "{error}"
        );
    }

    #[cfg(feature = "js")]
    #[test]
    fn a_step_fails_when_the_app_throws() {
        let app = nsh_engine::Bundle::source("throw new Error('boom')".into());
        let mut app = Headless::start(EngineConfig::new(app)).unwrap();
        let error = app.first_render().unwrap_err();
        assert!(error.starts_with("the app threw: Error: boom"), "{error}");
    }

    #[cfg(feature = "js")]
    #[test]
    fn process_exit_ends_a_timed_headless_wait() {
        use nsh_core::platform::Os;

        let bundle = nsh_engine::Bundle::source(
            "import process from 'process'; setTimeout(() => process.exit(0), 10);".into(),
        )
        .with_capabilities_json(Some(r#"{"process":{"exit":true}}"#))
        .unwrap();
        let mut config = EngineConfig::new(bundle);
        config.environment.os = Os::MacOs;
        let mut app = Headless::start(config).unwrap();
        let started = Instant::now();
        app.wait(Duration::from_secs(5)).unwrap();
        assert!(app.screen().sent(&Command::ExitApp));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
