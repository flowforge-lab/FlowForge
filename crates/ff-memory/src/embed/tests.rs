use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn off_runtime<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::spawn(f).join().unwrap()
}

#[tokio::test]
async fn embeds_text_from_an_openai_compatible_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{ "embedding": [0.1f32, 0.2, 0.3] }]
        })))
        .mount(&server)
        .await;
    let base = format!("{}/v1", server.uri());
    let got = off_runtime(move || {
        OpenAiEmbedder::new(base, "test-embed", None)
            .embed_query("hello")
            .unwrap()
    });
    assert_eq!(got, Some(vec![0.1, 0.2, 0.3]));
}

#[tokio::test]
async fn non_2xx_falls_back_to_none() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let base = format!("{}/v1", server.uri());
    let got = off_runtime(move || {
        OpenAiEmbedder::new(base, "test-embed", None)
            .embed_chunk("hello")
            .unwrap()
    });
    assert_eq!(got, None);
}

#[tokio::test]
async fn zero_vector_is_treated_as_no_vector() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{ "embedding": [0.0f32, 0.0, 0.0] }]
        })))
        .mount(&server)
        .await;
    let base = format!("{}/v1", server.uri());
    let got = off_runtime(move || {
        OpenAiEmbedder::new(base, "test-embed", None)
            .embed_query("hello")
            .unwrap()
    });
    assert_eq!(got, None);
}

#[test]
fn unreachable_endpoint_is_none_not_error() {
    let got = OpenAiEmbedder::new("http://127.0.0.1:1", "m", None)
        .embed_query("hello")
        .unwrap();
    assert_eq!(got, None);
}

#[test]
fn empty_input_never_hits_the_network() {
    // No server: a network call would error; empty input short-circuits.
    let got = OpenAiEmbedder::new("http://127.0.0.1:1", "m", None)
        .embed_query("   ")
        .unwrap();
    assert_eq!(got, None);
}
