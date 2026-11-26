//! Short display names for GGUF selections. The saved model ID remains immutable.
use super::hub::{ModelFile, shard};
use std::path::Path;

pub(crate) fn display_name(model: &str) -> Option<String> {
    if let Some(file) = ModelFile::parse(model) {
        let stem = file_stem(&file.name)?;
        return Some(if quant(stem) {
            format!("{} · {stem}", repo_name(&file.repo))
        } else {
            label(stem)
        });
    }
    if let Some(stem) = file_stem(model) {
        return Some(label(stem));
    }
    let (repo, quantization) = model.split_once(':')?;
    (super::hub::valid_repo(repo) && quant(quantization))
        .then(|| format!("{} · {quantization}", repo_name(repo)))
}

fn file_stem(path: &str) -> Option<&str> {
    let name = Path::new(path).file_name()?.to_str()?;
    let stem = shard(name)
        .map(|(base, _, _)| base)
        .or_else(|| name.strip_suffix(".gguf"))?;
    (!stem.is_empty()).then_some(stem)
}

fn label(stem: &str) -> String {
    for (at, _) in stem.rmatch_indices(['-', '.', '_']) {
        let (name, suffix) = stem.split_at(at);
        if !name.is_empty() && quant(&suffix[1..]) {
            return format!("{name} · {}", &suffix[1..]);
        }
    }
    stem.to_owned()
}

fn repo_name(repo: &str) -> &str {
    let name = repo.rsplit('/').next().unwrap_or(repo);
    if let Some((base, suffix)) = name.rsplit_once('-')
        && suffix.eq_ignore_ascii_case("gguf")
    {
        base
    } else {
        name
    }
}

fn quant(value: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    matches!(upper.as_str(), "BF16" | "F16" | "F32" | "MXFP4")
        || ["IQ", "TQ", "Q"].iter().any(|p| {
            upper.strip_prefix(p).is_some_and(|tail| {
                tail.starts_with(|c: char| c.is_ascii_digit())
                    && tail.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_model_labels_keep_the_quantization_and_hide_storage_details() {
        let revision = "a".repeat(40);
        for (source, expected) in [
            (
                format!("hf://example/Coder-GGUF@{revision}/variants/Coder-v1-7A1B-Q6_K.gguf"),
                "Coder-v1-7A1B · Q6_K",
            ),
            (
                format!("hf://example/Coder-GGUF@{revision}/Q6_K.gguf"),
                "Coder · Q6_K",
            ),
            (
                "/models/Example-Q4_K_M-00001-of-00002.gguf".into(),
                "Example · Q4_K_M",
            ),
            ("example/Coder-GGUF:Q8_0".into(), "Coder · Q8_0"),
            ("~/weights/example-v1.0.gguf".into(), "example-v1.0"),
        ] {
            assert_eq!(display_name(&source).as_deref(), Some(expected));
        }
        assert_eq!(display_name("plain-alias"), None);
    }
}
