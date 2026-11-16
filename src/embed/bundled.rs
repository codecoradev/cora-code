//! The code model compiled into the binary (feature `bundled-model`).
//!
//! Release binaries are built with the feature so a fresh download can embed
//! with no network: the first use writes the files into the data directory
//! (`model_store::install_bundled`) and everything after that is the same as for
//! a downloaded model. Without the feature there is nothing bundled and the
//! model is downloaded once instead (`cargo install`, crates.io).

/// One bundled file: its name and bytes.
pub type BundledFile = (&'static str, &'static [u8]);

/// The bundled model files, or `None` when this build does not carry them.
#[cfg(feature = "bundled-model")]
pub fn files() -> Option<&'static [BundledFile]> {
    macro_rules! file {
        ($name:literal) => {
            (
                $name,
                include_bytes!(concat!(env!("CORA_BUNDLED_MODEL_DIR"), "/", $name)) as &[u8],
            )
        };
    }
    static FILES: [BundledFile; 3] = [
        file!("config.json"),
        file!("tokenizer.json"),
        file!("model.safetensors"),
    ];
    Some(&FILES)
}

/// The bundled model files, or `None` when this build does not carry them.
#[cfg(not(feature = "bundled-model"))]
pub fn files() -> Option<&'static [BundledFile]> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_default_build_carries_no_model() {
        assert_eq!(super::files().is_some(), cfg!(feature = "bundled-model"));
    }
}
