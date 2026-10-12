//! Editor half of image generation: snapshot → wire request → bounded, masked layer.
//! Providers know nothing about documents; replacing one with an IPC client changes no edit logic.
use crate::{EngineError, Result, Session, commands::CommandSpec, jobs::JobCtx};
use photocraft_ai::{Backend, MAX_EDGE, Operation, PngImage, Request, Response, VERSION};
use photocraft_cms::{Builtin, Intent, Transform};
use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image};
use photocraft_color::PixelFormat;
use photocraft_doc::{Document, Layer, LayerContent};
use photocraft_raster::Surface;
use serde_json::{Value, json};
use std::sync::Arc;

pub const FILL: &str = "edit.generativeFill";
pub const REMOVE: &str = "edit.generativeRemove";
const REMOVE_PROMPT: &str = "Remove only the unwanted objects inside the indicated selection. The selection is the maximum allowed edit area, not an object silhouette: do not replace everything inside it. Preserve all other visible objects, people, body parts, clothing and scene structure, including those intersecting the selection. Reconstruct only the background hidden by the removed objects, using the surrounding scene as evidence. Continue surfaces, perspective, material boundaries, shadows and lighting naturally. Match the photograph's local colors, contrast, focus and grain; do not sharpen, denoise, restore or beautify it. Merge the reconstructed area invisibly into the surrounding image without a rectangular patch. Keep framing and alignment unchanged, preserve the image outside the selection and return opaque reconstructed content.";
const MAX_REMOVE_DETAIL: usize = 31_000;

fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}
pub(crate) fn document_enabled(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    photocraft_ai::dimensions(d.doc.size.width, d.doc.size.height).map_err(|e| e.to_string())?;
    if !matches!(
        d.doc.mode,
        photocraft_color::ColorMode::Rgb | photocraft_color::ColorMode::Grayscale | photocraft_color::ColorMode::Cmyk | photocraft_color::ColorMode::Lab
    ) {
        return Err("convert to RGB, Gray, CMYK or Lab before generative editing".into());
    }
    Ok(())
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    document_enabled(s)?;
    let d = s.active().ok_or("no document open")?;
    if d.doc.selection.as_ref().is_none_or(|mask| mask.content_bounds().intersect(&d.doc.bounds()).is_empty()) {
        return Err("make a selection before generative editing".into());
    }
    Ok(())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "ai.comparison",
            label: "Prepare Quality Comparison",
            menu: &[],
            shortcut: None,
            enabled: crate::commands::always,
            journal: false,
            params: "{} Return identical high/xhigh requests from the last capture. No network calls; explicitly export to inspect or use externally after authorization.",
            run: |s, _| {
                let guard = s.generative_diagnostic.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let capture = guard.as_ref().ok_or_else(|| error("no diagnostic capture"))?;
                capture.validate()?;
                let caps = photocraft_ai::Capabilities::for_model(&capture.request.model).ok_or_else(|| error("unknown comparison model"))?;
                let mut high = capture.request.clone();
                high.options.quality = photocraft_ai::Quality::High;
                let mut xhigh = high.clone();
                xhigh.options.quality = photocraft_ai::Quality::Xhigh;
                caps.validate(&xhigh.options, xhigh.source.width, xhigh.source.height).map_err(error)?;
                Ok(json!({"high":high,"xhigh":xhigh,"plan":capture.plan,"local":capture.local,"networkCalled":false}))
            },
        },
        CommandSpec {
            id: "ai.diagnostics",
            label: "Generation Diagnostics",
            menu: &[],
            shortcut: None,
            params: "{} Get the last explicitly enabled capture, or null. {clear:true} releases it.",
            enabled: crate::commands::always,
            journal: false,
            run: |s, p| {
                let mut capture = s.generative_diagnostic.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if p.get("clear").and_then(Value::as_bool) == Some(true) {
                    *capture = None;
                }
                serde_json::to_value(&*capture).map_err(error)
            },
        },
        CommandSpec {
            id: "ai.replay",
            label: "Replay Generation Offline",
            menu: &[],
            shortcut: None,
            params: "{capture:Diagnostic} Process a bounded exported capture without networking or document changes; returns new diagnostics. Omit capture to replay the last capture.",
            enabled: crate::commands::always,
            journal: false,
            run: |s, p| {
                let mut capture: crate::generative_pipeline::Diagnostic = if let Some(value) = p.get("capture") {
                    serde_json::from_value(value.clone()).map_err(error)?
                } else {
                    s.generative_diagnostic.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().ok_or_else(|| error("no diagnostic capture"))?
                };
                if let Some(local) = p.get("local") {
                    capture.local = serde_json::from_value(local.clone()).map_err(error)?;
                }
                capture.validate()?;
                let response = capture.response.clone().ok_or_else(|| error("capture contains no provider response"))?;
                capture.local.diagnostics = true;
                let ctx = JobCtx::default();
                let outcome = crate::generative_pipeline::process(&mut capture, response, &ctx);
                capture.error = outcome.as_ref().err().map(ToString::to_string);
                *s.generative_diagnostic.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(capture.clone());
                outcome?;
                serde_json::to_value(capture).map_err(error)
            },
        },
        CommandSpec {
            id: FILL,
            label: "Generative Fill…",
            menu: &["Edit"],
            shortcut: None,
            params: r#"{"prompt":str,"model":str=integrations.imageModel,"consent":true} Sends the visible canvas and selection mask to the configured provider; creates one masked layer. Max 2048×2048."#,
            enabled,
            run: |s, p| edit(s, p, Operation::Fill),
            journal: true,
        },
        CommandSpec {
            id: REMOVE,
            label: "Generative Remove…",
            menu: &["Edit"],
            shortcut: None,
            params: r#"{"prompt":str="","model":str=integrations.imageModel,"consent":true} Optional object description up to 31000 characters. Removes selected objects and reconstructs the background. Sends the visible canvas and selection mask; creates one masked layer. Max 2048×2048."#,
            enabled,
            run: |s, p| edit(s, p, Operation::Remove),
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
                    json!({"configured": s.generative_backend.is_some(), "backend": s.generative_backend.as_ref().map(|b| b.id()), "protocol": VERSION, "operations": ["fill", "remove", "expand"], "maxEdge": MAX_EDGE}),
                )
            },
        },
    ]
}

fn edit(s: &mut Session, p: &Value, operation: Operation) -> Result<Value> {
    // Reject unknown fields, including credentials, before any upload or history edit.
    if p.as_object().is_none_or(|fields| fields.keys().any(|key| !matches!(key.as_str(), "prompt" | "model" | "consent"))) {
        return Err(error("generative editing accepts only prompt, model and consent; configure credentials through the host"));
    }
    let detail = match p.get("prompt") {
        None if operation == Operation::Remove => "",
        Some(Value::String(text)) => text.as_str(),
        _ => return Err(error("prompt must be text")),
    };
    let (label, prompt) = match operation {
        Operation::Fill => {
            if detail.trim().is_empty() || detail.chars().count() > 32_000 {
                return Err(error("prompt must contain 1–32000 characters"));
            }
            ("Generative Fill", detail.to_string())
        }
        Operation::Remove => {
            if detail.chars().count() > MAX_REMOVE_DETAIL {
                return Err(error("removal description must not exceed 31000 characters"));
            }
            let prompt = if detail.trim().is_empty() { REMOVE_PROMPT.to_string() } else { format!("{REMOVE_PROMPT}\nObjects to remove: {}", detail.trim()) };
            ("Generative Remove", prompt)
        }
        Operation::Expand => return Err(error("use the Generative Expand command with a target size")),
    };
    let (model, backend) = options(s, p)?;
    let api = s.prefs().integrations.image_options.clone();
    let local = s.prefs().integrations.image_processing.clone();
    let caps = backend.capabilities(&model).map_err(error)?;
    let store = s.generative_diagnostic.clone();
    *store.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    photocraft_ai::GeometryPlan::new(
        s.active().ok_or(EngineError::NoDocument)?.doc.size.width,
        s.active().ok_or(EngineError::NoDocument)?.doc.size.height,
        &api,
        caps,
    )
    .map_err(error)?;
    let finish_store = store.clone();
    crate::jobs::edit_job(
        s,
        label,
        move |doc, active, ctx| {
            let start = crate::generative_pipeline::Timer::new();
            let mut request = prepare(doc, operation, prompt, model, ctx)?;
            request.options = api;
            let mut capture = crate::generative_pipeline::prepare(request, caps, local, ctx)?;
            let metadata = json!({"quality": capture.request.options.quality, "workingSize": capture.plan.working, "geometry": capture.plan});
            let surface = execute_pipeline(doc, backend.as_ref(), &mut capture, &store, &start, ctx)?;
            ctx.check()?;
            // The source is the full visible composite. Place its replacement above the root
            // stack so upper layers or a hidden/adjusted active group cannot obscure it.
            let id = doc.insert_above(None, Layer::new(label, LayerContent::Raster(surface)));
            *active = Some(id);
            Ok(json!({"layer":id.0,"operation":operation,"used":metadata,"alignment":capture.alignment,"captureId":capture.capture_id}))
        },
        move |result| crate::generative_pipeline::finish(&finish_store, result),
    )
}

pub(crate) fn options(s: &Session, p: &Value) -> Result<(String, Arc<dyn Backend>)> {
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
    Ok((model, backend))
}

pub(crate) fn generate(backend: &dyn Backend, request: &Request, ctx: &JobCtx) -> Result<Response> {
    ctx.check()?;
    ctx.progress(0.2, "Generating image (cancelling discards the result; the provider may still charge)");
    let response = backend.generate(request, &|| ctx.cancelled()).map_err(|e| match e {
        photocraft_ai::Error::Cancelled => EngineError::Cancelled,
        other => error(other),
    })?;
    ctx.check()?;
    Ok(response)
}

pub(crate) fn encode(width: u32, height: u32, pixels: Vec<u8>) -> Result<PngImage> {
    let image = Image::from_u8(width, height, ChannelLayout::Rgba, pixels).map_err(error)?;
    let png = photocraft_codecs::encode(&image, Format::Png, &EncodeOptions::default()).map_err(error)?;
    PngImage::from_bytes(width, height, &png).map_err(error)
}

fn prepare(doc: &Document, operation: Operation, prompt: String, model: String, ctx: &JobCtx) -> Result<Request> {
    let (width, height) = (doc.size.width, doc.size.height);
    photocraft_ai::dimensions(width, height).map_err(error)?;
    let selection = doc.selection.as_ref().ok_or_else(|| error("no selection"))?;
    ctx.check()?;
    ctx.progress(0.02, "Preparing image and selection");
    let source = encode(width, height, visible_pixels(doc, ctx)?)?;
    let mut mask = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        ctx.check()?;
        for x in 0..width {
            let coverage = selection.sample_channel(x as i32, y as i32, 0);
            let alpha = ((1.0 - finite_coverage(coverage)) * 255.0).round() as u8;
            mask.extend_from_slice(&[255, 255, 255, alpha]);
        }
    }
    let request = Request { version: VERSION, operation, prompt, model, options: Default::default(), source, mask: encode(width, height, mask)? };
    request.validate().map_err(error)?;
    Ok(request)
}

pub(crate) fn visible_pixels(doc: &Document, ctx: &JobCtx) -> Result<Vec<u8>> {
    photocraft_ai::dimensions(doc.size.width, doc.size.height).map_err(error)?;
    ctx.check()?;
    transport_pixels(doc, photocraft_compose::render(doc, doc.bounds()), ctx)
}

pub(crate) fn transport_pixels(doc: &Document, mut buffer: photocraft_compose::Buffer, ctx: &JobCtx) -> Result<Vec<u8>> {
    let srgb = Builtin::Srgb.profile();
    let transform = Transform::new(&crate::color_cmds::composite_profile(doc), srgb, Intent::RelativeColorimetric, true).map_err(error)?;
    let mut values: Vec<f32> = buffer.px.iter().flat_map(|px| px.iter().copied()).collect();
    transform.apply(&mut values, 4);
    for (pixel, converted) in buffer.px.iter_mut().zip(values.as_chunks::<4>().0) {
        *pixel = *converted;
    }
    ctx.check()?;
    Ok(buffer.to_rgba8().pixels)
}

fn finite_coverage(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

pub(crate) fn execute_pipeline(
    doc: &Document,
    backend: &dyn Backend,
    capture: &mut crate::generative_pipeline::Diagnostic,
    store: &crate::generative_pipeline::Capture,
    start: &crate::generative_pipeline::Timer,
    ctx: &JobCtx,
) -> Result<Surface> {
    let result = generate(backend, &capture.request, ctx).and_then(|r| crate::generative_pipeline::process(capture, r, ctx)).and_then(|rgba| {
        ctx.progress(0.85, "Preparing generated layer");
        result_surface(doc, &rgba, ctx)
    });
    if capture.local.diagnostics {
        capture.error = result.as_ref().err().map(ToString::to_string);
        capture.elapsed_ms = start.elapsed_ms();
        *store.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(capture.clone());
    }
    result
}
pub(crate) fn result_surface(doc: &Document, rgba: &[f32], ctx: &JobCtx) -> Result<Surface> {
    let (w, h) = (doc.size.width, doc.size.height);
    if rgba.len() != w as usize * h as usize * 4 {
        return Err(error("generated pixel dimensions disagree with document"));
    }
    let fmt = PixelFormat::new(doc.mode, doc.depth, true);
    let channels = fmt.channels();
    let mut native = vec![0.0; w as usize * h as usize * channels];
    let transform = Transform::new(Builtin::Srgb.profile(), &crate::color_cmds::document_profile(doc), Intent::RelativeColorimetric, true).map_err(error)?;
    transform.convert_f32(rgba, 4, &mut native, channels, true);
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
