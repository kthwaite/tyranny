## Formats

Input: Ogg Vorbis, MP3, FLAC, Opus, Musepack, WavPack, WAV, AAC, MP4, audio CD, and everything supported by ffmpeg (WMA, APE, MKA, TTA, SHN, ...), libmodplug and BASS
Output: PulseAudio, ALSA, OSS, JACK, RoarAudio, CoreAudio, libao, sndio (OpenBSD), aRts, Sun

## Playing

- Gapless playback
- ReplayGain support
- MP3 and Ogg streaming (SHOUTcast/Icecast)
- Play queue
- Optional playback resume on startup

## Interface

- Simple onboarding - click the prominent button, choose your music directory, wait for the obvious progress indicator to fill up, bam, done
- Comfortable three-pane library screen 'reminiscent of iTunes when it was good'
- Native desktop notifications
- Smart and dynamic playlists
- Automatically retrieve tags from MusicBrainz
- Album cover art from: Last.fm, MusicBrainz, Discogs, Musixmatch, Deezer, Tidal, Qobuz, Spotify
- Audio analyzer
- Audio equalizer
- Instant startup, even with thousands of tracks
- Internet radio support
- Playlist creation & modification
- Powerful playlist filters / live filtering
  - Boolean syntax: `&` for AND, `!` for NOT, and `|` for OR
  - Parentheses for grouping, with NOT > AND > OR precedence
  - Same-field alternatives: `artist:["Miles Davis", "Ornette Coleman"]`
- Easy to use directory browser
- Customizable colors
- Dynamic keybindings. You can bind a key to any command, :seek +1m for example
- Vi / less style search mode
- Vi style command mode with tab completion
- Drag and drop rows with multi-select
- Modify and save playlists intuitively
- Handles large playlists and queues
- Global mediakeys (and partial MPRIS) support
- Lyrics from: Genius, Musixmatch, lyrics.ovh, songlyrics, azlyrics, elyrics, letras and lrclib.net

## Misc

- CUE sheets support
- Excellent compilations handling
- Uses Unicode internally for all string handling
- Supports audio scrobbling (to e.g. Last.fm or Libre.fm) etc.
- Can run external commands for the currently selected files (tag-editor for example)
- UI updates to reflect currently playing track's cover art colors
- Control the player through MPRIS
