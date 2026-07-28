pub const BASE_SCORE: f64 = 50.0;
pub const SWIPE_DELTA: f64 = 10.0;
pub const SWIPE_DOWN_DELTA: f64 = 0.0;
pub const ARENA_DELTA: f64 = 8.0;
pub const MIN_SCORE: f64 = 0.0;
pub const MAX_SCORE: f64 = 100.0;
pub const ARENA_THRESHOLD: f64 = 5.0;

pub fn apply_swipe(current: Option<f64>, gesture: &str) -> f64 {
    let cur = current.unwrap_or(BASE_SCORE);
    let next = match gesture {
        "right" => cur + SWIPE_DELTA,         // 优
        "left" => cur - SWIPE_DELTA,          // 差
        "up" => cur + SWIPE_DELTA / 2.0,      // 待优化（轻正向）
        "down" => cur,                        // 跳过
        _ => cur,
    };
    next.clamp(MIN_SCORE, MAX_SCORE)
}

pub fn apply_arena(left: f64, right: f64, winner_is_left: bool) -> (f64, f64) {
    // Bradley-Terry style: expected from current scores, then nudge.
    let expected_left = 1.0 / (1.0 + 10f64.powf((right - left) / 40.0));
    let actual_left = if winner_is_left { 1.0 } else { 0.0 };
    let delta = ARENA_DELTA * (actual_left - expected_left);
    let nl = (left + delta).clamp(MIN_SCORE, MAX_SCORE);
    let nr = (right - delta).clamp(MIN_SCORE, MAX_SCORE);
    (nl, nr)
}

pub fn arena_suggested(a: f64, b: f64) -> bool {
    (a - b).abs() < ARENA_THRESHOLD
}
