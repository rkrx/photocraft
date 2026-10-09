//! Editor half of image generation: snapshot → wire request → bounded, masked layer.
//! Providers know nothing about documents; replacing one with an IPC client changes no edit logic.
use crate::{EngineError, Result, Session, commands::CommandSpec, jobs::JobCtx};
use photocraft_ai::{Backend, MAX_EDGE, MAX_PIXELS, Operation, PngImage, Request, Response, VERSION};
use photocraft_cms::{Builtin, Intent, Transform};
use photocraft_codecs::{ChannelLayout, DecodeOptions, EncodeOptions, Format, Image, Limits};
use photocraft_color::{PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, LayerContent};
use photocraft_raster::Surface;
use serde_json::{Value, json};
use std::sync::Arc;

pub const FILL: &str = "edit.generativeFill";

fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}
fn enabled(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    photocraft_ai::dimensions(d.doc.size.width, d.doc.size.height).map_err(|e| e.to_string())?;
    if d.doc.selection.as_ref().is_none_or(|mask| mask.content_bounds().intersect(&d.doc.bounds()).is_empty()) {
        return Err("make a selection before Generative Fill".into());
    }
    if !matches!(
        d.doc.mode,
        photocraft_color::ColorMode::Rgb | photocraft_color::ColorMode::Grayscale | photocraft_color::ColorMode::Cmyk | photocraft_color::ColorMode::Lab
    ) {
        return Err("convert to RGB, Gray, CMYK or Lab before Generative Fill".into());
    }
    Ok(())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: FILL,
            label: "Generative Fill…",
            menu: &["Edit"],
            shortcut: None,
            params: r#"{"prompt":str,"model":str=integrations.imageModel,"consent":true} Sends the visible canvas and selection mask to the configured provider; creates one masked layer. Max 2048×2048."#,
            enabled,
            run: fill,
            journal: true,
        },
        CommandSpec {
            id: "ai.status",
            label: "Image Generation Backend",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: crate::commands::always,
            journal: false,
            run: |s, _| {
                Ok(
                    json!({"configured": s.generative_backend.is_some(), "backend": s.generative_backend.as_ref().map(|b| b.id()), "protocol": VERSION, "operations": ["fill"], "maxEdge": MAX_EDGE}),
                )
            },
        },
    ]
}

fn fill(s: &mut Session, p: &Value) -> Result<Value> {
    // Reject unknown fields, including credentials, before any upload or history edit.
    if p.as_object().is_none_or(|fields| fields.keys().any(|key| !matches!(key.as_str(), "prompt" | "model" | "consent"))) {
        return Err(error("Generative Fill accepts only prompt, model and consent; configure credentials through the host"));
    }
    let prompt = p.get("prompt").and_then(Value::as_str).ok_or_else(|| error("prompt must be text"))?.to_string();
    if prompt.trim().is_empty() || prompt.chars().count() > 32_000 {
        return Err(error("prompt must contain 1–32000 characters"));
    }
    let model = match p.get("model") {
        None => s.prefs().integrations.image_model.clone(),
        Some(Value::String(model)) => model.clone(),
        Some(_) => return Err(error("model must be text")),
    };
    if model.is_empty() || model.len() > 128 || !model.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.')) {
        return Err(error("invalid model identifier"));
    }
    if p.get("consent").and_then(Value::as_bool) != Some(true) {
        return Err(error("confirm image upload with consent: true"));
    }
    let backend: Arc<dyn Backend> = s.generative_backend.clone().ok_or_else(|| error("configure an image-generation backend first"))?;
    crate::jobs::edit_job(
        s,
        "Generative Fill",
        move |doc, active, ctx| {
            let request = prepare(doc, prompt, model, ctx)?;
            ctx.check()?;
            ctx.progress(0.2, "Generating image (cancelling discards the result; the provider may still charge)");
            let response = backend.generate(&request, &|| ctx.cancelled()).map_err(|e| match e {
                photocraft_ai::Error::Cancelled => EngineError::Cancelled,
                other => error(other),
            })?;
            ctx.check()?;
            ctx.progress(0.85, "Preparing generated layer");
            let surface = result_surface(doc, &response, ctx)?;
            ctx.check()?;
            let id = doc.insert_above(*active, Layer::new("Generative Fill", LayerContent::Raster(surface)));
            *active = Some(id);
            Ok(id.0)
        },
        |layer| json!({"layer": layer, "operation": "fill"}),
    )
}

fn encode(width: u32, height: u32, pixels: Vec<u8>) -> Result<PngImage> {
    let image = Image::from_u8(width, height, ChannelLayout::Rgba, pixels).map_err(error)?;
    let png = photocraft_codecs::encode(&image, Format::Png, &EncodeOptions::default()).map_err(error)?;
    PngImage::from_bytes(width, height, &png).map_err(error)
}

fn prepare(doc: &Document, prompt: String, model: String, ctx: &JobCtx) -> Result<Request> {
    let (width, height) = (doc.size.width, doc.size.height);
    photocraft_ai::dimensions(width, height).map_err(error)?;
    let selection = doc.selection.as_ref().ok_or_else(|| error("no selection"))?;
    ctx.check()?;
    ctx.progress(0.02, "Preparing image and selection");
    let mut buffer = photocraft_compose::render(doc, doc.bounds());
    let srgb = Builtin::Srgb.profile();
    let transform = Transform::new(&crate::color_cmds::composite_profile(doc), srgb, Intent::RelativeColorimetric, true).map_err(error)?;
    let mut values: Vec<f32> = buffer.px.iter().flat_map(|px| px.iter().copied()).collect();
    transform.apply(&mut values, 4);
    for (pixel, converted) in buffer.px.iter_mut().zip(values.as_chunks::<4>().0) {
        *pixel = *converted;
    }
    ctx.check()?;
    let source = encode(width, height, buffer.to_rgba8().pixels)?;
    let mut mask = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        ctx.check()?;
        for x in 0..width {
            let coverage = selection.sample_channel(x as i32, y as i32, 0);
            let alpha = ((1.0 - finite_coverage(coverage)) * 255.0).round() as u8;
            mask.extend_from_slice(&[255, 255, 255, alpha]);
        }
    }
    let request = Request { version: VERSION, operation: Operation::Fill, prompt, model, source, mask: encode(width, height, mask)? };
    request.validate().map_err(error)?;
    Ok(request)
}

fn finite_coverage(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

fn result_surface(doc: &Document, response: &Response, ctx: &JobCtx) -> Result<Surface> {
    if response.version != VERSION {
        return Err(error("unsupported image response version"));
    }
    let bytes = response.image.bytes(photocraft_ai::MAX_IMAGE_BYTES).map_err(error)?;
    let opts = DecodeOptions {
        limits: Limits { max_width: MAX_EDGE, max_height: MAX_EDGE, max_pixels: MAX_PIXELS, max_alloc: 64 * 1024 * 1024 },
        ..Default::default()
    };
    let image = photocraft_codecs::decode_as_with(Format::Png, &bytes, &opts).map_err(error)?;
    if !image.warnings.is_empty() {
        return Err(error("backend returned an incomplete image"));
    }
    if (image.width(), image.height()) != (response.image.width, response.image.height) {
        return Err(error("response dimensions disagree with PNG"));
    }
    if let Some(icc) = image.icc.as_ref() {
        let profile = photocraft_cms::Profile::parse(icc).map_err(error)?;
        if !profile.same_colors(Builtin::Srgb.profile()) {
            return Err(error("backend must return sRGB PNG pixels"));
        }
    }
    let image = image.convert(ChannelLayout::Rgba, photocraft_codecs::SampleType::F32);
    let values = image.to_normalized();
    let mut generated = Surface::new(PixelFormat::new(photocraft_color::ColorMode::Rgb, SampleType::F32, true));
    let area = photocraft_geom::Rect::new(0, 0, image.width() as i32, image.height() as i32);
    generated.write_region(area, &values);
    let (w, h) = (doc.size.width, doc.size.height);
    // Automatic provider sizes are fitted to the submitted canvas. Refuse aspect changes.
    let input_ratio = f64::from(w) / f64::from(h);
    let output_ratio = f64::from(image.width()) / f64::from(image.height());
    if (input_ratio / output_ratio - 1.0).abs() > 0.03 {
        return Err(error("provider changed the image aspect ratio; result was not applied"));
    }
    if (image.width(), image.height()) != (w, h) {
        generated = photocraft_algo::resample::resize_surface_in_canvas(
            &generated,
            f64::from(w) / f64::from(image.width()),
            f64::from(h) / f64::from(image.height()),
            photocraft_algo::resample::Resample::Bicubic,
            area,
        );
    }
    let rgba = generated.read_region(doc.bounds());
    let fmt = PixelFormat::new(doc.mode, doc.depth, true);
    let channels = fmt.channels();
    let mut native = vec![0.0; w as usize * h as usize * channels];
    let transform = Transform::new(Builtin::Srgb.profile(), &crate::color_cmds::document_profile(doc), Intent::RelativeColorimetric, true).map_err(error)?;
    transform.convert_f32(&rgba, 4, &mut native, channels, true);
    let mask = doc.selection.as_ref().ok_or_else(|| error("no selection"))?;
    // The provider mask is guidance only: enforce the original feathered mask locally.
    for (i, pixel) in native.chunks_exact_mut(channels).enumerate() {
        if i % w as usize == 0 {
            ctx.check()?;
        }
        let coverage = finite_coverage(mask.sample_channel((i % w as usize) as i32, (i / w as usize) as i32, 0));
        if let Some(alpha) = pixel.last_mut() {
            *alpha *= coverage;
        }
    }
    let mut surface = Surface::new(fmt);
    surface.write_region(doc.bounds(), &native);
    Ok(surface)
}

#[cfg(test)]
mod tests;
