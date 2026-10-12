//! File-oriented adapters for the existing engine actions. No separate imaging pipeline.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use photocraft_ai::{Backend, Capabilities, Error, MAX_EDGE, MAX_IMAGE_BYTES, MAX_PIXELS, PngImage, Request, Response, VERSION};
use photocraft_automation::Headless;
use photocraft_codecs::{DecodeOptions, Format, Limits};
use photocraft_engine::generative_pipeline::Diagnostic;
use photocraft_io::ExportOptions;
use serde_json::{Value, json};

use super::{Args, R, print_json, warn_all};

#[derive(Clone, Copy)]
enum Operation {
    Fill,
    Remove,
    Expand,
}
impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Fill => "generative-fill",
            Self::Remove => "generative-remove",
            Self::Expand => "generative-expand",
        }
    }
    fn command(self) -> &'static str {
        match self {
            Self::Fill => "edit.generativeFill",
            Self::Remove => "edit.generativeRemove",
            Self::Expand => "edit.generativeExpand",
        }
    }
}
pub(super) fn fill(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    run(Operation::Fill, a, out, err)
}
pub(super) fn remove(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    run(Operation::Remove, a, out, err)
}
pub(super) fn expand(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    run(Operation::Expand, a, out, err)
}

fn run(operation: Operation, a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    let [input] = a.positional.as_slice() else {
        return Err(format!("{} requires exactly one input image", operation.name()));
    };
    let mask = if matches!(operation, Operation::Expand) { None } else { Some(a.get("--mask").ok_or("--mask is required")?) };
    let prompt = a.get("--prompt").unwrap_or("");
    let mask_mode_name = a.get("--mask-mode").unwrap_or("auto");
    let mask_mode = match mask_mode_name {
        "auto" => photocraft_ai_openai::MaskMode::Auto,
        "api" => photocraft_ai_openai::MaskMode::Api,
        "reference" => photocraft_ai_openai::MaskMode::Reference,
        _ => return Err("--mask-mode must be auto, api or reference".into()),
    };
    if matches!(operation, Operation::Fill) && prompt.trim().is_empty() {
        return Err("generative-fill requires a nonempty --prompt".into());
    }
    let target = Path::new(a.get("--out").ok_or("--out is required")?);
    let mut inputs = vec![Path::new(input)];
    if let Some(mask) = mask {
        inputs.push(Path::new(mask));
    }
    if let Some(response) = a.get("--response") {
        inputs.push(Path::new(response));
    }
    check_output(target, &inputs, a.has("--overwrite"))?;
    if !a.has("--response") && !a.has("--consent") {
        return Err("live generation uploads the image and mask; pass --consent to authorize the API request".into());
    }

    let mut h = Headless::trusted_local();
    if let Some(processing) = a.get("--processing") {
        let value: Value = serde_json::from_str(processing).map_err(|e| format!("--processing: {e}"))?;
        h.command_run("prefs.set", json!({"path":"integrations.imageProcessing","value":value})).map_err(|e| e.to_string())?;
    }
    if let Some(quality) = a.get("--quality") {
        h.command_run("prefs.set", json!({"path":"integrations.imageOptions.quality","value":quality})).map_err(|e| e.to_string())?;
    }
    // Loading a mask as a second document reuses the engine's channel/CMS/selection path.
    for file in std::iter::once(input.as_str()).chain(mask) {
        let opened = h.open(Path::new(file)).map_err(|e| e.to_string())?;
        if let Some(warnings) = opened.get("warnings").and_then(Value::as_array) {
            let warnings: Vec<String> = warnings.iter().filter_map(Value::as_str).map(str::to_owned).collect();
            warn_all(err, &warnings);
        }
    }
    if mask.is_some() {
        h.select(0).map_err(|e| e.to_string())?;
        h.command_run("select.loadSelection", json!({"document":1,"channel":"composite","operation":"new"})).map_err(|e| e.to_string())?;
        h.close(Some(1)).map_err(|e| e.to_string())?;
    }
    let mut params = json!({"prompt":prompt,"consent":true});
    if matches!(operation, Operation::Expand) {
        let original = h.session.active().ok_or("no input document")?.doc.size;
        let size = if let Some(aspect) = a.get("--aspect") {
            if aspect != "16:9" || a.has("--width") || a.has("--height") {
                return Err("--aspect accepts 16:9 and cannot be combined with --width/--height".into());
            }
            let size = photocraft_engine::generative_expand_cmds::banner_size(original).ok_or("no 16:9 expansion fits the size limit")?;
            [size.width, size.height]
        } else {
            [dimension(a, "--width")?, dimension(a, "--height")?]
        };
        params["width"] = json!(size[0]);
        params["height"] = json!(size[1]);
        params["anchor"] = json!(a.get("--anchor").unwrap_or("center"));
        photocraft_engine::generative_expand_cmds::geometry(original, &params).map_err(|e| e.to_string())?;
    }

    h.session.generative_backend = if let Some(path) = a.get("--response") {
        Some(Arc::new(RecordedResponse::read(Path::new(path))?) as Arc<dyn Backend>)
    } else {
        Some(photocraft_ai_openai::from_env_with_mask_mode(mask_mode).map_err(|e| e.to_string())?.ok_or("OPENAI_API_KEY is required for live generation")?)
    };
    let diagnostics = a.get("--diagnostics").map(PathBuf::from);
    if let Some(dir) = &diagnostics {
        fs::create_dir(dir).map_err(|e| format!("cannot create fresh diagnostic directory {}: {e}", dir.display()))?;
        h.command_run("prefs.set", json!({"path":"integrations.imageProcessing.diagnostics","value":true})).map_err(|e| e.to_string())?;
    }
    if let Some(model) = a.get("--model") {
        params["model"] = json!(model);
    }
    let result = h.command_run(operation.command(), params);
    // Capture before propagating a failure: geometry/provider errors are the reason to export.
    let diagnostic_result = if let Some(dir) = &diagnostics { export_diagnostic(&mut h, dir, mask_mode_name, a.has("--response"), operation) } else { Ok(()) };
    if let Err(e) = diagnostic_result {
        if let Err(action) = result {
            return Err(format!("{action}; diagnostic export failed: {e}"));
        }
        return Err(e);
    }
    let action = result.map_err(|e| e.to_string())?;
    let saved = h.save(None, Some(target), None, &ExportOptions::default()).map_err(|e| e.to_string())?;
    print_json(
        out,
        &json!({"operation":operation.name(),"offline":a.has("--response"),"maskMode":mask_mode_name,"action":action,"saved":saved,"diagnostics":diagnostics}),
        false,
    )
}

fn dimension(a: &Args, flag: &str) -> Result<u32, String> {
    a.get(flag)
        .ok_or_else(|| format!("{flag} is required (or use --aspect 16:9)"))?
        .parse::<u32>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| format!("{flag} must be a positive integer"))
}

fn check_output(target: &Path, inputs: &[&Path], overwrite: bool) -> R {
    let name = target.file_name().ok_or("--out must name a file")?;
    let extension =
        target.extension().and_then(|s| s.to_str()).ok_or("--out needs a supported file extension, for example .png or .pcraft")?.to_ascii_lowercase();
    if !matches!(extension.as_str(), "pcraft" | "psd" | "psb") && !photocraft_codecs::from_extension(&extension).is_some_and(|format| format.caps().write) {
        return Err("unsupported output format; choose an export format such as .png or .pcraft".into());
    }
    let parent = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let absolute = fs::canonicalize(parent).map_err(|e| format!("output directory: {e}"))?.join(name);
    let identity = if absolute.exists() { fs::canonicalize(&absolute).map_err(|e| e.to_string())? } else { absolute };
    for input in inputs {
        if fs::canonicalize(input).map_err(|e| format!("input {}: {e}", input.display()))? == identity {
            return Err("output must not replace the input image, mask or recorded response".into());
        }
    }
    if target.is_dir() || (target.exists() && !overwrite) {
        return Err("output already exists; choose a new file or pass --overwrite".into());
    }
    Ok(())
}

fn export_diagnostic(h: &mut Headless, dir: &Path, mode: &str, offline: bool, operation: Operation) -> R {
    let reference = mode == "reference" || (mode == "auto" && matches!(operation, Operation::Remove | Operation::Fill));
    fs::write(
        dir.join("transport.json"),
        serde_json::to_vec_pretty(&json!({"requestedMaskMode":mode,"resolvedMaskMode":if reference {"reference"} else {"api"},"offline":offline}))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let capture = h.command_run("ai.diagnostics", json!({})).map_err(|e| e.to_string())?;
    fs::write(dir.join("capture.json"), serde_json::to_vec_pretty(&capture).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if capture.is_null() {
        return Ok(());
    }
    let diagnostic: Diagnostic = serde_json::from_value(capture).map_err(|e| e.to_string())?;
    write_png(dir, "request-source", &diagnostic.request.source)?;
    write_png(dir, "request-mask", &diagnostic.request.mask)?;
    if reference {
        write_png(dir, "reference-mask", &photocraft_ai_openai::selection_reference(&diagnostic.request.mask).map_err(|e| e.to_string())?)?;
    }
    if let Some(response) = &diagnostic.response {
        write_png(dir, "response", &response.image)?;
    }
    for (name, image) in &diagnostic.stages {
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err("invalid diagnostic stage name".into());
        }
        write_png(dir, name, image)?;
    }
    Ok(())
}

fn write_png(dir: &Path, name: &str, image: &PngImage) -> R {
    fs::write(dir.join(format!("{name}.png")), image.bytes(MAX_IMAGE_BYTES).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

struct RecordedResponse(PngImage);
impl RecordedResponse {
    fn read(path: &Path) -> Result<Self, String> {
        let mut bytes = Vec::new();
        File::open(path).map_err(|e| e.to_string())?.take(MAX_IMAGE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err("recorded response exceeds the image byte limit".into());
        }
        let options = DecodeOptions {
            limits: Limits { max_width: MAX_EDGE, max_height: MAX_EDGE, max_pixels: MAX_PIXELS, max_alloc: 64 * 1024 * 1024 },
            ..Default::default()
        };
        let image = photocraft_codecs::decode_as_with(Format::Png, &bytes, &options).map_err(|e| e.to_string())?;
        Ok(Self(PngImage::from_bytes(image.width(), image.height(), &bytes).map_err(|e| e.to_string())?))
    }
}
impl Backend for RecordedResponse {
    fn id(&self) -> &'static str {
        "recorded-response"
    }
    fn capabilities(&self, model: &str) -> photocraft_ai::Result<Capabilities> {
        Capabilities::for_model(model).ok_or_else(|| Error::Invalid("unknown model capabilities for recorded response".into()))
    }
    fn generate(&self, request: &Request, cancelled: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<Response> {
        request.validate()?;
        if cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(Response { version: VERSION, image: self.0.clone(), quality: None })
    }
}
