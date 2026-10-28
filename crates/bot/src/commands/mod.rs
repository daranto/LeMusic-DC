//! Slash-Commands.

mod playback;
mod playlist;
mod queue;

use crate::{Data, Error};

pub fn all() -> Vec<poise::Command<Data, Error>> {
    vec![
        playback::play(),
        playback::search(),
        playback::join(),
        playback::leave(),
        playback::skip(),
        playback::forceskip(),
        playback::pause(),
        playback::resume(),
        playback::stop(),
        playback::seek(),
        playback::volume(),
        playback::nowplaying(),
        queue::queue(),
        queue::skipto(),
        queue::remove(),
        queue::move_track(),
        queue::shuffle(),
        queue::clear(),
        queue::repeat(),
        playlist::playlist(),
    ]
}
