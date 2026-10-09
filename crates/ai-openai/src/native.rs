use base64::Engine as _;
use photocraft_ai::{Backend, Error, MAX_IMAGE_BYTES, PngImage, Request, Response, Result, VERSION};
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde_json::{Value, json};
use std::io::Read;
use std::time::Duration;

const ENDPOINT: &str = "https://api.openai.com/v1/images/edits";
const MAX_RESPONSE: u64 = 20 * 1024 * 1024;

/// Session-only credentials, never serialized or included in debug/error output.
pub struct OpenAi {
    authorization: HeaderValue,
}
impl OpenAi {
    pub fn new(api_key: &str) -> Result<Self> {
        let key = api_key.trim();
        if key.is_empty() || key.len() > 1024 {
            return Err(Error::Invalid("enter an OpenAI API key".into()));
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| Error::Invalid("invalid API key".into()))?;
        authorization.set_sensitive(true);
        Ok(Self { authorization })
    }
    fn generate_at(&self, request: &Request, cancelled: &(dyn Fn() -> bool + Sync), endpoint: &str) -> Result<Response> {
        request.validate()?;
        let ratio = f64::from(request.source.width) / f64::from(request.source.height);
        if !(1.0 / 3.0..=3.0).contains(&ratio) {
            return Err(Error::Invalid("OpenAI requires an output aspect ratio between 1:3 and 3:1".into()));
        }
        if cancelled() {
            return Err(Error::Cancelled);
        }
        // Blocking HTTP always runs outside an async executor, including CLI/MCP callers.
        std::thread::scope(|scope| {
            let worker = std::thread::Builder::new()
                .name("photocraft-openai".into())
                .spawn_scoped(scope, || self.request_at(request, cancelled, endpoint))
                .map_err(|_| Error::Provider("could not start HTTPS worker".into()))?;
            worker.join().map_err(|_| Error::Provider("HTTPS worker failed; the request may have completed".into()))?
        })
    }
    fn request_at(&self, request: &Request, cancelled: &(dyn Fn() -> bool + Sync), endpoint: &str) -> Result<Response> {
        let client = Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .user_agent("Photocraft-dev")
            .build()
            .map_err(|_| Error::Provider("could not initialize HTTPS".into()))?;
        if cancelled() {
            return Err(Error::Cancelled);
        }
        let body = json!({
            "model": request.model, "prompt": request.prompt,
            "images": [{"image_url": request.source.data_url()}],
            "mask": {"image_url": request.mask.data_url()},
            "n": 1, "size": "auto", "quality": "auto", "output_format": "png"
        });
        let mut response = client
            .post(endpoint)
            .header(AUTHORIZATION, self.authorization.clone())
            .json(&body)
            .send()
            .map_err(|_| Error::Provider("HTTPS request failed or timed out; it may have reached OpenAI".into()))?;
        if cancelled() {
            return Err(Error::Cancelled);
        }
        let status = response.status();
        if !status.is_success() {
            // Do not echo provider bodies: they can contain request text or sensitive values.
            return Err(Error::Provider(format!("OpenAI returned HTTP {}; check key, model access and API billing", status.as_u16())));
        }
        if response.content_length().is_some_and(|n| n > MAX_RESPONSE) {
            return Err(Error::Provider("response exceeds the size limit".into()));
        }
        let mut bytes = Vec::new();
        (&mut response).take(MAX_RESPONSE + 1).read_to_end(&mut bytes).map_err(|_| Error::Provider("could not read image response".into()))?;
        if cancelled() {
            return Err(Error::Cancelled);
        }
        parse_response(&bytes)
    }
}
impl Backend for OpenAi {
    fn id(&self) -> &'static str {
        "openai"
    }
    fn generate(&self, request: &Request, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<Response> {
        self.generate_at(request, cancelled, ENDPOINT)
    }
}

fn parse_response(bytes: &[u8]) -> Result<Response> {
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(Error::Provider("response exceeds the size limit".into()));
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Error::Provider("invalid image response JSON".into()))?;
    let encoded = value
        .get("data")
        .and_then(Value::as_array)
        .filter(|data| data.len() == 1)
        .and_then(|data| data.first())
        .and_then(|item| item.get("b64_json"))
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Provider("response did not contain exactly one encoded image".into()))?;
    if encoded.len() > MAX_IMAGE_BYTES.saturating_add(2) / 3 * 4 {
        return Err(Error::Provider("encoded image exceeds the size limit".into()));
    }
    let png = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| Error::Provider("invalid image base64".into()))?;
    // Only read the PNG header here. Full bounded decoding belongs to the editor.
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") || png.get(12..16) != Some(b"IHDR") {
        return Err(Error::Provider("response is not a PNG image".into()));
    }
    let dimension = |offset: usize| -> Result<u32> {
        let data: [u8; 4] = png.get(offset..offset + 4).and_then(|v| v.try_into().ok()).ok_or_else(|| Error::Provider("truncated PNG header".into()))?;
        Ok(u32::from_be_bytes(data))
    };
    let image = PngImage::from_bytes(dimension(16)?, dimension(20)?, &png)?;
    Ok(Response { version: VERSION, image })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    fn request() -> Request {
        let image = PngImage::from_bytes(1, 1, b"\x89PNG\r\n\x1a\n").unwrap();
        Request {
            version: VERSION,
            operation: photocraft_ai::Operation::Fill,
            prompt: "flower".into(),
            model: photocraft_ai::DEFAULT_MODEL.into(),
            source: image.clone(),
            mask: image,
        }
    }
    fn server(status: &str, body: String) -> (String, std::thread::JoinHandle<Value>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/edits", listener.local_addr().unwrap());
        let status = status.to_string();
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut headers = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse::<usize>().unwrap();
                }
                headers.push_str(&line);
            }
            assert!(headers.starts_with("POST /edits HTTP/1.1"));
            assert!(headers.to_lowercase().contains("authorization: bearer test-key"));
            let mut data = vec![0; length];
            reader.read_exact(&mut data).unwrap();
            write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            serde_json::from_slice(&data).unwrap()
        });
        (endpoint, handle)
    }
    #[test]
    fn http_contract_uses_inline_images_and_does_not_follow_redirects() {
        let backend = OpenAi::new("test-key").unwrap();
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aL1sAAAAASUVORK5CYII=";
        let (endpoint, worker) = server("200 OK", json!({"data": [{"b64_json": png}]}).to_string());
        let result = backend.generate_at(&request(), &|| false, &endpoint).unwrap();
        assert_eq!((result.image.width, result.image.height), (1, 1));
        let body = worker.join().unwrap();
        assert_eq!(body["model"], photocraft_ai::DEFAULT_MODEL);
        assert_eq!(body["n"], 1);
        assert!(body["images"][0]["image_url"].as_str().unwrap().starts_with("data:image/png;base64,"));
        assert_eq!(body["images"][0]["image_url"], body["mask"]["image_url"]);
        assert!(!body.to_string().contains("test-key"));
        let (endpoint, worker) = server("302 Found\r\nLocation: http://127.0.0.1:1/steal", "test-key".into());
        let error = backend.generate_at(&request(), &|| false, &endpoint).unwrap_err().to_string();
        assert!(error.contains("302"));
        assert!(!error.contains("test-key"));
        worker.join().unwrap();
    }
    #[test]
    fn removal_instructions_use_the_same_bounded_image_edit_transport() {
        let backend = OpenAi::new("test-key").unwrap();
        let mut request = request();
        request.operation = photocraft_ai::Operation::Remove;
        request.prompt = "Remove the cable and reconstruct the background.".into();
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aL1sAAAAASUVORK5CYII=";
        let response = serde_json::json!({"data": [{"b64_json": png}]}).to_string();
        let (endpoint, worker) = server("200 OK", response);
        backend.generate_at(&request, &|| false, &endpoint).unwrap();
        let body = worker.join().unwrap();
        assert_eq!(body["prompt"], request.prompt);
        assert_eq!(body["model"], request.model);
        assert_eq!(body["images"][0]["image_url"], request.source.data_url());
        assert_eq!(body["mask"]["image_url"], request.mask.data_url());
        assert!(!body.to_string().contains("test-key"));
    }
    #[test]
    fn malformed_provider_responses_fail() {
        for bytes in [b"no JSON".as_slice(), b"{}", br#"{"data":[{"url":"https://example.org"}]}"#, br#"{"data":[{"b64_json":"?"}]}"#] {
            assert!(parse_response(bytes).is_err());
        }
        assert!(OpenAi::new("").is_err());
        assert!(OpenAi::new("secret\nvalue").is_err());
    }
    #[test]
    fn cancellation_prevents_network() {
        let backend = OpenAi::new("test-key").unwrap();
        let image = PngImage::from_bytes(1, 1, b"\x89PNG\r\n\x1a\n").unwrap();
        let request = Request {
            version: VERSION,
            operation: photocraft_ai::Operation::Fill,
            prompt: "flower".into(),
            model: photocraft_ai::DEFAULT_MODEL.into(),
            source: image.clone(),
            mask: image,
        };
        assert!(matches!(backend.generate(&request, &|| true), Err(Error::Cancelled)));
    }
}
