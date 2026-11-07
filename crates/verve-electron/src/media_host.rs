//! The host of `<video>` and `<audio>`: the page's media commands answered.
//!
//! Verve plays no sound and decodes no video, so every source is one the host cannot play
//! (`MEDIA_ERR_SRC_NOT_SUPPORTED`, code 4, which is what Chromium 142 reports for a source it
//! cannot play: observed in Electron 39.8.10 for a `<video src>` that is not there). The
//! [`MediaBackend`] is the seam a platform decoder would implement (it would also hand over the
//! current frame of a video, which nothing draws yet); the poster of a `<video>` is
//! drawn as a picture by the layout, not by the backend.

use verve_dom::{MediaCommand, MediaCommandKind, MediaUpdate};

/// Answers the media commands of a page.
pub(crate) trait MediaBackend {
    /// What the backend reports for `command`, in the order it happened.
    fn handle(&mut self, command: &MediaCommand) -> Vec<MediaUpdate>;
}

/// The backend that supports nothing.
pub(crate) struct Unsupported;

impl MediaBackend for Unsupported {
    fn handle(&mut self, command: &MediaCommand) -> Vec<MediaUpdate> {
        match &command.kind {
            MediaCommandKind::Load { .. } => vec![MediaUpdate::Error {
                code: 4,
                message: "MEDIA_ELEMENT_ERROR: Format error".into(),
            }],
            MediaCommandKind::Play => vec![MediaUpdate::CommandFailed {
                request_id: command.request_id,
                // Observed in Electron 39.8.10: the rejection of `play()` is
                // `NotSupportedError: Failed to load because no supported source was found.`
                message: "Failed to load because no supported source was found.".into(),
            }],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(kind: MediaCommandKind) -> MediaCommand {
        MediaCommand {
            node: 3,
            generation: 1,
            request_id: 9,
            kind,
        }
    }

    #[test]
    fn a_load_fails_with_code_four_and_a_play_is_refused() {
        let mut host = Unsupported;
        let load = MediaCommandKind::Load {
            src: "a.mp4".into(),
            settings: Default::default(),
        };
        let answer = host.handle(&command(load));
        assert!(matches!(
            answer.as_slice(),
            [MediaUpdate::Error { code: 4, .. }]
        ));
        let play = host.handle(&command(MediaCommandKind::Play));
        assert!(matches!(
            play.as_slice(),
            [MediaUpdate::CommandFailed { request_id: 9, .. }]
        ));
        assert!(host.handle(&command(MediaCommandKind::Dispose)).is_empty());
    }
}
