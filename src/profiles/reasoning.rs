//! Which endpoints read a model's earlier reasoning back.
//!
//! The answer is the provider's: Z.ai's documentation asks for
//! `reasoning_content` with the tool results, and without it GLM reasons on a
//! fraction of the rounds. Strict servers refuse a field they do not know, so
//! it goes only to Z.ai's hosts and to a model named `glm-*`, on any host.

/// The field `model` on `base_url` reads its earlier reasoning from, if any.
pub(super) fn thinking_field(base_url: &str, model: &str) -> Option<String> {
    (is_zai_host(base_url) || is_glm(model)).then(|| "reasoning_content".to_owned())
}

fn is_zai_host(base_url: &str) -> bool {
    url::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| {
            host == "api.z.ai" || host == "bigmodel.cn" || host.ends_with(".bigmodel.cn")
        })
}

fn is_glm(model: &str) -> bool {
    model.to_ascii_lowercase().starts_with("glm-")
}

#[cfg(test)]
mod tests {
    use super::thinking_field;

    const FIELD: Option<&str> = Some("reasoning_content");

    #[test]
    fn z_ai_s_hosts_read_it_whatever_the_model() {
        for url in [
            "https://api.z.ai/api/paas/v4",
            "https://API.Z.AI/api/coding/paas/v4",
            "https://open.bigmodel.cn/api/paas/v4",
        ] {
            assert_eq!(thinking_field(url, "some-model").as_deref(), FIELD, "{url}");
        }
    }

    #[test]
    fn a_glm_model_reads_it_on_any_host() {
        for model in ["glm-5.3-flash", "GLM-4.7"] {
            assert_eq!(
                thinking_field("http://127.0.0.1:8080/v1", model).as_deref(),
                FIELD,
                "{model}"
            );
        }
    }

    #[test]
    fn every_other_endpoint_gets_no_field() {
        for (url, model) in [
            ("https://api.openai.com/v1", "gpt-5.2"),
            ("https://api.mistral.ai/v1", "mistral-large"),
            ("https://notbigmodel.cn/v1", "qwen3"),
            ("https://z.ai.example.com/v1", "qwen3"),
            ("not a url", "qwen3"),
        ] {
            assert_eq!(thinking_field(url, model), None, "{url} {model}");
        }
    }
}
