//! Small status indicators run in NSH's native animation graph, never by repainting the canvas.
//! Unlike the GPUI reference, all motion stops when the host requests reduced motion.

use std::f32::consts::PI;

use falcon_core::program_status::BadgeTone;
use nsh_core::animation::{Easing, Keyframe, KeyframeTiming};
use nsh_core::prelude::*;

const FPS: f32 = 30.0;

#[derive(Clone, Copy)]
pub(super) struct StatusMotion {
    pub fade: AnimatedValue,
    pub rotation: AnimatedValue,
    pub scale: AnimatedValue,
    pub shake: AnimatedValue,
    pub halo: AnimatedValue,
    pub edge: AnimatedValue,
}

impl StatusMotion {
    pub fn new() -> Self {
        Self {
            fade: animated_value(1.0),
            rotation: animated_value(0.0),
            scale: animated_value(1.0),
            shake: animated_value(0.0),
            halo: animated_value(0.0),
            edge: animated_value(0.0),
        }
    }

    pub fn play(self, tone: Option<BadgeTone>, reduced: bool) {
        // set() cancels previous drivers, including loops, on every state/kind change.
        self.fade.set(1.0);
        self.rotation.set(0.0);
        self.scale.set(1.0);
        self.shake.set(0.0);
        self.halo.set(0.0);
        self.edge.set(if tone == Some(BadgeTone::Blocked) {
            1.0
        } else {
            0.0
        });
        let Some(tone) = tone.filter(|_| !reduced) else {
            return;
        };
        animate(self.fade, 220.0, false, |t| t * t * (3.0 - 2.0 * t));
        match tone {
            BadgeTone::Working => animate(self.rotation, 1000.0, true, |t| 360.0 * t),
            BadgeTone::Blocked => {
                animate(self.rotation, 700.0, false, |t| {
                    (t * 6.0 * PI).sin() * (1.0 - t) * 16.0
                });
                animate(self.halo, 1800.0, true, |t| 0.45 * breathe(t));
                animate(self.edge, 1800.0, true, |t| 0.35 + 0.65 * breathe(t));
            }
            BadgeTone::Done => {
                animate(self.scale, 450.0, false, |t| 0.3 + 0.7 * back_out(t));
                animate(self.edge, 1400.0, false, |t| (1.0 - t).powi(2));
            }
            BadgeTone::Error => {
                // Translate is in logical pixels; the caller applies the interface zoom.
                animate(self.shake, 500.0, false, |t| {
                    (t * 6.0 * PI).sin() * (1.0 - t) * 3.0
                });
                animate(self.edge, 1400.0, false, |t| (1.0 - t).powi(2));
            }
        }
    }
}

fn animate(value: AnimatedValue, ms: f32, repeat: bool, sample: impl Fn(f32) -> f32) {
    let steps = (ms * FPS / 1000.0).ceil() as usize;
    let frames = (0..=steps).map(|i| {
        let t = i as f32 / steps as f32;
        Keyframe::at(t, sample(t))
    });
    value.set(sample(0.0));
    value.keyframes(
        frames,
        KeyframeTiming::new(ms)
            .easing(Easing::Linear)
            .iterations(if repeat { None } else { Some(1) })
            .frame_rate(FPS),
    );
}

fn breathe(t: f32) -> f32 {
    let s = (t * 2.0 * PI).sin();
    ((s * s * s + s) / 2.0 + 1.0) / 2.0
}

fn back_out(t: f32) -> f32 {
    let x = t - 1.0;
    1.0 + 2.70158 * x * x * x + 1.70158 * x * x
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsh_core::{
        MountItem,
        animation::{AnimationOp, Composite},
        element::Root,
        platform::Environment,
    };
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;
    use std::time::Duration;

    #[derive(Clone, PartialEq, Props)]
    struct MotionHarnessProps {
        tone: u32,
    }

    #[component]
    fn MotionHarness(props: MotionHarnessProps) -> impl IntoElement {
        let motion = StatusMotion::new();
        let tone = [
            BadgeTone::Working,
            BadgeTone::Blocked,
            BadgeTone::Done,
            BadgeTone::Error,
        ][props.tone as usize];
        effect(move || motion.play(Some(tone), reduce_motion()));
        view()
            .bind_opacity(&motion.fade)
            .bind_rotate(&motion.rotation)
            .bind_scale(&motion.scale)
            .bind_translate_x(&motion.shake)
            .child(view().bind_opacity(&motion.halo))
            .child(view().bind_opacity(&motion.edge))
    }

    fn start(tone: u32, reduced: bool) -> Headless {
        let root = Root::new::<MotionHarness>(move || MotionHarnessProps { tone });
        let mut config = EngineConfig::new(root);
        config.environment.reduce_motion = reduced;
        Headless::start(config).unwrap()
    }

    fn collect(app: &mut Headless) -> Vec<AnimationOp> {
        let first = app
            .next_batch(Duration::from_secs(5))
            .expect("animation batch");
        let mut items = first.items;
        while let Some(batch) = app.next_batch(Duration::from_millis(30)) {
            items.extend(batch.items);
        }
        items
            .into_iter()
            .filter_map(|item| match item {
                MountItem::Animation(op) => Some(op),
                _ => None,
            })
            .collect()
    }

    fn starts(ops: &[AnimationOp]) -> Vec<&Composite> {
        ops.iter()
            .filter_map(|op| match op {
                AnimationOp::StartComposite { spec, .. } => Some(spec),
                _ => None,
            })
            .collect()
    }

    fn loops(spec: &Composite) -> bool {
        match spec {
            Composite::Loop {
                iterations: None, ..
            } => true,
            Composite::Loop { child, .. } => loops(child),
            Composite::Sequence(children) | Composite::Parallel { children, .. } => {
                children.iter().any(loops)
            }
            _ => false,
        }
    }

    #[test]
    fn status_drivers_are_native_capped_and_only_working_or_blocked_loop() {
        for tone in 0..4 {
            let mut app = start(tone, false);
            let ops = collect(&mut app);
            let animations = starts(&ops);
            assert!(!animations.is_empty(), "missing tone {tone}");
            assert!(animations.iter().all(|spec| spec.frame_rate() == Some(FPS)));
            assert_eq!(animations.iter().any(|spec| loops(spec)), tone < 2);
        }
    }

    #[test]
    fn reduced_motion_starts_static_and_cancels_existing_native_drivers() {
        for tone in 0..4 {
            let mut app = start(tone, true);
            assert!(starts(&collect(&mut app)).is_empty());

            let mut app = start(tone, false);
            let ops = collect(&mut app);
            let mut nodes = Vec::new();
            for spec in starts(&ops) {
                spec.nodes(&mut nodes);
            }
            app.engine().control().set_environment(Environment {
                reduce_motion: true,
                ..Default::default()
            });
            let stopped = collect(&mut app);
            assert!(starts(&stopped).is_empty());
            for running in nodes {
                assert!(
                    stopped
                        .iter()
                        .any(|op| matches!(op, AnimationOp::Set { node, .. } if *node == running)),
                    "tone {tone} left native driver {running} running"
                );
            }
        }
    }
}
