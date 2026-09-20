//! Human-like browser interactions
//!
//! Simulates realistic mouse movements and typing patterns to avoid
//! behavior-based bot detection.

mod drag;

use drag::{validate_drag, PendingRelease};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;

use crate::cdp::Session;
use crate::error::Result;
use crate::page::{
    coordinated_key_char, coordinated_key_down, coordinated_key_up, coordinated_mouse_down,
    coordinated_mouse_move, coordinated_mouse_up, coordinated_mouse_wheel, HeldInputState,
    MouseButton,
};

/// Speed mode for human simulation
#[derive(Debug, Clone, Copy, Default)]
pub enum HumanSpeed {
    /// Fast mode - minimal delays
    Fast,
    /// Normal mode - balanced
    #[default]
    Normal,
    /// Slow mode - maximum realism
    Slow,
}

impl HumanSpeed {
    fn mouse_points(&self, distance: f64) -> usize {
        match self {
            HumanSpeed::Fast => (distance / 50.0).clamp(3.0, 10.0) as usize,
            HumanSpeed::Normal => (distance / 10.0).clamp(10.0, 50.0) as usize,
            HumanSpeed::Slow => (distance / 5.0).clamp(20.0, 100.0) as usize,
        }
    }

    fn move_delay_ms(&self) -> (u64, u64) {
        match self {
            HumanSpeed::Fast => (1, 5),
            HumanSpeed::Normal => (5, 25),
            HumanSpeed::Slow => (10, 50),
        }
    }

    fn type_delay_ms(&self) -> (u64, u64) {
        match self {
            HumanSpeed::Fast => (10, 30),
            HumanSpeed::Normal => (50, 150),
            HumanSpeed::Slow => (100, 300),
        }
    }
}

fn random_range(min: u64, max: u64) -> u64 {
    debug_assert!(
        min < max,
        "random_range: min ({}) must be less than max ({})",
        min,
        max
    );
    if min >= max {
        return min;
    }
    fastrand::u64(min..max)
}

fn random_f64_range(min: f64, max: f64) -> f64 {
    debug_assert!(
        min < max,
        "random_f64_range: min ({}) must be less than max ({})",
        min,
        max
    );
    if min >= max {
        return min;
    }
    min + fastrand::f64() * (max - min)
}

fn random_bool(probability: f64) -> bool {
    fastrand::f64() < probability
}

/// Point type
type Point = (f64, f64);

/// Generate Bezier curve for natural mouse movement
fn bezier_curve(start: Point, end: Point, num_points: usize) -> Vec<Point> {
    let num_points = num_points.max(2);

    let cp1 = (
        start.0 + (end.0 - start.0) * random_f64_range(0.2, 0.4) + random_f64_range(-50.0, 50.0),
        start.1 + (end.1 - start.1) * random_f64_range(0.0, 0.3) + random_f64_range(-50.0, 50.0),
    );
    let cp2 = (
        start.0 + (end.0 - start.0) * random_f64_range(0.6, 0.8) + random_f64_range(-50.0, 50.0),
        start.1 + (end.1 - start.1) * random_f64_range(0.7, 1.0) + random_f64_range(-50.0, 50.0),
    );

    let mut points = Vec::with_capacity(num_points);

    for i in 0..num_points {
        let t = i as f64 / (num_points - 1) as f64;
        let t2 = t * t;
        let t3 = t2 * t;
        let mt = 1.0 - t;
        let mt2 = mt * mt;
        let mt3 = mt2 * mt;

        // Clamp to >= 0 so we never dispatch a negative mouse position.
        let x =
            (mt3 * start.0 + 3.0 * mt2 * t * cp1.0 + 3.0 * mt * t2 * cp2.0 + t3 * end.0).max(0.0);
        let y =
            (mt3 * start.1 + 3.0 * mt2 * t * cp1.1 + 3.0 * mt * t2 * cp2.1 + t3 * end.1).max(0.0);

        points.push((x, y));
    }

    points
}

/// Human-like interaction helpers
pub struct Human<'a> {
    session: &'a Session,
    /// Per-target state obtained from Session, shared with every Page clone.
    held_input: Arc<tokio::sync::Mutex<HeldInputState>>,
    speed: HumanSpeed,
    pending_releases: Mutex<Vec<PendingRelease>>,
    cleanup_wait: tokio::sync::Mutex<()>,
}

impl<'a> Human<'a> {
    /// Create a Human helper whose input shares the session's Page coordinator.
    pub fn new(session: &'a Session) -> Self {
        Self {
            session,
            held_input: session.held_input(),
            speed: HumanSpeed::Normal,
            pending_releases: Mutex::new(Vec::new()),
            cleanup_wait: tokio::sync::Mutex::new(()),
        }
    }

    /// Set the speed mode
    pub fn with_speed(mut self, speed: HumanSpeed) -> Self {
        self.speed = speed;
        self
    }

    /// Move mouse to target position with human-like Bezier curve
    pub async fn move_to(&self, target_x: f64, target_y: f64) -> Result<()> {
        // Start from a random position relative to the target — this looks more
        // natural than a fixed range and works on any viewport size.
        let offset_x = random_f64_range(-300.0, 300.0);
        let offset_y = random_f64_range(-200.0, 200.0);
        let start_x = (target_x + offset_x).max(0.0);
        let start_y = (target_y + offset_y).max(0.0);

        let distance = ((target_x - start_x).powi(2) + (target_y - start_y).powi(2)).sqrt();
        let num_points = self.speed.mouse_points(distance);
        let (min_delay, max_delay) = self.speed.move_delay_ms();

        let path = bezier_curve((start_x, start_y), (target_x, target_y), num_points);

        // Move through path using Page's coordinator when available.
        for (x, y) in path {
            self.mouse_move(x, y).await?;
            sleep(Duration::from_millis(random_range(min_delay, max_delay))).await;
        }

        Ok(())
    }

    /// Move mouse to target and click
    pub async fn move_and_click(&self, target_x: f64, target_y: f64) -> Result<()> {
        self.move_to(target_x, target_y).await?;

        // Small delay before click
        sleep(Duration::from_millis(random_range(50, 150))).await;

        // Click with slight jitter
        let click_x = target_x + random_f64_range(-2.0, 2.0);
        let click_y = target_y + random_f64_range(-2.0, 2.0);

        self.mouse_down(click_x, click_y).await?;

        sleep(Duration::from_millis(random_range(50, 120))).await;

        self.mouse_up(click_x, click_y).await?;

        // Small delay after click
        sleep(Duration::from_millis(random_range(30, 100))).await;

        Ok(())
    }

    async fn mouse_move(&self, x: f64, y: f64) -> Result<()> {
        coordinated_mouse_move(self.session, &self.held_input, x, y).await
    }

    async fn mouse_down(&self, x: f64, y: f64) -> Result<()> {
        coordinated_mouse_down(self.session, &self.held_input, x, y, MouseButton::Left).await
    }

    async fn mouse_up(&self, x: f64, y: f64) -> Result<()> {
        coordinated_mouse_up(self.session, &self.held_input, x, y, MouseButton::Left).await
    }

    async fn mouse_wheel(&self, x: f64, y: f64, delta_x: f64, delta_y: f64) -> Result<()> {
        coordinated_mouse_wheel(self.session, &self.held_input, x, y, delta_x, delta_y).await
    }

    /// Press at `(x, y)`, drag horizontally by `dx` pixels, release.
    ///
    /// Designed for slider-captcha drags, where the trajectory itself is
    /// scored: ease-out velocity (fast start, decelerating approach), small
    /// y-jitter, then an overshoot past the target and a settle back onto the
    /// exact offset before release — real humans rarely stop dead on target.
    ///
    /// The operation exclusively owns the target input coordinator until release;
    /// existing non-left buttons are preserved, and an already-held left button fails.
    /// Cancellation schedules a bounded release; retain this helper to confirm it
    /// with [`Human::finish_drag_cleanup`].
    ///
    /// `dx` may be negative (drag left). Overshoot is skipped when `dx` is
    /// too small to make it plausible.
    pub async fn drag_by(&self, x: f64, y: f64, dx: f64) -> Result<()> {
        validate_drag(x, y, dx)?;
        let mut release = self.approach_drag(x, y).await?;
        sleep(Duration::from_millis(random_range(80, 200))).await;
        let moved = async {
            release.press().await?;
            sleep(Duration::from_millis(random_range(60, 140))).await;

            // Overshoot only when the drag is long enough to make it plausible.
            let overshoot = if dx.abs() > 40.0 {
                random_f64_range(4.0, 14.0).min(dx.abs() * 0.3) * dx.signum()
            } else {
                0.0
            };

            let target_x = (x + dx).max(0.0);
            let over_x = (x + dx + overshoot).max(0.0);
            let end_y = (y + random_f64_range(-3.0, 3.0)).max(0.0);

            let distance = (over_x - x).abs();
            let num_points = self.speed.mouse_points(distance);
            let (min_delay, max_delay) = self.speed.move_delay_ms();

            let path = bezier_curve((x, y), (over_x, end_y), num_points);

            // Ease-out: delay grows along the path so the drag decelerates into
            // the target instead of arriving at constant speed.
            let n = path.len().max(2);
            for (i, (px, py)) in path.into_iter().enumerate() {
                let t = i as f64 / (n - 1) as f64;
                let jitter_y = if i > 0 && i < n - 1 {
                    random_f64_range(-1.5, 1.5)
                } else {
                    0.0
                };
                release.move_to(px, (py + jitter_y).max(0.0)).await?;
                let delay = random_range(min_delay, max_delay);
                let eased = (delay as f64 * (0.5 + 2.0 * t * t)) as u64;
                sleep(Duration::from_millis(eased.max(min_delay))).await;
            }

            // Settle back from the overshoot onto the exact target.
            if overshoot != 0.0 {
                sleep(Duration::from_millis(random_range(60, 150))).await;
                let settle_points = random_range(3, 6);
                for i in 1..=settle_points {
                    let t = i as f64 / settle_points as f64;
                    let sx = over_x + (target_x - over_x) * t;
                    let sy = (end_y + (y - end_y) * t + random_f64_range(-0.8, 0.8)).max(0.0);
                    release.move_to(sx.max(0.0), sy).await?;
                    sleep(Duration::from_millis(random_range(20, 60))).await;
                }
            }

            sleep(Duration::from_millis(random_range(30, 100))).await;
            release.x = target_x;
            release.y = y;
            Ok(())
        }
        .await;
        release.release().await.and(moved)
    }

    /// Drag on a horizontal track without overshoot, reversal or vertical drift.
    /// Intended for bounded sliders whose target is the end of the track.
    /// Shares the ownership and cancellation contract of [`Human::drag_by`].
    pub async fn drag_horizontal_by(&self, x: f64, y: f64, dx: f64) -> Result<()> {
        validate_drag(x, y, dx)?;
        let mut release = self.approach_drag(x, y).await?;
        sleep(Duration::from_millis(100)).await;
        let moved = async {
            release.press().await?;
            let (points, delay) = match self.speed {
                HumanSpeed::Fast => (25, 20),
                HumanSpeed::Normal => (60, 20),
                HumanSpeed::Slow => (100, 25),
            };
            sleep(Duration::from_millis(100)).await;
            for i in 1..=points {
                let t = i as f64 / points as f64;
                let next_x = x + dx * (t * t * (3.0 - 2.0 * t));
                release.move_to(next_x, y).await?;
                sleep(Duration::from_millis(delay)).await;
            }
            Ok(())
        }
        .await;
        release.release().await.and(moved)
    }

    /// Type text with human-like timing
    pub async fn type_text(&self, text: &str) -> Result<()> {
        let (min_delay, max_delay) = self.speed.type_delay_ms();

        for ch in text.chars() {
            // Type through the shared key coordinator so held modifiers are
            // reflected in the native CDP event.
            coordinated_key_char(self.session, &self.held_input, &ch.to_string()).await?;

            // Variable delay based on character
            let base_delay = if ch == ' ' {
                random_range(min_delay + 30, max_delay + 30)
            } else if ch.is_ascii_punctuation() {
                random_range(min_delay + 50, max_delay + 50)
            } else {
                random_range(min_delay, max_delay)
            };

            // Occasional thinking pause
            let delay = if matches!(self.speed, HumanSpeed::Normal | HumanSpeed::Slow)
                && random_bool(0.05)
            {
                base_delay + random_range(200, 500)
            } else {
                base_delay
            };

            sleep(Duration::from_millis(delay)).await;

            // Occasional typo (slow mode only)
            if matches!(self.speed, HumanSpeed::Slow) && random_bool(0.01) && text.len() > 10 {
                let wrong_char = (b'a' + random_range(0, 26) as u8) as char;
                coordinated_key_char(self.session, &self.held_input, &wrong_char.to_string())
                    .await?;
                sleep(Duration::from_millis(random_range(100, 300))).await;

                // Backspace through the shared held-key coordinator.
                coordinated_key_down(self.session, &self.held_input, "Backspace").await?;
                coordinated_key_up(self.session, &self.held_input, "Backspace").await?;
                sleep(Duration::from_millis(random_range(50, 150))).await;
            }
        }

        Ok(())
    }

    /// Press a key through the session's held-key coordinator.
    pub async fn press_key(&self, key: &str) -> Result<()> {
        coordinated_key_down(self.session, &self.held_input, key).await?;
        sleep(Duration::from_millis(random_range(50, 100))).await;
        coordinated_key_up(self.session, &self.held_input, key).await
    }

    /// Scroll the page by delta_y pixels (positive = down, negative = up)
    pub async fn scroll(&self, delta_y: f64) -> Result<()> {
        let num_scrolls = random_range(3, 8);
        let per_scroll = delta_y / num_scrolls as f64;

        for _ in 0..num_scrolls {
            let jitter = random_f64_range(-20.0, 20.0);
            let scroll_amount = per_scroll + jitter;

            self.mouse_wheel(
                random_f64_range(400.0, 800.0),
                random_f64_range(300.0, 600.0),
                0.0,
                scroll_amount,
            )
            .await?;

            sleep(Duration::from_millis(random_range(30, 100))).await;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bezier_curve_endpoints() {
        let start = (50.0, 75.0);
        let end = (200.0, 300.0);

        let points = bezier_curve(start, end, 10);

        let first = points.first().unwrap();
        assert!((first.0 - start.0).abs() < 0.001);
        assert!((first.1 - start.1).abs() < 0.001);

        let last = points.last().unwrap();
        assert!((last.0 - end.0).abs() < 0.001);
        assert!((last.1 - end.1).abs() < 0.001);
    }

    #[test]
    fn test_bezier_curve_clamps_to_nonnegative() {
        // Target very near the origin: the randomized control points can push
        // the raw cubic below zero, so every emitted point must be clamped to
        // >= 0 (and stay finite). Run many iterations since control points and
        // start offsets are randomized.
        for _ in 0..500 {
            let start = (0.0, 0.0);
            let end = (2.0, 2.0);
            let points = bezier_curve(start, end, 25);
            for (x, y) in points {
                assert!(x >= 0.0, "x should be clamped to >= 0, got {x}");
                assert!(y >= 0.0, "y should be clamped to >= 0, got {y}");
                assert!(x.is_finite(), "x should be finite, got {x}");
                assert!(y.is_finite(), "y should be finite, got {y}");
            }
        }
    }

    #[test]
    fn test_human_speed_mouse_points() {
        let distance = 500.0;

        let fast = HumanSpeed::Fast.mouse_points(distance);
        let normal = HumanSpeed::Normal.mouse_points(distance);
        let slow = HumanSpeed::Slow.mouse_points(distance);

        assert!(fast < normal);
        assert!(normal < slow);
    }
}
