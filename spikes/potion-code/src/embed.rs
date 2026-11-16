//! cora's own embedding backends, included from the main source tree so the
//! evaluation measures exactly what cora ships (`hashing` and `pretrained`).
#[path = "../../../src/embed/tokens.rs"]
#[allow(dead_code)]
pub mod tokens;

#[path = "../../../src/embed/token_vocab.rs"]
#[allow(dead_code)]
pub mod token_vocab;
