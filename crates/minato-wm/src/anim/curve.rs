//! CSS-style cubic bezier easing: the curve runs from (0,0) to (1,1) with
//! control points (x1,y1) and (x2,y2); x is time, y is progress.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bezier {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

/// One coordinate of the bezier at parameter `s`, with endpoints 0 and 1.
fn coord(a: f32, b: f32, s: f32) -> f32 {
    let inv = 1.0 - s;
    3.0 * inv * inv * s * a + 3.0 * inv * s * s * b + s * s * s
}

fn slope(a: f32, b: f32, s: f32) -> f32 {
    let inv = 1.0 - s;
    3.0 * inv * inv * a + 6.0 * inv * s * (b - a) + 3.0 * s * s * (1.0 - b)
}

impl Bezier {
    pub fn new([x1, y1, x2, y2]: [f32; 4]) -> Self {
        // x must stay monotonic for the curve to be a function of time.
        Self {
            x1: x1.clamp(0.0, 1.0),
            y1,
            x2: x2.clamp(0.0, 1.0),
            y2,
        }
    }

    pub const LINEAR: Bezier = Bezier {
        x1: 0.0,
        y1: 0.0,
        x2: 1.0,
        y2: 1.0,
    };

    /// Progress at time `t` (clamped to 0..=1). May overshoot 0..1 when a
    /// control point's y does.
    pub fn eval(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if t == 0.0 || t == 1.0 {
            return t;
        }
        // Solve x(s) = t: Newton first, bisection if it stalls.
        let mut s = t;
        for _ in 0..8 {
            let err = coord(self.x1, self.x2, s) - t;
            if err.abs() < 1e-6 {
                return coord(self.y1, self.y2, s);
            }
            let d = slope(self.x1, self.x2, s);
            if d.abs() < 1e-6 {
                break;
            }
            s = (s - err / d).clamp(0.0, 1.0);
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        s = t;
        for _ in 0..40 {
            let x = coord(self.x1, self.x2, s);
            if (x - t).abs() < 1e-6 {
                break;
            }
            if x < t {
                lo = s;
            } else {
                hi = s;
            }
            s = (lo + hi) / 2.0;
        }
        coord(self.y1, self.y2, s)
    }
}

/// Envelope value at which a spring counts as settled.
const SETTLE_LN: f32 = 6.9078; // -ln(0.001)
/// For a critically damped spring, (1 + wt)e^(-wt) = 0.001 at wt ≈ 9.233.
const CRITICAL_SETTLE: f32 = 9.233;

/// How progress evolves over normalized time 0..=1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Easing {
    Bezier(Bezier),
    /// A damped spring (the motion used throughout Apple's UI). The
    /// animation duration is the time it takes to settle, so springs and
    /// beziers with the same duration finish together. `damping` 1.0 never
    /// overshoots; lower values bounce a little.
    Spring { damping: f32 },
    /// A real mass-spring system (Apple's model): `omega` is the natural
    /// frequency in rad/s, `zeta` the damping ratio. It runs in real time
    /// until it settles and can start with a velocity.
    Physical { omega: f32, zeta: f32 },
}

/// One sample of an animation's progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub value: f32,
    /// Progress per second.
    pub velocity: f32,
    pub done: bool,
}

/// Physical springs never run longer than this.
const MAX_PHYSICAL_SECS: f32 = 3.0;

impl Easing {
    pub const LINEAR: Easing = Easing::Bezier(Bezier::LINEAR);

    pub fn from_spec(spec: minato_config::CurveSpec) -> Self {
        match spec {
            minato_config::CurveSpec::Bezier(p) => Easing::Bezier(Bezier::new(p)),
            minato_config::CurveSpec::Spring { spring } => Easing::Spring {
                damping: spring.clamp(0.1, 1.0),
            },
            minato_config::CurveSpec::AppleSpring { apple_spring: s } => Easing::Physical {
                omega: std::f32::consts::TAU / s.duration.clamp(0.05, 5.0),
                zeta: (1.0 - s.bounce).clamp(0.1, 1.0),
            },
        }
    }

    /// Progress at normalized time `t` (0..=1 of the nominal duration).
    /// Physical springs are sampled as if started at rest.
    pub fn eval(&self, t: f32) -> f32 {
        match *self {
            Easing::Bezier(b) => b.eval(t),
            Easing::Spring { damping } => spring(damping, t),
            Easing::Physical { .. } => self.sample(t, 1.0, 0.0).value,
        }
    }

    /// Progress after `elapsed` seconds of an animation nominally `nominal`
    /// seconds long. `v0` (progress per second) only affects physical
    /// springs, which also ignore `nominal`.
    pub fn sample(&self, elapsed: f32, nominal: f32, v0: f32) -> Sample {
        match *self {
            Easing::Physical { omega, zeta } => {
                let at = |t: f32| physical(omega, zeta, v0, t);
                let value = at(elapsed);
                let dt = 1e-3;
                let velocity = (at(elapsed + dt) - value) / dt;
                let settled = (value - 1.0).abs() < 1e-3 && velocity.abs() < 1e-2;
                Sample {
                    value: if settled { 1.0 } else { value },
                    velocity: if settled { 0.0 } else { velocity },
                    done: settled || elapsed >= MAX_PHYSICAL_SECS,
                }
            }
            _ => {
                let t = if nominal <= 0.0 { 1.0 } else { (elapsed / nominal).clamp(0.0, 1.0) };
                let value = self.eval(t);
                let dt = 1e-3;
                let velocity = if nominal <= 0.0 {
                    0.0
                } else {
                    (self.eval((t + dt).min(1.0)) - value) / (dt * nominal)
                };
                Sample {
                    value,
                    velocity,
                    done: t >= 1.0,
                }
            }
        }
    }
}

/// Position (0 → 1) of a unit-mass spring released from 0 with velocity
/// `v0` towards rest at 1.
fn physical(omega: f32, zeta: f32, v0: f32, t: f32) -> f32 {
    let d0 = -1.0; // start one unit away from the target
    let d = if zeta >= 0.999 {
        (d0 + (v0 + omega * d0) * t) * (-omega * t).exp()
    } else {
        let wd = omega * (1.0 - zeta * zeta).sqrt();
        (-zeta * omega * t).exp()
            * (d0 * (wd * t).cos() + ((v0 + zeta * omega * d0) / wd) * (wd * t).sin())
    };
    1.0 + d
}

fn spring(damping: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t >= 1.0 {
        return 1.0;
    }
    let z = damping.clamp(0.1, 1.0);
    if z >= 0.999 {
        let w = CRITICAL_SETTLE;
        1.0 - (1.0 + w * t) * (-w * t).exp()
    } else {
        let w = SETTLE_LN / z;
        let wd = w * (1.0 - z * z).sqrt();
        let decay = (-z * w * t).exp();
        1.0 - decay * ((wd * t).cos() + (z * w / wd) * (wd * t).sin())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_shape() {
        let s = Easing::Spring { damping: 1.0 };
        assert_eq!(s.eval(0.0), 0.0);
        assert_eq!(s.eval(1.0), 1.0);
        // Fast start: two thirds done after a quarter of the time.
        assert!(s.eval(0.25) > 0.6, "{}", s.eval(0.25));
        assert!((s.eval(0.95) - 1.0).abs() < 0.01);
        let mut last = 0.0;
        for i in 0..=100 {
            let v = s.eval(i as f32 / 100.0);
            assert!(v + 1e-5 >= last && v <= 1.0 + 1e-5);
            last = v;
        }
        // Under-damped springs overshoot, then settle.
        let b = Easing::Spring { damping: 0.6 };
        let max = (0..=100).map(|i| b.eval(i as f32 / 100.0)).fold(0.0, f32::max);
        assert!(max > 1.0);
        assert!((b.eval(0.99) - 1.0).abs() < 0.01);
    }

    fn smooth() -> Easing {
        Easing::from_spec(minato_config::CurveSpec::AppleSpring {
            apple_spring: minato_config::AppleSpring {
                duration: 0.5,
                bounce: 0.0,
            },
        })
    }

    #[test]
    fn apple_smooth_spring_timing() {
        // duration 0.5, bounce 0: omega = 4π, critically damped.
        let s = smooth();
        let v = |t: f32| s.sample(t, 0.0, 0.0).value;
        assert!(v(0.0).abs() < 1e-6);
        // ~90% of the way after ~0.31 s, essentially there by 0.55 s.
        assert!((v(0.31) - 0.90).abs() < 0.02, "{}", v(0.31));
        assert!(v(0.55) > 0.99);
        assert!(!s.sample(0.3, 0.0, 0.0).done);
        assert!(s.sample(1.0, 0.0, 0.0).done);
        // Monotonic, no overshoot.
        let mut last = 0.0;
        for i in 0..=100 {
            let x = v(i as f32 / 100.0);
            assert!(x + 1e-5 >= last && x <= 1.0 + 1e-4);
            last = x;
        }
    }

    #[test]
    fn initial_velocity_carries_momentum() {
        let s = smooth();
        let rest = s.sample(0.05, 0.0, 0.0);
        let moving = s.sample(0.05, 0.0, 6.0);
        assert!(moving.value > rest.value);
        // The velocity at t=0 is what we passed in.
        assert!((s.sample(0.0, 0.0, 6.0).velocity - 6.0).abs() < 0.1);
        // Going backwards first still ends at 1.
        assert!(s.sample(2.0, 0.0, -6.0).done);
    }

    #[test]
    fn bezier_sample_matches_eval() {
        let b = Easing::Bezier(Bezier::new([0.25, 0.1, 0.25, 1.0]));
        let smp = b.sample(0.1, 0.2, 0.0);
        assert!((smp.value - b.eval(0.5)).abs() < 1e-6);
        assert!(!smp.done && b.sample(0.2, 0.2, 0.0).done);
        assert!(smp.velocity > 0.0);
    }

    #[test]
    fn endpoints() {
        for p in [[0.25, 0.1, 0.25, 1.0], [0.2, 0.9, 0.1, 1.0], [0.0, 0.0, 1.0, 1.0]] {
            let b = Bezier::new(p);
            assert_eq!(b.eval(0.0), 0.0);
            assert_eq!(b.eval(1.0), 1.0);
            assert_eq!(b.eval(-1.0), 0.0);
            assert_eq!(b.eval(2.0), 1.0);
        }
    }

    #[test]
    fn linear_is_identity() {
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            assert!((Bezier::LINEAR.eval(t) - t).abs() < 1e-4);
        }
    }

    #[test]
    fn css_ease_known_values() {
        // CSS `ease` = cubic-bezier(0.25, 0.1, 0.25, 1.0); reference values
        // computed independently.
        let b = Bezier::new([0.25, 0.1, 0.25, 1.0]);
        assert!((b.eval(0.5) - 0.8024).abs() < 2e-3, "{}", b.eval(0.5));
        assert!((b.eval(0.25) - 0.4094).abs() < 2e-3, "{}", b.eval(0.25));
    }

    #[test]
    fn monotonic_for_non_overshooting_curves() {
        for p in [[0.25, 0.1, 0.25, 1.0], [0.42, 0.0, 0.58, 1.0], [0.2, 0.9, 0.1, 1.0], [0.0, 0.0, 0.2, 1.0]] {
            let b = Bezier::new(p);
            let mut last = 0.0;
            for i in 0..=200 {
                let v = b.eval(i as f32 / 200.0);
                assert!(v + 1e-5 >= last, "{p:?} not monotonic at {i}");
                last = v;
            }
        }
    }

    #[test]
    fn overshoot_is_allowed() {
        let b = Bezier::new([0.3, 1.6, 0.5, 1.0]);
        let max = (0..=100).map(|i| b.eval(i as f32 / 100.0)).fold(0.0, f32::max);
        assert!(max > 1.0);
    }
}
