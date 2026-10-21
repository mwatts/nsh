//! The step `drag`: a pointer held down on a view, moved a display frame
//! at a time to a point or onto another view, and lifted, as the desktop
//! platforms take a mouse drag (`nsh-platform-gpui`'s `input.rs`): the
//! touches go to JS's responder system, a `panActivation` around the
//! pointer decides on every move, and once the pointer passed the slop
//! the innermost view with a native pan (`pan={{ x, y }}`, `onPanStart`,
//! `onPanEnd`) taking that direction pans, unless a JS responder blocks
//! native gestures. As a mouse's, the drag moves no scroll view (that is
//! [`Headless::scroll`]) and presses nothing.

use nsh_core::{
    Command, EventKind, EventPayload, NodeId, PanActivation, Point, Points, ROOT_ID, TouchEvent,
    TouchPhase, TouchPoint, geometry::point, time::Instant,
};

use super::{FRAME, Headless, Pumped, SCROLL_STEP, screen::Screen, selector::Selector};

/// Where [`Headless::drag`] lifts the pointer.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DragTo {
    /// This far from where it went down.
    By(Point<Points>),
    /// Over the middle of the view a selector names, where that view is
    /// when the drag begins.
    Onto(String),
}

/// How far the pointer moves before a native pan starts.
const PAN_SLOP: Points = Points(6.0);

/// A view's `panActivation` deciding on the drag.
struct Gate {
    id: NodeId,
    activation: PanActivation,
    /// Pans from this index on are around the gated view: they wait until
    /// its pan fails.
    outer: usize,
}

/// A view that pans natively, and the axes it takes.
#[derive(Clone, Copy)]
struct Pan {
    id: NodeId,
    x: bool,
    y: bool,
}

impl Pan {
    /// Whether it takes a drag by `(dx, dy)`: both axes take any, one axis
    /// the drags mostly along it.
    fn accepts(self, by: Point<Points>) -> bool {
        let horizontal = by.x.0.abs() > by.y.0.abs();
        (self.x && self.y) || (self.x && horizontal) || (self.y && !horizontal)
    }
}

/// A drag in progress.
struct Drag {
    /// The innermost view under the pointer when it went down.
    target: NodeId,
    /// The views around `target` that listen to touches, innermost first.
    path: Vec<NodeId>,
    start: Point<Points>,
    at: Point<Points>,
    /// When the pointer was at `at`, and where it was a move before and
    /// when: the velocity.
    at_time: Instant,
    before: (Point<Points>, Instant),
    began: Instant,
    pans: Vec<Pan>,
    gate: Option<Gate>,
    /// The view panning.
    pan: Option<NodeId>,
    /// Past the slop with nothing to pan, or a JS responder took it.
    free: bool,
    moved: bool,
    /// The screen's commands when the drag began: JS's responder blocks
    /// native gestures with the ones after.
    commands: usize,
}

impl Drag {
    fn new(headless: &Headless, target: NodeId, at: Point<Points>) -> Drag {
        let screen = &headless.screen;
        let chain = screen.chain(target);
        let mut pans = Vec::new();
        let mut gate = None;
        for &id in &chain {
            let axes = headless.animator.pan_axes(id);
            if axes.is_some()
                || screen.hears(id, EventKind::PanStart)
                || screen.hears(id, EventKind::PanEnd)
            {
                let (x, y) = match axes {
                    Some((false, false)) | None => (true, true),
                    Some(axes) => axes,
                };
                pans.push(Pan { id, x, y });
            }
            if gate.is_none()
                && let Some(activation) = screen.pan_activation(id)
            {
                gate = Some(Gate {
                    id,
                    activation,
                    outer: pans.len(),
                });
            }
        }
        let now = headless.clock.now();
        Drag {
            target,
            path: chain
                .into_iter()
                .filter(|&id| screen.hears(id, EventKind::Touch))
                .collect(),
            start: at,
            at,
            at_time: now,
            before: (at, now),
            began: now,
            pans,
            gate,
            pan: None,
            free: false,
            moved: false,
            commands: screen.commands().len(),
        }
    }

    /// Whether JS's responder blocks native gestures now.
    fn js_blocks(&self, screen: &Screen) -> bool {
        screen.commands()[self.commands..]
            .iter()
            .rev()
            .find_map(|(_, command)| match command {
                Command::SetJsResponder { block } => Some(*block),
                Command::ClearJsResponder => Some(false),
                _ => None,
            })
            .unwrap_or(false)
    }

    fn translation(&self) -> Point<Points> {
        self.at - self.start
    }

    /// The pointer moved to `at` now.
    fn move_to(&mut self, at: Point<Points>, now: Instant) {
        self.before = (self.at, self.at_time);
        (self.at, self.at_time) = (at, now);
    }

    /// Points per second over the last move.
    fn velocity(&self) -> (f32, f32) {
        let seconds = self
            .at_time
            .saturating_duration_since(self.before.1)
            .as_secs_f32();
        if seconds <= 0.0 {
            return (0.0, 0.0);
        }
        let by = self.at - self.before.0;
        (by.x.0 / seconds, by.y.0 / seconds)
    }
}

impl Headless {
    /// The step `drag(<selector>, <to>)`: holds the pointer down on the
    /// middle of the view `selector` names (on the innermost view there),
    /// moves it [`SCROLL_STEP`] a display frame to `to`, and lifts it
    /// there, as a mouse drag on the desktop does. The views around it
    /// that listen to touches hear them all (`onTouch*`, the responder
    /// negotiation, `onResponder*`); a `panActivation` around it decides on
    /// each move and makes its view the responder when it activates; past
    /// 6 points, the innermost view that pans natively in that direction
    /// pans (`onPanStart`, its `pan` values follow the pointer,
    /// `onPanEnd`) unless a JS responder blocks native gestures. Fails
    /// when either selector names no view or several.
    pub fn drag(&mut self, selector: &str, to: DragTo) -> Result<(), String> {
        let parsed = Selector::parse(selector)?;
        let onto = match &to {
            DragTo::By(by) => format!("by {}, {}", by.x.0, by.y.0),
            DragTo::Onto(target) => target.clone(),
        };
        self.step(format!("drag({selector}, {onto})"));
        let source = self.picked(self.screen.view(&parsed), selector, "view")?;
        let from = self.middle(source, selector)?;
        let end = match to {
            DragTo::By(by) => from + by,
            DragTo::Onto(target) => {
                let parsed = Selector::parse(&target)?;
                let id = self.picked(self.screen.view(&parsed), &target, "view")?;
                self.middle(id, &target)?
            }
        };
        let target = self.screen.hit(source, from);
        let mut drag = Drag::new(self, target, from);
        self.touch(&drag, TouchPhase::Start, None);
        // JS hears the pointer go down before it moves: a responder it
        // grants now blocks native gestures from the first move.
        self.settle();
        let way = end - from;
        let moves = (way.x.0.hypot(way.y.0) / SCROLL_STEP.0).ceil().max(1.0) as u32;
        for i in 1..=moves {
            let f = i as f32 / moves as f32;
            let at = from + point(way.x * f, way.y * f);
            self.drag_move(&mut drag, at);
            self.drag_frame();
        }
        self.touch(&drag, TouchPhase::End, None);
        if let Some(pan) = drag.pan {
            self.animator.pan_moved(pan, drag.translation());
            self.pan_event(&drag, pan, EventKind::PanEnd, drag.translation());
        }
        self.done()
    }

    /// The middle of view `id` in the surface.
    fn middle(&self, id: NodeId, selector: &str) -> Result<Point<Points>, String> {
        self.screen
            .page_bounds(id)
            .map(|b| b.center())
            .ok_or_else(|| format!("`{selector}` has no place on screen"))
    }

    /// The pointer moved to `at`.
    fn drag_move(&mut self, drag: &mut Drag, at: Point<Points>) {
        drag.move_to(at, self.clock.now());
        let by = drag.translation();
        let mut activated = None;
        if let Some(gate) = drag.gate.take() {
            match gate.activation.decide(by.x, by.y) {
                Some(true) => {
                    drag.pans.truncate(gate.outer);
                    activated = Some(gate.id);
                    if drag.pan.is_none() {
                        drag.free = true;
                    }
                }
                Some(false) => {}
                None => drag.gate = Some(gate),
            }
        }
        self.touch(drag, TouchPhase::Move, activated);
        let first = !std::mem::replace(&mut drag.moved, true);
        let js_blocks = drag.js_blocks(&self.screen);
        if let Some(pan) = drag.pan
            && js_blocks
        {
            // The JS responder took the drag over from the native pan.
            self.pan_event(drag, pan, EventKind::PanEnd, by);
            drag.pan = None;
            drag.free = true;
        }
        let past = by.x.0.hypot(by.y.0) > PAN_SLOP.0;
        // JS hears of the first move only with it: its responder gets a
        // chance to take the drag before a native pan starts.
        let js_pending = !drag.path.is_empty() && first;
        if drag.pan.is_none() && !drag.free && past && !js_pending {
            let waiting = drag.gate.as_ref().map(|g| g.outer);
            if waiting.is_none() {
                drag.free = true;
            }
            let open = waiting.unwrap_or(drag.pans.len());
            let started = (!js_blocks)
                .then(|| drag.pans[..open].iter().find(|p| p.accepts(by)).copied())
                .flatten();
            if let Some(pan) = started {
                drag.gate = None;
                drag.pan = Some(pan.id);
                self.animator.pan_began(pan.id);
                self.pan_event(drag, pan.id, EventKind::PanStart, Point::default());
            }
        }
        if let Some(pan) = drag.pan {
            self.animator.pan_moved(pan, by);
        }
    }

    /// One display frame of a drag: what the move made comes in.
    fn drag_frame(&mut self) {
        if self.clock.is_manual() {
            self.frame(FRAME);
            self.drain();
            return;
        }
        let frame = std::time::Instant::now() + FRAME;
        while std::time::Instant::now() < frame {
            if let Pumped::Batch | Pumped::Exited = self.pump(frame) {
                break;
            }
        }
    }

    /// Reports the pointer to JS, as the platforms do, when a view around
    /// it listens to touches.
    fn touch(&self, drag: &Drag, phase: TouchPhase, activated: Option<NodeId>) {
        if drag.path.is_empty() {
            return;
        }
        let origin = self
            .screen
            .page_bounds(drag.target)
            .map_or(Point::default(), |b| b.origin);
        let elapsed = self.clock.now().saturating_duration_since(drag.began);
        let event = TouchEvent {
            phase,
            timestamp: elapsed.as_secs_f64() * 1000.0,
            points: vec![TouchPoint {
                id: 1,
                target: drag.target,
                // JS keeps a finger's path from its start.
                path: if phase == TouchPhase::Start {
                    drag.path.clone()
                } else {
                    Vec::new()
                },
                page: drag.at,
                location: drag.at - origin,
                changed: true,
            }],
            activated,
        };
        self.engine.control().dispatch_event(
            ROOT_ID,
            EventKind::Touch,
            EventPayload::Touch(Box::new(event)),
        );
    }

    /// Sends `kind` (`PanStart`, `PanEnd`) to the panning view `pan`.
    fn pan_event(&self, drag: &Drag, pan: NodeId, kind: EventKind, translation: Point<Points>) {
        let (value_x, value_y) = self.animator.pan_values(pan);
        let (velocity_x, velocity_y) = drag.velocity();
        self.send(
            pan,
            kind,
            EventPayload::Pan {
                translation,
                velocity_x,
                velocity_y,
                value_x,
                value_y,
            },
        );
    }
}
