//! `VERVE_FRAME_STATS=1`: the window draws frames one after the other and logs, every
//! [`REPORT_EVERY`] frames, the median and the 95th percentile of how long a frame took to build
//! and how far apart frames came. A frame is built from the start of its prepaint (the hitboxes
//! of the page) to the end of its paint.

use std::time::Instant;

/// How many frames the stats take between lines.
const REPORT_EVERY: usize = 240;

/// How long frames took to build and how far apart they came.
pub(crate) struct FrameStats {
    began: Option<Instant>,
    last: Option<Instant>,
    built: Vec<f32>,
    apart: Vec<f32>,
}

impl FrameStats {
    /// The stats `VERVE_FRAME_STATS` asks for, if it is set.
    pub(crate) fn from_env() -> Option<Self> {
        std::env::var_os("VERVE_FRAME_STATS").map(|_| Self {
            began: None,
            last: None,
            built: Vec::new(),
            apart: Vec::new(),
        })
    }

    /// A frame begins to be built.
    pub(crate) fn begin(&mut self) {
        self.began = Some(Instant::now());
    }

    /// The frame that began last was built.
    pub(crate) fn end(&mut self) {
        let Some(began) = self.began.take() else {
            return;
        };
        self.built.push(began.elapsed().as_secs_f32() * 1000.);
        if let Some(last) = self.last.replace(began) {
            self.apart.push((began - last).as_secs_f32() * 1000.);
        }
        if self.built.len() >= REPORT_EVERY {
            eprintln!(
                "frames: {} built p50 {:.2} ms p95 {:.2} ms, apart p50 {:.2} ms p95 {:.2} ms",
                self.built.len(),
                percentile(&mut self.built, 0.5),
                percentile(&mut self.built, 0.95),
                percentile(&mut self.apart, 0.5),
                percentile(&mut self.apart, 0.95),
            );
            self.built.clear();
            self.apart.clear();
        }
    }
}

/// The value `share` of the way up the sorted values, `0` for none.
fn percentile(values: &mut [f32], share: f32) -> f32 {
    values.sort_by(f32::total_cmp);
    match values.len() {
        0 => 0.,
        n => values[((n - 1) as f32 * share).round() as usize],
    }
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn the_median_and_the_95th_percentile_of_a_hundred_values() {
        let mut values: Vec<f32> = (1..=100).rev().map(|n| n as f32).collect();
        assert_eq!(percentile(&mut values, 0.5), 51.);
        assert_eq!(percentile(&mut values, 0.95), 95.);
        assert_eq!(percentile(&mut [], 0.5), 0.);
    }
}
