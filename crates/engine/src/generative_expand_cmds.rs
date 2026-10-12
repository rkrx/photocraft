//! Outpainting: padded visible source, protected original rectangle and one atomic canvas edit.
use crate::{EngineError, Result, Session, commands::CommandSpec, generative_cmds as shared, jobs::JobCtx};
use photocraft_ai::{MAX_EDGE, Operation, Request, VERSION};
use photocraft_doc::{Document, Layer, LayerContent, Size};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::{Value, json};

pub const EXPAND: &str = "edit.generativeExpand";
const PROMPT: &str = "Outpaint only the new margins beyond the original image rectangle. Keep the original image at its exact current size and position; do not stretch, crop, move, reinterpret or change its contents. Treat every edge of the original rectangle as a continuation constraint: continue surfaces, lines, shadows and material boundaries through that edge at the same position and perspective. Continue any existing people, body parts or objects cut off by an image edge coherently into the new margin; do not replace them with unrelated people or abruptly terminate them. Match the original photograph's local colors, exposure, contrast, focus, blur and photographic grain exactly. Do not sharpen, denoise, restore, beautify or modernize the photograph. The new margins must join invisibly without a rectangular frame, brightness step or texture change. Generate complete opaque image content in the new margins.";

fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    shared::document_enabled(s)?;
    let size = s.active().ok_or("no document open")?.doc.size;
    if size.width == MAX_EDGE && size.height == MAX_EDGE {
        return Err("the canvas already reaches the generative image size limit".into());
    }
    Ok(())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: EXPAND,
        label: "Generative Expand…",
        menu: &["Edit"],
        shortcut: None,
        params: r#"{"width":int,"height":int,"anchor":str="center","prompt":str="","model":str=integrations.imageModel,"consent":true} Expands without shrinking or scaling; at least one side must grow, both at most 2048. Anchors: topLeft, top, topRight, left, center, right, bottomLeft, bottom, bottomRight. Optional scene description up to 31000 characters. Sends a padded visible canvas/mask; canvas, content translation and generated margin layer form one undo step."#,
        enabled,
        run: expand,
        journal: true,
    }]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub offset: [i32; 2],
}

/// Shared validation for the dialog and command. No clamping or implicit image resampling.
pub fn geometry(original: Size, p: &Value) -> Result<Geometry> {
    photocraft_ai::dimensions(original.width, original.height).map_err(|e| error(e.to_string()))?;
    let dimension =
        |key: &str| p.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| error(format!("{key} must be a positive integer")));
    let (width, height) = (dimension("width")?, dimension("height")?);
    photocraft_ai::dimensions(width, height).map_err(|e| error(e.to_string()))?;
    if width < original.width || height < original.height || (width, height) == (original.width, original.height) {
        return Err(error("expand at least one side without shrinking the original canvas"));
    }
    let anchor = match p.get("anchor") {
        None => "center",
        Some(Value::String(anchor)) => anchor.as_str(),
        _ => return Err(error("anchor must be text")),
    };
    let (x, y) = match anchor {
        "topLeft" => (0, 0),
        "top" => (1, 0),
        "topRight" => (2, 0),
        "left" => (0, 1),
        "center" => (1, 1),
        "right" => (2, 1),
        "bottomLeft" => (0, 2),
        "bottom" => (1, 2),
        "bottomRight" => (2, 2),
        _ => return Err(error("unknown canvas anchor")),
    };
    // Centre matches Canvas Size's rounded half-offset (the extra pixel goes left/top).
    let offset = |delta: u32, factor| match factor {
        0 => 0,
        2 => delta as i32,
        _ => delta.div_ceil(2) as i32,
    };
    Ok(Geometry { width, height, offset: [offset(width - original.width, x), offset(height - original.height, y)] })
}

/// Smallest exact 16:9 canvas containing the original, with additional area in all cases.
pub fn banner_size(original: Size) -> Option<Size> {
    photocraft_ai::dimensions(original.width, original.height).ok()?;
    let mut k = original.width.div_ceil(16).max(original.height.div_ceil(9));
    if (k * 16, k * 9) == (original.width, original.height) {
        k = k.checked_add(1)?;
    }
    let size = Size::new(k.checked_mul(16)?, k.checked_mul(9)?);
    photocraft_ai::dimensions(size.width, size.height).ok()?;
    Some(size)
}

fn expand(s: &mut Session, p: &Value) -> Result<Value> {
    if p.as_object().is_none_or(|fields| fields.keys().any(|key| !matches!(key.as_str(), "width" | "height" | "anchor" | "prompt" | "model" | "consent"))) {
        return Err(error("Generative Expand accepts only width, height, anchor, prompt, model and consent; configure credentials through the host"));
    }
    let target = geometry(s.active().ok_or(EngineError::NoDocument)?.doc.size, p)?;
    let detail = match p.get("prompt") {
        None => "",
        Some(Value::String(text)) => text.as_str(),
        _ => return Err(error("prompt must be text")),
    };
    if detail.chars().count() > 31_000 {
        return Err(error("scene description must not exceed 31000 characters"));
    }
    let prompt = if detail.trim().is_empty() { PROMPT.to_string() } else { format!("{PROMPT}\nScene to continue: {}", detail.trim()) };
    let (model, backend) = shared::options(s, p)?;
    let api = s.prefs().integrations.image_options.clone();
    let local = s.prefs().integrations.image_processing.clone();
    let caps = backend.capabilities(&model).map_err(|e| error(e.to_string()))?;
    photocraft_ai::GeometryPlan::new(target.width, target.height, &api, caps).map_err(|e| error(e.to_string()))?;
    let store = s.generative_diagnostic.clone();
    *store.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    let finish_store = store.clone();
    crate::jobs::edit_job(
        s,
        "Generative Expand",
        move |doc, active, ctx| {
            let start = crate::generative_pipeline::Timer::new();
            let original_size = doc.size;
            let original_rect = Rect::from_xywh(target.offset[0], target.offset[1], original_size.width, original_size.height);
            ctx.check()?;
            let original = photocraft_compose::render(doc, doc.bounds());
            let mut request = prepare(doc, &original, target, prompt, model, ctx)?;
            request.options = api;
            let mut capture = crate::generative_pipeline::prepare(request, caps, local, ctx)?;
            let metadata = json!({"quality":capture.request.options.quality,"workingSize":capture.plan.working,"geometry":capture.plan});
            // These are changes to the job's private snapshot. No partial canvas edit is published.
            crate::image_cmds::translate_doc(doc, EXPAND, target.offset[0], target.offset[1])?;
            doc.size = Size::new(target.width, target.height);
            crate::canvas_geom::refresh(doc, crate::canvas_geom::Refresh::Shapes);
            ctx.check()?;
            // Canvas-relative fills/effects can change when the canvas grows. Reject that
            // before uploading instead of silently changing the protected original image.
            if photocraft_compose::render(doc, original_rect).px != original.px {
                return Err(error("canvas-relative layers would change the original image; expand a flattened copy instead"));
            }
            let original_selection = doc.selection.take();
            let mut margins = Surface::new(photocraft_color::PixelFormat::GRAY8);
            margins.fill_rect(doc.bounds(), &[1.0]);
            margins.fill_rect(original_rect, &[0.0]);
            doc.selection = Some(margins);
            let surface = shared::execute_pipeline(doc, backend.as_ref(), &mut capture, &store, &start, ctx)?;
            // Incomplete transparent margins would reveal previously off-canvas layer pixels.
            let alpha = surface.format().channels() - 1;
            for y in 0..target.height as i32 {
                ctx.check()?;
                for x in 0..target.width as i32 {
                    if !original_rect.contains(x, y) && surface.sample_channel(x, y, alpha) < 0.999 {
                        let message = "provider returned transparent margins; expanded canvas was not applied";
                        if capture.local.diagnostics {
                            capture.error = Some(message.into());
                            *store.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(capture.clone());
                        }
                        return Err(error(message));
                    }
                }
            }
            ctx.check()?;
            doc.selection = original_selection;
            let id = doc.insert_above(None, Layer::new("Generative Expand", LayerContent::Raster(surface)));
            *active = Some(id);
            Ok(
                json!({"layer": id.0,"operation":Operation::Expand,"width":target.width,"height":target.height,"offset":target.offset,"used":metadata,"alignment":capture.alignment,"captureId":capture.capture_id}),
            )
        },
        move |result| crate::generative_pipeline::finish(&finish_store, result),
    )
}

fn prepare(doc: &Document, original: &photocraft_compose::Buffer, target: Geometry, prompt: String, model: String, ctx: &JobCtx) -> Result<Request> {
    ctx.check()?;
    ctx.progress(0.02, "Preparing image and new canvas margins");
    let original = shared::transport_pixels(doc, original.clone(), ctx)?;
    let old_rect = Rect::from_xywh(target.offset[0], target.offset[1], doc.size.width, doc.size.height);
    let count = target.width as usize * target.height as usize * 4; // geometry is bounded to 2048²
    let mut source = vec![0; count];
    let mut mask = vec![255; count];
    for (index, (pixel, mask_pixel)) in source.as_chunks_mut::<4>().0.iter_mut().zip(mask.as_chunks_mut::<4>().0.iter_mut()).enumerate() {
        let x = (index % target.width as usize) as i32;
        let y = (index / target.width as usize) as i32;
        if x == 0 {
            ctx.check()?;
        }
        if old_rect.contains(x, y) {
            let start = ((y - old_rect.y0) as usize * doc.size.width as usize + (x - old_rect.x0) as usize) * 4;
            let input = original.get(start..start + 4).ok_or_else(|| error("visible source dimensions disagree with pixels"))?;
            pixel.copy_from_slice(input);
        } else if let Some(alpha) = mask_pixel.last_mut() {
            *alpha = 0;
        }
    }
    let request = Request {
        version: VERSION,
        operation: Operation::Expand,
        prompt,
        model,
        options: Default::default(),
        source: shared::encode(target.width, target.height, source)?,
        mask: shared::encode(target.width, target.height, mask)?,
    };
    request.validate().map_err(|e| error(e.to_string()))?;
    Ok(request)
}

#[cfg(test)]
mod tests;
