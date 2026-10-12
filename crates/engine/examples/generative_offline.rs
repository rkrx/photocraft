//! Synthetic end-to-end fixture and high/xhigh request pair. Never connects to a provider.
use photocraft_ai::{Backend, Capabilities, Error, Request, Response, VERSION};
use photocraft_engine::{Session, generative_pipeline};
use serde_json::json;
use std::sync::Arc;

struct Synthetic {
    shift: i32,
}
impl Backend for Synthetic {
    fn id(&self) -> &'static str {
        "synthetic-offline"
    }
    fn capabilities(&self, _: &str) -> photocraft_ai::Result<Capabilities> {
        Ok(Capabilities::image2(true))
    }
    fn generate(&self, r: &Request, cancelled: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<Response> {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        let source = generative_pipeline::pixels(&r.source).map_err(|e| Error::Provider(e.to_string()))?;
        let mask = generative_pipeline::pixels(&r.mask).map_err(|e| Error::Provider(e.to_string()))?;
        let (w, h) = (r.source.width, r.source.height);
        let mut out = vec![0; w as usize * h as usize * 4];
        for y in 0..h {
            for x in 0..w {
                let sx = x as i32 - self.shift;
                let i = y as usize * w as usize + x as usize;
                let p = if sx < 0 || sx >= w as i32 {
                    [0.; 4]
                } else {
                    let j = y as usize * w as usize + sx as usize;
                    if mask.get(j).is_some_and(|m| m[3] < 0.5) { [0.72, 0.25, 0.15, 1.] } else { source.get(j).copied().unwrap_or([0.; 4]) }
                };
                if let Some(pixel) = out.get_mut(i * 4..i * 4 + 4) {
                    pixel.copy_from_slice(&p.map(|v| (v * 255.).round() as u8));
                }
            }
        }
        let image = photocraft_codecs::Image::from_u8(w, h, photocraft_codecs::ChannelLayout::Rgba, out).map_err(|e| Error::Provider(e.to_string()))?;
        let bytes = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(|e| Error::Provider(e.to_string()))?;
        Ok(Response { quality: Some(r.options.quality), version: VERSION, image: photocraft_ai::PngImage::from_bytes(w, h, &bytes)? })
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |key: &str| args.iter().position(|x| x == key).and_then(|i| args.get(i + 1));
    let out = std::path::PathBuf::from(arg("--out").map(String::as_str).unwrap_or("log/generative-offline"));
    let operation = arg("--operation").map(String::as_str).unwrap_or("fill");
    let shift = arg("--shift").map(|v| v.parse::<i32>()).transpose()?.unwrap_or(0);
    let mut session = Session::new();
    session.execute("file.new", json!({"width":778,"height":600,"depth":16,"background":"white"}))?;
    session.edit("Synthetic fixture", |doc, _| {
        let mut surface = photocraft_raster::Surface::new(photocraft_color::PixelFormat::new(doc.mode, doc.depth, true));
        for y in 0..600 {
            for x in 0..778 {
                let v = if x == 155 || y == 177 { 0.95 } else { ((x * 73 + y * 97 + x * y * 13) % 251) as f32 / 512. + 0.15 };
                surface.write_pixel(x, y, &[v, v * 0.8, 1. - v, if (120..150).contains(&x) && y < 100 { 0.0 } else { 1.0 }]);
            }
        }
        doc.insert_above(None, photocraft_doc::Layer::new("Synthetic lines", photocraft_doc::LayerContent::Raster(surface)));
        Ok(())
    })?;
    session.execute("select.rect", json!({"x":300,"y":210,"width":140,"height":140}))?;
    session.edit_prefs(|p| p.integrations.image_processing.diagnostics = true);
    session.generative_backend = Some(Arc::new(Synthetic { shift }));
    let (command, params) = match operation {
        "fill" => ("edit.generativeFill", json!({"prompt":"Replace the selected area with a red product","consent":true})),
        "remove" => ("edit.generativeRemove", json!({"prompt":"The selected cable","consent":true})),
        "expand" => ("edit.generativeExpand", json!({"width":1024,"height":640,"anchor":"bottomRight","prompt":"Continue the background","consent":true})),
        _ => return Err("operation must be fill, remove or expand".into()),
    };
    let start = std::time::Instant::now();
    let result = session.execute(command, params)?;
    let generation_ms = start.elapsed().as_millis();
    std::fs::create_dir_all(&out)?;
    let diagnostic = session.execute("ai.diagnostics", json!({}))?;
    std::fs::write(out.join("diagnostics.json"), serde_json::to_vec_pretty(&diagnostic)?)?;
    let comparison = session.execute("ai.comparison", json!({}))?;
    std::fs::write(out.join("comparison-high-xhigh.json"), serde_json::to_vec_pretty(&comparison)?)?;
    let capture: generative_pipeline::Diagnostic = serde_json::from_value(diagnostic)?;
    for (name, image) in &capture.stages {
        std::fs::write(out.join(format!("{name}.png")), image.bytes(photocraft_ai::MAX_IMAGE_BYTES)?)?;
    }
    for (name, image) in [("transmitted-source", &capture.request.source), ("generation-mask", &capture.request.mask)] {
        std::fs::write(out.join(format!("{name}.png")), image.bytes(photocraft_ai::MAX_IMAGE_BYTES)?)?;
    }
    if let Some(response) = &capture.response {
        std::fs::write(out.join("raw-response.png"), response.image.bytes(photocraft_ai::MAX_IMAGE_BYTES)?)?;
    }
    let replay = session.execute("ai.replay", json!({}))?;
    if replay["stages"]["final-result"] != serde_json::to_value(capture.stages.get("final-result"))? {
        return Err("offline replay changed the output".into());
    }
    println!(
        "{}",
        json!({"result":result,"elapsedMs":start.elapsed().as_millis(),"generationMs":generation_ms,"output":out,"networkCalled":false,"replayMatches":true})
    );
    Ok(())
}
#[cfg(target_arch = "wasm32")]
fn main() {}
