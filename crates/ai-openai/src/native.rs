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
    mask_mode: MaskMode,
}
/// Provider transport choice; the editor's insertion mask remains independent and exact.
#[derive(Clone, Copy, Default)]
pub enum MaskMode {
    #[default]
    Auto,
    Api,
    Reference,
}
/// Convert the inverse-alpha wire mask to an opaque black/white selection reference.
pub fn selection_reference(mask: &PngImage) -> Result<PngImage> {
    use photocraft_codecs::{ChannelLayout, DecodeOptions, Format, Image, Limits};
    let opts = DecodeOptions {
        limits: Limits {
            max_width: photocraft_ai::MAX_EDGE,
            max_height: photocraft_ai::MAX_EDGE,
            max_pixels: photocraft_ai::MAX_PIXELS,
            max_alloc: 64 * 1024 * 1024,
        },
        ..Default::default()
    };
    let decoded =
        photocraft_codecs::decode_as_with(Format::Png, &mask.bytes(photocraft_ai::MAX_MASK_BYTES)?, &opts).map_err(|e| Error::Invalid(e.to_string()))?;
    if (decoded.width(), decoded.height()) != (mask.width, mask.height) || !decoded.layout().has_alpha() {
        return Err(Error::Invalid("selection reference requires an alpha mask with matching dimensions".into()));
    }
    let alpha = decoded.layout().channels() - 1;
    let pixels = (0..decoded.height())
        .flat_map(|y| {
            let decoded = &decoded;
            (0..decoded.width()).map(move |x| ((1.0 - decoded.get(x, y, alpha)).clamp(0.0, 1.0) * 255.0).round() as u8)
        })
        .collect();
    let image = Image::from_u8(decoded.width(), decoded.height(), ChannelLayout::Gray, pixels).map_err(|e| Error::Invalid(e.to_string()))?;
    let bytes = photocraft_codecs::encode(&image, Format::Png, &Default::default()).map_err(|e| Error::Invalid(e.to_string()))?;
    PngImage::from_bytes(mask.width, mask.height, &bytes)
}
impl OpenAi {
    pub fn new(api_key: &str) -> Result<Self> {
        let key = api_key.trim();
        if key.is_empty() || key.len() > 1024 {
            return Err(Error::Invalid("enter an OpenAI API key".into()));
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| Error::Invalid("invalid API key".into()))?;
        authorization.set_sensitive(true);
        Ok(Self { authorization, mask_mode: MaskMode::Auto })
    }
    pub fn with_mask_mode(mut self, mode: MaskMode) -> Self {
        self.mask_mode = mode;
        self
    }
    fn reference_mode(&self, request: &Request) -> bool {
        match self.mask_mode {
            MaskMode::Reference => true,
            MaskMode::Api => false,
            MaskMode::Auto => matches!(request.operation, photocraft_ai::Operation::Remove | photocraft_ai::Operation::Fill),
        }
    }
    fn body(&self, request: &Request) -> Result<Value> {
        let mut body = json!({
            "model": request.model, "prompt": request.prompt,
            "images": [{"image_url": request.source.data_url()}],
            "n": 1, "size": format!("{}x{}", request.source.width, request.source.height), "quality": request.options.quality.name(), "output_format": "png"
        });
        if self.reference_mode(request) {
            let reference = selection_reference(&request.mask)?;
            body["images"] = json!([{"image_url":request.source.data_url()},{"image_url":reference.data_url()}]);
            body["prompt"] = json!(format!(
                "Edit the first image. The second image is only a selection guide: white marks the maximum editable area, black marks protected content. Never include the guide in the photograph. Keep framing and alignment unchanged. Match the original image's local colors, contrast, focus, lighting and grain. Return the edited first image only.\n{}",
                request.prompt
            ));
        } else {
            body["mask"] = json!({"image_url":request.mask.data_url()});
        }
        Ok(body)
    }
    fn generate_at(&self, request: &Request, cancelled: &(dyn Fn() -> bool + Sync), endpoint: &str) -> Result<Response> {
        request.validate()?;
        if cancelled() {
            return Err(Error::Cancelled);
        }
        self.capabilities(&request.model)?.validate(&request.options, request.source.width, request.source.height)?;
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
        let body = self.body(request)?;
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
    fn capabilities(&self, model: &str) -> Result<photocraft_ai::Capabilities> {
        photocraft_ai::Capabilities::for_model(model).ok_or_else(|| Error::Invalid("model capabilities are unknown; select a documented image model".into()))
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
    let quality = value
        .get("quality")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v.clone()).map_err(|_| Error::Provider("unsupported reported quality".into())))
        .transpose()?;
    Ok(Response { quality, version: VERSION, image })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    fn request() -> Request {
        let image = PngImage::from_bytes(1024, 1024, b"\x89PNG\r\n\x1a\n").unwrap();
        Request {
            version: VERSION,
            operation: photocraft_ai::Operation::Fill,
            prompt: "flower".into(),
            model: photocraft_ai::DEFAULT_MODEL.into(),
            options: photocraft_ai::ApiOptions::default(),
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
        let backend = OpenAi::new("test-key").unwrap().with_mask_mode(MaskMode::Api);
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aL1sAAAAASUVORK5CYII=";
        let (endpoint, worker) = server("200 OK", json!({"data": [{"b64_json": png}]}).to_string());
        let result = backend.generate_at(&request(), &|| false, &endpoint).unwrap();
        assert_eq!((result.image.width, result.image.height), (1, 1));
        let body = worker.join().unwrap();
        assert_eq!(body["model"], photocraft_ai::DEFAULT_MODEL);
        assert_eq!(body["n"], 1);
        assert_eq!(body["size"], "1024x1024");
        assert_eq!(body["quality"], "high");
        assert!(body.get("input_fidelity").is_none());
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
    fn removal_and_expansion_use_the_same_bounded_image_edit_transport() {
        for operation in [photocraft_ai::Operation::Remove, photocraft_ai::Operation::Expand] {
            let backend = OpenAi::new("test-key").unwrap().with_mask_mode(MaskMode::Api);
            let mut request = request();
            request.operation = operation;
            request.prompt = match operation {
                photocraft_ai::Operation::Remove => "Remove the cable and reconstruct the background.",
                _ => "Continue the image naturally into the new margins.",
            }
            .into();
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
    }
    #[test]
    fn reference_transport_sends_an_opaque_selection_guide_and_no_api_mask() {
        let mut r = request();
        let image = photocraft_codecs::Image::from_u8(1024, 1024, photocraft_codecs::ChannelLayout::Rgba, [255, 255, 255, 0].repeat(1024 * 1024)).unwrap();
        let bytes = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap();
        r.mask = PngImage::from_bytes(1024, 1024, &bytes).unwrap();
        r.operation = photocraft_ai::Operation::Remove;
        let backend = OpenAi::new("test-key").unwrap();
        let response_png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aL1sAAAAASUVORK5CYII=";
        let (endpoint, worker) = server("200 OK", json!({"data":[{"b64_json":response_png}]}).to_string());
        backend.generate_at(&r, &|| false, &endpoint).unwrap();
        let body = worker.join().unwrap();
        assert_eq!(body["images"].as_array().unwrap().len(), 2);
        assert!(body.get("mask").is_none());
        assert!(body["prompt"].as_str().unwrap().contains("selection guide"));
        assert!(body["prompt"].as_str().unwrap().ends_with(&r.prompt));
        assert_eq!(body["images"][0]["image_url"], r.source.data_url());
        let reference = selection_reference(&r.mask).unwrap();
        let reference_image = photocraft_codecs::decode(&reference.bytes(MAX_IMAGE_BYTES).unwrap()).unwrap();
        assert!(!reference_image.layout().has_alpha());
        assert_eq!(reference_image.get(0, 0, 0), 1.0);
        assert_eq!(body["images"][1]["image_url"], reference.data_url());
        assert!(!body.to_string().contains("test-key"));
        for operation in [photocraft_ai::Operation::Fill, photocraft_ai::Operation::Expand] {
            r.operation = operation;
            assert_eq!(backend.body(&r).unwrap().get("mask").is_some(), operation == photocraft_ai::Operation::Expand);
            assert!(OpenAi::new("test-key").unwrap().with_mask_mode(MaskMode::Reference).body(&r).unwrap().get("mask").is_none());
        }
    }
    #[test]
    fn reference_mask_inverts_alpha_including_soft_coverage_and_rejects_bad_pngs() {
        let image =
            photocraft_codecs::Image::from_u8(3, 1, photocraft_codecs::ChannelLayout::Rgba, vec![255, 255, 255, 0, 255, 255, 255, 128, 255, 255, 255, 255])
                .unwrap();
        let bytes = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap();
        let mask = PngImage::from_bytes(3, 1, &bytes).unwrap();
        let result = selection_reference(&mask).unwrap();
        let image = photocraft_codecs::decode(&result.bytes(MAX_IMAGE_BYTES).unwrap()).unwrap();
        assert_eq!(image.data(), &[255, 127, 0]);
        assert!(selection_reference(&PngImage::from_bytes(4, 1, &bytes).unwrap()).is_err());
        assert!(selection_reference(&PngImage::from_bytes(3, 1, b"\x89PNG\r\n\x1a\n").unwrap()).is_err());
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
        let image = PngImage::from_bytes(1024, 1024, b"\x89PNG\r\n\x1a\n").unwrap();
        let request = Request {
            version: VERSION,
            operation: photocraft_ai::Operation::Fill,
            prompt: "flower".into(),
            model: photocraft_ai::DEFAULT_MODEL.into(),
            options: photocraft_ai::ApiOptions::default(),
            source: image.clone(),
            mask: image,
        };
        assert!(matches!(backend.generate(&request, &|| true), Err(Error::Cancelled)));
    }
    #[test]
    fn quality_and_geometry_capabilities_reject_before_network_without_downgrading() {
        let b = OpenAi::new("test-key").unwrap().with_mask_mode(MaskMode::Api);
        let mut r = request();
        r.options.quality = photocraft_ai::Quality::Xhigh;
        r.model = "gpt-image-2".into();
        assert!(b.generate_at(&r, &|| false, "http://127.0.0.1:1").unwrap_err().to_string().contains("quality"));
        r.model = photocraft_ai::DEFAULT_MODEL.into();
        r.source.width = 778;
        r.mask.width = 778;
        assert!(matches!(b.generate_at(&r, &|| false, "http://127.0.0.1:1"), Err(Error::Invalid(_))));
        r = request();
        r.options.quality = photocraft_ai::Quality::Xhigh;
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aL1sAAAAASUVORK5CYII=";
        let (endpoint, worker) = server("200 OK", json!({"data":[{"b64_json":png}],"quality":"xhigh"}).to_string());
        let response = b.generate_at(&r, &|| false, &endpoint).unwrap();
        assert_eq!(response.quality, Some(photocraft_ai::Quality::Xhigh));
        let body = worker.join().unwrap();
        assert_eq!(body["quality"], "xhigh");
        assert_eq!(body["size"], "1024x1024");
        assert!(body.get("seed").is_none());
    }
}
