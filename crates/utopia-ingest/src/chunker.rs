//! Chunking: text-splitter semantic chunking (paragraph and sentence boundaries first),
//! character budget + overlap.

use text_splitter::{ChunkConfig, TextSplitter};

#[derive(Debug, Clone)]
pub struct ChunkPiece {
    pub seq: i32,
    pub text: String,
    pub char_start: i32,
    pub char_end: i32,
}

/// Default budget: 1200 characters (roughly 1000+ tokens for Chinese), overlap 150.
pub fn chunk_text(text: &str) -> Vec<ChunkPiece> {
    let config = ChunkConfig::new(1200)
        .with_overlap(150)
        .expect("overlap < capacity");
    let splitter = TextSplitter::new(config);
    splitter
        .chunk_indices(text)
        .enumerate()
        .map(|(i, (offset, chunk))| ChunkPiece {
            seq: i as i32,
            text: chunk.to_string(),
            char_start: offset as i32,
            char_end: (offset + chunk.len()) as i32,
        })
        .collect()
}
