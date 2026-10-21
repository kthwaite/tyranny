pub type Frame = [f32; 2];
pub const SAMPLE_RATE: u32 = 48_000;
pub const BLOCK_FRAMES: usize = 1024;

mod artwork;
pub mod decoder;
// wired in by the directory protocol commands
#[cfg_attr(not(test), expect(dead_code))]
mod directory;
pub mod equalizer;
mod filter;
pub mod library;
pub mod library_types;
pub mod listing;
mod lyrics;
mod metadata;
pub mod output;
pub mod owner;
pub mod play_order;
pub mod playback;
pub mod protocol;
mod scanner;
pub mod server;
mod store;
mod stream;
