//! The default code embedding: `potion-code-16M-v2`, a static (Model2Vec) model.
//!
//! It maps a piece of text to 256 numbers by tokenizing it and averaging the
//! learned vector of each token, so embedding is a table lookup: thousands of
//! symbols per millisecond on a CPU, a few tens of MB of memory, no GPU and no
//! network. Measured on retrieval sets (see `docs/code-intelligence.md`) it finds
//! the right function clearly more often than keyword search or cora's hashing
//! embedding.

use anyhow::{Context, Result};
use model2vec_rs::model::StaticModel;
use std::path::Path;
use std::sync::OnceLock;

/// A loaded static embedding model.
pub struct PotionModel {
    model: StaticModel,
    dim: usize,
}

impl PotionModel {
    /// Load `config.json`, `tokenizer.json` and `model.safetensors` from `dir`.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.to_str().context("model path is not valid UTF-8")?;
        let model = StaticModel::from_pretrained(path, None, None, None)
            .with_context(|| format!("load the embedding model from {}", dir.display()))?;
        let dim = model.dim();
        Ok(Self { model, dim })
    }

    /// Number of dimensions of every embedding.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Embed several texts; rows are unit vectors (or zeros for empty input).
    pub fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        let mut out = self.model.encode(texts);
        for v in &mut out {
            // A text with no known token averages to nothing: keep it finite.
            if v.iter().any(|x| !x.is_finite()) {
                v.iter_mut().for_each(|x| *x = 0.0);
            }
        }
        out
    }

    /// Embed one text.
    pub fn embed(&self, text: &str) -> Vec<f32> {
        self.embed_batch(&[text.to_string()])
            .pop()
            .unwrap_or_else(|| vec![0.0; self.dim])
    }
}

static MODEL: OnceLock<PotionModel> = OnceLock::new();

/// Load the model from `dir` into the process-wide slot (first load wins).
pub fn install(dir: &Path) -> Result<&'static PotionModel> {
    if let Some(m) = MODEL.get() {
        return Ok(m);
    }
    let loaded = PotionModel::load(dir)?;
    Ok(MODEL.get_or_init(|| loaded))
}

/// The loaded model, if one was installed.
pub fn loaded() -> Option<&'static PotionModel> {
    MODEL.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PotionModel {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny-model");
        PotionModel::load(&dir).expect("fixture model loads")
    }

    fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn the_model_reports_its_dimensions_and_unit_vectors() {
        let m = fixture();
        assert_eq!(m.dim(), 8);
        let v = m.embed("read a file from disk");
        assert_eq!(v.len(), 8);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm {norm}");
    }

    #[test]
    fn texts_about_the_same_topic_are_closer_than_texts_about_another() {
        let m = fixture();
        let q = m.embed("open a file from disk");
        let file = m.embed("read_config_file path");
        let json = m.embed("parse_json_text decode");
        assert!(dot(&q, &file) > dot(&q, &json) + 0.3);
    }

    #[test]
    fn batch_and_single_embedding_agree_and_unknown_text_stays_finite() {
        let m = fixture();
        let batch = m.embed_batch(&["send http request".to_string(), "zzz qqq".to_string()]);
        assert_eq!(batch[0], m.embed("send http request"));
        assert!(batch.iter().flatten().all(|x| x.is_finite()));
    }

    #[test]
    fn identifiers_are_split_into_words() {
        // snake_case and camelCase names reach the model as separate words
        let m = fixture();
        let a = m.embed("read_file");
        let b = m.embed("read file");
        assert!(dot(&a, &b) > 0.99, "{}", dot(&a, &b));
    }
}
