//! Движение: пружины для всего, что отзывается на действия, и кривые для заданных сценок.

#[derive(Clone, Copy, Debug)]
pub struct Tune {
    pub stiffness: f32,
    pub damping: f32,
}

/// Быстрый отклик элементов: без перелёта.
pub const SNAP: Tune = Tune { stiffness: 420.0, damping: 38.0 };
/// Панели и выделение: с лёгким дыханием в конце.
pub const SOFT: Tune = Tune { stiffness: 190.0, damping: 23.0 };
/// Появление: заметный, но короткий перелёт.
pub const BOUNCE: Tune = Tune { stiffness: 300.0, damping: 19.0 };
/// Камера схемы: тяжёлая и плавная.
pub const CAMERA: Tune = Tune { stiffness: 95.0, damping: 19.5 };

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
}

impl Spring {
    pub fn new(value: f32) -> Self {
        Self { value, velocity: 0.0, target: value }
    }

    /// Шаг с мелким дроблением: при длинном кадре пружина не должна разлететься.
    pub fn step(&mut self, dt: f32, tune: Tune) {
        let mut left = dt.min(0.1);
        while left > 0.0 {
            let h = left.min(1.0 / 240.0);
            let force = (self.target - self.value) * tune.stiffness - self.velocity * tune.damping;
            self.velocity += force * h;
            self.value += self.velocity * h;
            left -= h;
        }
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < 0.0015 * self.target.abs().max(1.0) && self.velocity.abs() < 0.02
    }

    pub fn set(&mut self, value: f32) {
        *self = Self::new(value);
    }
}

pub fn clamp01(t: f32) -> f32 {
    t.clamp(0.0, 1.0)
}

pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - clamp01(t)).powi(3)
}

/// С перелётом: станция выскакивает на линию и садится на место.
pub fn ease_back(t: f32) -> f32 {
    let t = clamp01(t);
    let c = 2.2;
    1.0 + (c + 1.0) * (t - 1.0).powi(3) + c * (t - 1.0).powi(2)
}

pub fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = clamp01((x - low) / (high - low));
    t * t * (3.0 - 2.0 * t)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_arrives_and_stops() {
        let mut spring = Spring::new(0.0);
        spring.target = 100.0;
        for _ in 0..300 {
            spring.step(1.0 / 60.0, SOFT);
        }
        assert_eq!(spring.value, 100.0);
    }

    #[test]
    fn long_frame_does_not_blow_up() {
        let mut spring = Spring::new(0.0);
        spring.target = 1.0;
        spring.step(2.0, BOUNCE);
        assert!(spring.value.abs() < 2.0);
    }

    #[test]
    fn easing_ends_are_exact() {
        for curve in [ease_out, ease_back] {
            assert!(curve(0.0).abs() < 1e-5);
            assert!((curve(1.0) - 1.0).abs() < 1e-5);
        }
    }
}
