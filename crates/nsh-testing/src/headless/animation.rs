//! The native animations of a headless app: the graph platforms run on
//! their UI thread (nsh-core's `animation`, platform-free), run here frame by
//! frame while anything animates, finished animations reported to the
//! engine as a platform reports them. Nothing is drawn: the values only
//! move, so what waits for an animation's end goes on.

use std::{collections::HashMap, time::Duration};

use nsh_core::{
    GestureValues, MountItem, NodeId, Point, Points,
    animation::{Finished, Graph, NodeId as ValueId},
    time::{Clock, Instant},
};

/// A display frame at 60 Hz.
pub(crate) const FRAME: Duration = Duration::from_micros(16_667);

pub(crate) struct Animator {
    graph: Graph,
    finished: Vec<Finished>,
    /// Scroll view -> (its offset's value, horizontal).
    scrolls: HashMap<NodeId, (ValueId, bool)>,
    /// View -> the values its drags move (`pan={{ x, y }}`), and theirs
    /// when its drag began.
    pans: HashMap<NodeId, (GestureValues, Point<f32>)>,
    /// When the graph's clock started (its times are seconds since).
    epoch: Instant,
    /// When the next frame is due, while anything animates.
    next: Option<Instant>,
    /// The app's clock, whose time the frames are at.
    clock: Clock,
}

impl Animator {
    /// An animator on `clock`, the app's.
    pub(crate) fn new(clock: Clock) -> Self {
        Self {
            graph: Graph::default(),
            finished: Vec::new(),
            scrolls: HashMap::new(),
            pans: HashMap::new(),
            epoch: clock.now(),
            next: None,
            clock,
        }
    }

    /// Takes in one item of a batch: animation ops and scroll values.
    pub(crate) fn apply(&mut self, item: &MountItem) {
        match item {
            MountItem::Animation(op) => self.graph.apply(op.clone(), &mut self.finished),
            MountItem::ScrollConfig {
                id,
                horizontal,
                scroll_value,
                ..
            } => match scroll_value {
                Some(value) => {
                    self.scrolls.insert(*id, (*value, *horizontal));
                }
                None => {
                    self.scrolls.remove(id);
                }
            },
            MountItem::Gestures { id, values } => {
                if values.pan_x.is_none() && values.pan_y.is_none() {
                    self.pans.remove(id);
                } else {
                    self.pans.entry(*id).or_default().0 = *values;
                }
            }
            MountItem::Delete { id } => {
                self.scrolls.remove(id);
                self.pans.remove(id);
            }
            _ => return,
        }
        self.wake();
    }

    /// A scroll view moved: feeds its offset's value.
    pub(crate) fn scrolled(&mut self, id: NodeId, offset: Point<Points>) {
        if let Some(&(value, horizontal)) = self.scrolls.get(&id) {
            let along = if horizontal { offset.x } else { offset.y };
            self.graph.set(value, along.0, &mut self.finished);
            self.wake();
        }
    }

    /// Which axes view `id`'s drags move (`pan.x`, `pan.y`), if any.
    pub(crate) fn pan_axes(&self, id: NodeId) -> Option<(bool, bool)> {
        let (values, _) = self.pans.get(&id)?;
        Some((values.pan_x.is_some(), values.pan_y.is_some()))
    }

    /// A drag on `id` began: grabbing an animating value stops it where
    /// it is, and the drag moves it on from there.
    pub(crate) fn pan_began(&mut self, id: NodeId) {
        let graph = &self.graph;
        if let Some((values, start)) = self.pans.get_mut(&id) {
            let now = |value: Option<ValueId>| value.and_then(|v| graph.value(v)).unwrap_or(0.0);
            *start = Point::new(now(values.pan_x), now(values.pan_y));
        }
    }

    /// The drag on `id` moved by `by` since it began.
    pub(crate) fn pan_moved(&mut self, id: NodeId, by: Point<Points>) {
        let Some(&(values, start)) = self.pans.get(&id) else {
            return;
        };
        if let Some(x) = values.pan_x {
            self.graph.set(x, start.x + by.x.0, &mut self.finished);
        }
        if let Some(y) = values.pan_y {
            self.graph.set(y, start.y + by.y.0, &mut self.finished);
        }
        self.wake();
    }

    /// The values the drag on `id` moves, now.
    pub(crate) fn pan_values(&self, id: NodeId) -> (Option<Points>, Option<Points>) {
        let Some((values, _)) = self.pans.get(&id) else {
            return (None, None);
        };
        let now = |value: Option<ValueId>| value.and_then(|v| self.graph.value(v)).map(Points);
        (now(values.pan_x), now(values.pan_y))
    }

    /// Starts the frames when something animates.
    fn wake(&mut self) {
        if self.graph.is_animating() && self.next.is_none() {
            self.next = Some(self.clock.now());
        }
    }

    /// Whether anything animates.
    pub(crate) fn running(&self) -> bool {
        self.next.is_some()
    }

    /// When the next frame is due, while anything animates.
    pub(crate) fn next_frame(&self) -> Option<Instant> {
        self.next
    }

    /// Runs the frames that are due.
    pub(crate) fn tick(&mut self) {
        let Some(next) = self.next else {
            return;
        };
        let now = self.clock.now();
        if now < next {
            return;
        }
        self.graph
            .tick((now - self.epoch).as_secs_f64(), &mut self.finished);
        self.next = self.graph.is_animating().then(|| (next + FRAME).max(now));
    }

    /// The animations that finished since the last call.
    pub(crate) fn take_finished(&mut self) -> Vec<Finished> {
        std::mem::take(&mut self.finished)
    }
}
