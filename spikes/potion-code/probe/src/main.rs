use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
fn main() -> anyhow::Result<()> {
    let t = std::time::Instant::now();
    let mut m = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::JinaEmbeddingsV2BaseCode).with_show_download_progress(true))?;
    println!("loaded in {:?}", t.elapsed());
    let e = m.embed(vec!["def read_file(path): return open(path).read()", "how to read a file"], None)?;
    println!("dim {}", e[0].len());
    Ok(())
}
