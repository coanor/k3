use std::{error::Error, path::Path};

use k3_app::AudioPlayer;

type OpenPlayer = fn(&Path, i8) -> Result<AudioPlayer, Box<dyn Error>>;

#[test]
fn frontends_can_open_the_shared_audio_player_paused() {
    let open: OpenPlayer = AudioPlayer::open_paused;
    let _ = open;
}
