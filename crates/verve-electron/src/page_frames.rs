//! Pictures with more than one frame: decoding them, and choosing the frame to show.
//!
//! GIF, APNG and animated WebP are read frame by frame (each frame already composed onto the
//! whole picture) with the codecs `image` links. Which frame is on show is a function of the time
//! since the picture first appeared in a display list: [`frame_at`] answers it, and how long
//! until it changes, so that the page can sleep till then.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Frame, ImageFormat};
use verve_layout::ImageFrames;

/// Frames of a picture that take longer to hold than this are not read: they would take more
/// RAM than the pictures of a whole page should.
const MOST_BYTES: usize = 256 << 20;

/// A frame that asks to be shown for no longer than this is shown for [`SLOWEST_BLINK`], as
/// Chromium does (`gif_image_decoder.cc`: "Many annoying ads specify a 0 duration to make an
/// image flash as quickly as possible. We follow Firefox's behavior and use a duration of 100 ms
/// for any frames that specify a duration of <= 10 ms").
const BLINK: Duration = Duration::from_millis(10);
const SLOWEST_BLINK: Duration = Duration::from_millis(100);

/// The frames of an animated picture, or `None` when `bytes` are not one (a still picture, or
/// not a picture at all).
pub(crate) fn animated(bytes: &[u8]) -> Option<ImageFrames> {
    let frames = match image::guess_format(bytes).ok()? {
        ImageFormat::Gif => {
            let decoder = GifDecoder::new(Cursor::new(bytes)).ok()?;
            let mut read = read(decoder.into_frames())?;
            read.loops = gif_loops(bytes);
            read
        }
        ImageFormat::WebP => {
            let decoder = WebPDecoder::new(Cursor::new(bytes)).ok()?;
            if !decoder.has_animation() {
                return None;
            }
            let mut read = read(decoder.into_frames())?;
            read.loops = webp_loops(bytes);
            read
        }
        ImageFormat::Png => {
            let decoder = PngDecoder::new(Cursor::new(bytes)).ok()?;
            if !decoder.is_apng().ok()? {
                return None;
            }
            let mut read = read(decoder.apng().ok()?.into_frames())?;
            read.loops = apng_loops(bytes);
            read
        }
        _ => return None,
    };
    (frames.frames.len() > 1).then_some(frames)
}

fn read(frames: image::Frames<'_>) -> Option<ImageFrames> {
    let mut out: Vec<(Arc<[u8]>, Duration)> = Vec::new();
    let (mut width, mut height) = (0, 0);
    let mut held = 0;
    for frame in frames {
        let frame: Frame = frame.ok()?;
        let delay = Duration::from(frame.delay());
        let image = frame.into_buffer();
        (width, height) = image.dimensions();
        let mut pixels = image.into_raw();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        held += pixels.len();
        if held > MOST_BYTES {
            break;
        }
        out.push((Arc::from(pixels), delay));
    }
    (width > 0 && height > 0 && !out.is_empty()).then_some(ImageFrames {
        width,
        height,
        frames: out,
        loops: 0,
    })
}

/// How many times a GIF plays: once without the NETSCAPE2.0 loop extension, for ever when it
/// counts 0, and the count and one more otherwise (the count is how many times it repeats).
fn gif_loops(bytes: &[u8]) -> u32 {
    const EXTENSION: &[u8] = b"NETSCAPE2.0";
    let Some(at) = bytes
        .windows(EXTENSION.len())
        .position(|window| window == EXTENSION)
    else {
        return 1;
    };
    match bytes[at + EXTENSION.len()..] {
        [3, 1, low, high, ..] => match u32::from(low) | u32::from(high) << 8 {
            0 => 0,
            count => count + 1,
        },
        _ => 1,
    }
}

/// How many times an APNG plays: the `num_plays` of its `acTL` chunk (0 is for ever). Observed in
/// Electron 39.8.10: `num_plays` 1 and 2 play once and twice.
fn apng_loops(bytes: &[u8]) -> u32 {
    let Some(at) = bytes.windows(4).position(|window| window == b"acTL") else {
        return 0;
    };
    bytes.get(at + 8..at + 12).map_or(0, |plays| {
        u32::from_be_bytes([plays[0], plays[1], plays[2], plays[3]])
    })
}

/// How many times an animated WebP plays: the loop count in its `ANIM` chunk, 2 bytes little
/// endian after the 4 of the background colour (0 is for ever). Observed in Electron 39.8.10:
/// loop counts 1 and 2 play once and twice.
fn webp_loops(bytes: &[u8]) -> u32 {
    let Some(at) = bytes.windows(4).position(|window| window == b"ANIM") else {
        return 0;
    };
    bytes
        .get(at + 12..at + 14)
        .map_or(0, |count| u32::from(count[0]) | u32::from(count[1]) << 8)
}

/// How long frame `delay` is shown.
fn shown_for(delay: Duration) -> Duration {
    if delay <= BLINK { SLOWEST_BLINK } else { delay }
}

/// The frame of `frames` shown `elapsed` after it appeared, and how long until another one is
/// (`None` when it never changes again: a picture of one frame, or one that has played out).
pub(crate) fn frame_at(frames: &ImageFrames, elapsed: Duration) -> (u32, Option<Duration>) {
    let total: Duration = frames.frames.iter().map(|(_, d)| shown_for(*d)).sum();
    if frames.frames.len() < 2 || total.is_zero() {
        return (0, None);
    }
    let last = (frames.frames.len() - 1) as u32;
    if frames.loops > 0 && elapsed >= total * frames.loops {
        return (last, None);
    }
    let into = Duration::from_nanos((elapsed.as_nanos() % total.as_nanos()) as u64);
    let mut start = Duration::ZERO;
    for (index, (_, delay)) in frames.frames.iter().enumerate() {
        let end = start + shown_for(*delay);
        if into < end {
            return (index as u32, Some(end - into));
        }
        start = end;
    }
    (last, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(delays_ms: &[u64], loops: u32) -> ImageFrames {
        ImageFrames {
            width: 1,
            height: 1,
            frames: delays_ms
                .iter()
                .map(|ms| (Arc::from(vec![0u8; 4]), Duration::from_millis(*ms)))
                .collect(),
            loops,
        }
    }

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn the_frame_follows_the_time_and_the_wait_is_to_the_next_one() {
        let pictures = frames(&[200, 300], 0);
        assert_eq!(frame_at(&pictures, MS(0)), (0, Some(MS(200))));
        assert_eq!(frame_at(&pictures, MS(250)), (1, Some(MS(250))));
        assert_eq!(frame_at(&pictures, MS(500)), (0, Some(MS(200))));
    }

    #[test]
    fn a_frame_of_ten_milliseconds_or_less_is_shown_for_a_hundred() {
        let pictures = frames(&[0, 10, 11], 0);
        assert_eq!(frame_at(&pictures, MS(99)), (0, Some(MS(1))));
        assert_eq!(frame_at(&pictures, MS(100)), (1, Some(MS(100))));
        assert_eq!(frame_at(&pictures, MS(200)), (2, Some(MS(11))));
    }

    #[test]
    fn an_animation_that_has_played_out_stays_on_its_last_frame() {
        let pictures = frames(&[100, 100], 2);
        assert_eq!(frame_at(&pictures, MS(399)).0, 1);
        assert_eq!(frame_at(&pictures, MS(150)).0, 1);
        assert_eq!(frame_at(&pictures, MS(400)), (1, None));
        assert_eq!(frame_at(&pictures, MS(10_000)), (1, None));
    }

    #[test]
    fn a_still_picture_never_changes() {
        assert_eq!(frame_at(&frames(&[100], 0), MS(500)), (0, None));
    }

    #[test]
    fn apng_and_webp_play_as_many_times_as_they_count() {
        let apng = [&b"PNG....acTL"[..], &[0, 0, 0, 2, 0, 0, 0, 3]].concat();
        assert_eq!(apng_loops(&apng), 3);
        let webp = [
            &b"RIFF....WEBPANIM"[..],
            &[6, 0, 0, 0, 255, 255, 255, 255, 2, 0],
        ]
        .concat();
        assert_eq!(webp_loops(&webp), 2);
    }

    #[test]
    fn a_gif_plays_as_often_as_its_loop_extension_says() {
        let mut bytes = b"GIF89a".to_vec();
        assert_eq!(gif_loops(&bytes), 1);
        bytes.extend_from_slice(b"NETSCAPE2.0\x03\x01\x00\x00\x00");
        assert_eq!(gif_loops(&bytes), 0);
        let bytes = [&b"GIF89aNETSCAPE2.0"[..], &[3, 1, 2, 0, 0]].concat();
        assert_eq!(gif_loops(&bytes), 3);
    }
}
