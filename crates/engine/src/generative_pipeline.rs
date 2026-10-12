//! Shared local generation pipeline. Pure bounded pixels; no HTTP, filesystem or editor mutation.
use crate::{EngineError, Result, jobs::JobCtx};
use photocraft_ai::{Capabilities, GeometryPlan, Operation, PngImage, Request, Response, VERSION};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}
/// wasm has no host clock here; never call Instant::now on that target.
pub(crate) struct Timer(#[cfg(not(target_arch = "wasm32"))] std::time::Instant);
impl Timer {
    pub(crate) fn new() -> Self {
        Self(
            #[cfg(not(target_arch = "wasm32"))]
            std::time::Instant::now(),
        )
    }
    pub(crate) fn elapsed_ms(&self) -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.0.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
        }
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalOptions {
    pub check_alignment: bool,
    pub correct_translation: bool,
    /// Integer working-surface pixels, not document pixels. Limited search is ±8 maximum.
    pub max_translation: u32,
    /// Feathering stays INSIDE the authorized mask, measured in document pixels.
    pub transition_width: u32,
    pub match_boundary_color: bool,
    pub diagnostics: bool,
}
impl Default for LocalOptions {
    fn default() -> Self {
        Self { check_alignment: true, correct_translation: true, max_translation: 4, transition_width: 2, match_boundary_color: false, diagnostics: false }
    }
}
impl LocalOptions {
    pub fn validate(&self) -> Result<()> {
        if self.max_translation > 8 || self.transition_width > 32 {
            return Err(error("translation must be 0–8 working pixels; transition must be 0–32 document pixels"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alignment {
    pub status: String,
    pub translation: [i32; 2],
    pub samples: usize,
    pub error: f64,
    /// Variance-weighted, mean-centred RGB correlation; absent in older captures/uncheckable context.
    #[serde(default)]
    pub correlation: Option<f64>,
    pub color_offset: [f32; 3],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    #[serde(default)]
    pub capture_id: u32,
    /// Local processing success is distinct from publishing a layer into the document.
    #[serde(default)]
    pub applied: bool,
    pub request: Request,
    pub plan: GeometryPlan,
    pub local: LocalOptions,
    pub response: Option<Response>,
    pub stages: BTreeMap<String, PngImage>,
    pub alignment: Option<Alignment>,
    pub elapsed_ms: u64,
    /// Protected original rectangle in expanded document pixels (x0,y0,x1,y1 inclusive).
    pub protected_original: Option<[u32; 4]>,
    pub error: Option<String>,
}
pub type Capture = Arc<Mutex<Option<Diagnostic>>>;
impl Diagnostic {
    pub fn retag(&mut self) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        self.capture_id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.applied = false;
    }
    pub const MAX_BYTES: usize = 96 * 1024 * 1024;
    pub fn validate(&self) -> Result<()> {
        if self.error.as_ref().is_some_and(|s| s.len() > 4096)
            || self.alignment.as_ref().is_some_and(|a| {
                a.status.len() > 64
                    || !a.error.is_finite()
                    || a.correlation.is_some_and(|v| !v.is_finite() || !(-1.0..=1.0).contains(&v))
                    || a.color_offset.iter().any(|v| !v.is_finite())
            })
        {
            return Err(error("invalid diagnostic metadata"));
        }
        self.request.validate().map_err(error)?;
        self.local.validate()?;
        photocraft_ai::dimensions(self.plan.document[0], self.plan.document[1]).map_err(error)?;
        if !self.plan.scale.is_finite()
            || !(0.0..=f64::from(photocraft_ai::MAX_EDGE)).contains(&self.plan.scale)
            || self.plan.scale == 0.0
            || self.plan.offset.iter().any(|x| !x.is_finite() || *x < 0.0 || *x > 2048.0)
            || (0..2).any(|axis| self.plan.offset[axis] + f64::from(self.plan.document[axis]) * self.plan.scale > f64::from(self.plan.working[axis]) + 0.00001)
            || self.plan.working != [self.request.source.width, self.request.source.height]
            || self.protected_original.is_some_and(|[x0, y0, x1, y1]| x0 > x1 || y0 > y1 || x1 >= self.plan.document[0] || y1 >= self.plan.document[1])
            || self.stages.len() > 7
        {
            return Err(error("invalid diagnostic geometry or stage count"));
        }
        let mut bytes = self.request.source.base64.len() + self.request.mask.base64.len();
        for (name, image) in &self.stages {
            if !matches!(name.as_str(), "source-document" | "insertion-mask" | "mapped-result" | "final-result") {
                return Err(error("unknown diagnostic stage"));
            }
            if [image.width, image.height] != self.plan.document {
                return Err(error("diagnostic stage dimensions disagree with document geometry"));
            }
            image.bytes(photocraft_ai::MAX_IMAGE_BYTES).map_err(error)?;
            bytes = bytes.saturating_add(image.base64.len());
        }
        if let Some(r) = &self.response {
            r.image.bytes(photocraft_ai::MAX_IMAGE_BYTES).map_err(error)?;
            bytes = bytes.saturating_add(r.image.base64.len());
        }
        if bytes > Self::MAX_BYTES {
            return Err(error("diagnostic capture exceeds 96 MiB limit"));
        }
        Ok(())
    }
}
/// Safe PNG decode, including real dimensions and sRGB profile validation for every replay path.
pub fn pixels(image: &PngImage) -> Result<Vec<[f32; 4]>> {
    use photocraft_codecs::{ChannelLayout, DecodeOptions, Format, Limits, SampleType};
    let bytes = image.bytes(photocraft_ai::MAX_IMAGE_BYTES).map_err(error)?;
    let opts = DecodeOptions {
        limits: Limits {
            max_width: photocraft_ai::MAX_EDGE,
            max_height: photocraft_ai::MAX_EDGE,
            max_pixels: photocraft_ai::MAX_PIXELS,
            max_alloc: 64 * 1024 * 1024,
        },
        ..Default::default()
    };
    let decoded = photocraft_codecs::decode_as_with(Format::Png, &bytes, &opts).map_err(error)?;
    if !decoded.warnings.is_empty() || (decoded.width(), decoded.height()) != (image.width, image.height) {
        return Err(error("incomplete PNG or declared dimensions disagree with pixels"));
    }
    if let Some(icc) = &decoded.icc {
        let profile = photocraft_cms::Profile::parse(icc).map_err(error)?;
        if !profile.same_colors(photocraft_cms::Builtin::Srgb.profile()) {
            return Err(error("backend must return sRGB PNG pixels"));
        }
    }
    Ok(decoded.convert(ChannelLayout::Rgba, SampleType::F32).to_normalized().as_chunks::<4>().0.to_vec())
}
fn png(w: u32, h: u32, px: &[[f32; 4]]) -> Result<PngImage> {
    crate::generative_cmds::encode(w, h, px.iter().flat_map(|p| p.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)).collect())
}
/// Bilinear interpolation of premultiplied colour; unpremultiply once, so transparent colour cannot bleed.
fn sample(px: &[[f32; 4]], w: u32, h: u32, x: f64, y: f64) -> [f32; 4] {
    if !x.is_finite() || !y.is_finite() || x < -0.5 || y < -0.5 || x > f64::from(w) - 0.5 || y > f64::from(h) - 0.5 {
        return [0.0; 4];
    }
    let x = x.clamp(0.0, f64::from(w) - 1.0);
    let y = y.clamp(0.0, f64::from(h) - 1.0);
    let (ix, iy) = (x.floor() as u32, y.floor() as u32);
    let (fx, fy) = ((x - f64::from(ix)) as f32, (y - f64::from(iy)) as f32);
    let mut out = [0.; 4];
    for (sx, sy, weight) in [
        (ix, iy, (1. - fx) * (1. - fy)),
        ((ix + 1).min(w - 1), iy, fx * (1. - fy)),
        (ix, (iy + 1).min(h - 1), (1. - fx) * fy),
        ((ix + 1).min(w - 1), (iy + 1).min(h - 1), fx * fy),
    ] {
        if let Some(p) = px.get(sy as usize * w as usize + sx as usize) {
            for (v, c) in out.iter_mut().take(3).zip(p.iter().take(3)) {
                *v += c * p[3] * weight;
            }
            out[3] += p[3] * weight;
        }
    }
    if out[3] > 0. {
        let a = out[3];
        for c in out.iter_mut().take(3) {
            *c /= a;
        }
    }
    out
}
/// Image and mask share this mapping exactly. Padding is protected and excluded from alignment.
pub fn prepare(request: Request, caps: Capabilities, local: LocalOptions, ctx: &JobCtx) -> Result<Diagnostic> {
    request.validate().map_err(error)?;
    let Request { source, mask, operation, prompt, model, options: api, .. } = request;
    local.validate()?;
    if (source.width, source.height) != (mask.width, mask.height) {
        return Err(error("source and insertion mask must have identical dimensions"));
    }
    let plan = GeometryPlan::new(source.width, source.height, &api, caps).map_err(error)?;
    let input = pixels(&source)?;
    let mask_px = pixels(&mask)?;
    let [w, h] = plan.working;
    let mut work = Vec::with_capacity(w as usize * h as usize);
    let mut generation = Vec::with_capacity(work.capacity());
    for y in 0..h {
        ctx.check()?;
        for x in 0..w {
            let q = plan.to_document(f64::from(x), f64::from(y));
            // Small rounding padding extends edge samples, rather than adding transparent
            // framing that invites the model to recompose the photograph. Transparent pixels
            // inside the document retain their alpha through premultiplied sampling.
            work.push(sample(
                &input,
                source.width,
                source.height,
                q[0].clamp(0.0, f64::from(source.width) - 1.0),
                q[1].clamp(0.0, f64::from(source.height) - 1.0),
            ));
            let alpha = if q[0] < -0.5 || q[1] < -0.5 || q[0] >= f64::from(source.width) - 0.5 || q[1] >= f64::from(source.height) - 0.5 {
                1.0
            } else {
                sample(&mask_px, mask.width, mask.height, q[0], q[1])[3]
            };
            generation.push([1., 1., 1., alpha]);
        }
    }
    let request = Request { version: VERSION, operation, prompt, model, options: api, source: png(w, h, &work)?, mask: png(w, h, &generation)? };
    request.validate().map_err(error)?;
    let protected_original = if operation == Operation::Expand {
        let mut bounds = [source.width, source.height, 0, 0];
        let mut found = false;
        for (i, p) in mask_px.iter().enumerate() {
            if p[3] >= 0.999 {
                let x = (i % source.width as usize) as u32;
                let y = (i / source.width as usize) as u32;
                bounds = [bounds[0].min(x), bounds[1].min(y), bounds[2].max(x), bounds[3].max(y)];
                found = true;
            }
        }
        found.then_some(bounds)
    } else {
        None
    };
    let stages = BTreeMap::from([("source-document".into(), source), ("insertion-mask".into(), mask)]);
    let mut capture = Diagnostic {
        capture_id: 0,
        applied: false,
        request,
        plan,
        local,
        response: None,
        stages,
        alignment: None,
        elapsed_ms: 0,
        protected_original,
        error: None,
    };
    capture.retag();
    Ok(capture)
}
/// Low-frequency, spatially varying colour residual measured only in protected context.
/// Coarse cells keep memory/work bounded; interpolation preserves a smooth correction field.
fn boundary_colors(
    source: &[[f32; 4]],
    insertion: &[[f32; 4]],
    result: &[[f32; 4]],
    plan: GeometryPlan,
    shift: [i32; 2],
    ctx: &JobCtx,
) -> Result<Vec<[f32; 4]>> {
    const CELL: u32 = 8;
    let [w, h] = plan.document;
    let (gw, gh) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let len = gw as usize * gh as usize;
    let mut sums = vec![[0.; 3]; len];
    let mut counts = vec![0_u32; len];
    for y in (0..h).step_by(4) {
        ctx.check()?;
        for x in (0..w).step_by(4) {
            let i = y as usize * w as usize + x as usize;
            let Some(baseline) = source.get(i) else { continue };
            if baseline[3] < 0.99 || insertion.get(i).is_none_or(|m| m[3] < 0.999) {
                continue;
            }
            let q = plan.to_working(f64::from(x), f64::from(y));
            let generated = sample(result, plan.working[0], plan.working[1], q[0] + f64::from(shift[0]), q[1] + f64::from(shift[1]));
            if generated[3] < 0.99 {
                continue;
            }
            let cell = (y / CELL) as usize * gw as usize + (x / CELL) as usize;
            if let (Some(sum), Some(count)) = (sums.get_mut(cell), counts.get_mut(cell)) {
                for c in 0..3 {
                    sum[c] += (baseline[c] - generated[c]).clamp(-0.12, 0.12);
                }
                *count += 1;
            }
        }
    }
    let total = counts.iter().map(|n| u64::from(*n)).sum::<u64>().max(1) as f32;
    let mean = std::array::from_fn::<_, 3, _>(|c| sums.iter().map(|s| s[c]).sum::<f32>() / total);
    let mut field: Vec<[f32; 4]> = sums
        .iter()
        .zip(&counts)
        .map(|(s, n)| {
            let v = if *n > 0 { s.map(|v| v / *n as f32) } else { mean };
            [v[0], v[1], v[2], 1.]
        })
        .collect();
    let mut next = field.clone();
    // The correction influences only 32 native pixels (four cells) inside the boundary.
    // Fixed diffusion passes do not scale with image area or depend on API latency.
    for _ in 0..64 {
        ctx.check()?;
        for y in 0..gh {
            for x in 0..gw {
                let i = y as usize * gw as usize + x as usize;
                if counts.get(i).is_some_and(|n| *n > 0) {
                    continue;
                }
                let neighbours = [(x.saturating_sub(1), y), ((x + 1).min(gw - 1), y), (x, y.saturating_sub(1)), (x, (y + 1).min(gh - 1))];
                let mut value = [0.; 4];
                value[3] = 1.;
                for (nx, ny) in neighbours {
                    if let Some(p) = field.get(ny as usize * gw as usize + nx as usize) {
                        for c in 0..3 {
                            value[c] += p[c] * 0.25;
                        }
                    }
                }
                if let Some(dst) = next.get_mut(i) {
                    *dst = value;
                }
            }
        }
        std::mem::swap(&mut field, &mut next);
    }
    Ok(field)
}

fn align(source: &[[f32; 4]], result: &[[f32; 4]], mask: &[[f32; 4]], plan: GeometryPlan, local: &LocalOptions, ctx: &JobCtx) -> Result<Alignment> {
    let mut report = Alignment { status: "disabled".into(), translation: [0, 0], samples: 0, error: 0.0, correlation: None, color_offset: [0.; 3] };
    if !local.check_alignment {
        return Ok(report);
    }
    let [w, h] = plan.working;
    let radius = local.max_translation as i32;
    let step = ((w as f64 * h as f64 / 4096.).sqrt().ceil() as usize).max(1);
    let inverse: Vec<[f32; 4]> = mask.iter().map(|p| [1., 1., 1., 1. - p[3]]).collect();
    let boundary = distance(&inverse, w, h, 33);
    let mut points = Vec::new();
    let mut sum = [0.; 3];
    let mut square = [0.; 3];
    for y in (radius..h as i32 - radius).step_by(step) {
        for x in (radius..w as i32 - radius).step_by(step) {
            let q = plan.to_document(x as f64, y as f64);
            if q[0] < 0. || q[1] < 0. || q[0] >= f64::from(plan.document[0]) - 1. || q[1] >= f64::from(plan.document[1]) - 1. {
                continue;
            }
            let i = y as usize * w as usize + x as usize;
            let Some(p) = source.get(i) else { continue };
            // Guard the complete search neighbourhood against crossing editable regions.
            let preserved = (-radius..=radius)
                .all(|dy| (-radius..=radius).all(|dx| mask.get((y + dy) as usize * w as usize + (x + dx) as usize).is_some_and(|p| p[3] >= 0.999)));
            if p[3] < 0.99 || !preserved {
                continue;
            }
            for c in 0..3 {
                let value = f64::from(p[c]);
                sum[c] += value;
                square[c] += value * value;
            }
            points.push((x, y, *p));
        }
    }
    report.samples = points.len();
    let n = points.len().max(1) as f64;
    let variance = (0..3).map(|c| (square[c] / n - (sum[c] / n).powi(2)).max(0.)).sum::<f64>() / 3.;
    if points.len() < 64 || variance < 0.0004 {
        report.status = "insufficient-context".into();
        return Ok(report);
    }
    let mut scores = Vec::new();
    for dy in -radius..=radius {
        ctx.check()?;
        for dx in -radius..=radius {
            let mut offset = [0.; 3];
            let (mut ps, mut qs, mut pp, mut qq, mut pq) = ([0.; 3], [0.; 3], [0.; 3], [0.; 3], [0.; 3]);
            let mut count = 0.0_f64;
            for (x, y, p) in &points {
                if let Some(q) = result.get((y + dy) as usize * w as usize + (x + dx) as usize)
                    && q[3] >= 0.99
                {
                    for c in 0..3 {
                        offset[c] += p[c] - q[c];
                        let (p, q) = (f64::from(p[c]), f64::from(q[c]));
                        ps[c] += p;
                        qs[c] += q;
                        pp[c] += p * p;
                        qq[c] += q * q;
                        pq[c] += p * q;
                    }
                    count += 1.;
                }
            }
            if count < n * 0.9 {
                continue;
            }
            for c in &mut offset {
                *c /= count as f32;
            }
            // Pixel differences cannot distinguish geometry from changed exposure/contrast.
            // Centre each channel and normalize its covariance; affine positive tonal changes
            // keep the same spatial correspondence. Ignore constant source channels.
            let mut weighted = 0.0;
            let mut weight = 0.0;
            for c in 0..3 {
                let vp = (pp[c] - ps[c] * ps[c] / count).max(0.0);
                let vq = (qq[c] - qs[c] * qs[c] / count).max(0.0);
                if vp > 1e-10 {
                    let correlation = if vq > 1e-10 { ((pq[c] - ps[c] * qs[c] / count) / (vp * vq).sqrt()).clamp(-1.0, 1.0) } else { 0.0 };
                    weighted += vp * correlation;
                    weight += vp;
                }
            }
            if weight <= 1e-10 {
                continue;
            }
            let mut mae = 0.;
            for (x, y, p) in &points {
                if let Some(q) = result.get((y + dy) as usize * w as usize + (x + dx) as usize)
                    && q[3] >= 0.99
                {
                    for c in 0..3 {
                        mae += f64::from((p[c] - q[c] - offset[c]).abs());
                    }
                }
            }
            scores.push((1.0 - weighted / weight, [dx, dy], offset, mae / (count * 3.)));
        }
    }
    scores.sort_by(|a, b| a.0.total_cmp(&b.0));
    let Some(best) = scores.first() else {
        report.status = "insufficient-context".into();
        return Ok(report);
    };
    report.error = best.3;
    report.correlation = Some(1.0 - best.0);
    let identity = scores.iter().find(|v| v.1 == [0, 0]);
    let unique = scores.get(1).is_some_and(|second| second.0 > best.0 + 0.05);
    if best.0 <= 0.1 && unique && best.1 != [0, 0] {
        report.status = if local.correct_translation { "corrected-translation" } else { "translation-detected" }.into();
        report.translation = best.1;
        report.color_offset = best.2;
    } else if identity.is_some_and(|v| v.0 <= 0.1) {
        report.status = "plausible".into();
        if let Some(identity) = identity {
            report.error = identity.3;
            report.correlation = Some(1.0 - identity.0);
        }
    } else if best.0 > 0.65 {
        report.status = "geometric-deviation".into();
    } else {
        report.status = "insufficient-context".into();
    }
    let mut offset = [0.; 3];
    let mut ring_count = 0;
    for (x, y, p) in &points {
        let i = *y as usize * w as usize + *x as usize;
        if boundary.get(i).is_none_or(|d| *d as f64 > 32. * plan.scale) {
            continue;
        }
        let [dx, dy] = report.translation;
        if let Some(q) = result.get((y + dy) as usize * w as usize + (x + dx) as usize)
            && q[3] >= 0.99
        {
            for c in 0..3 {
                offset[c] += p[c] - q[c];
            }
            ring_count += 1;
        }
    }
    report.color_offset = if ring_count >= 32 { offset.map(|v| v / ring_count as f32) } else { [0.; 3] };
    Ok(report)
}
/// Distance from a hard authorized boundary in document pixels. O(pixels), capped at 32.
fn distance(mask: &[[f32; 4]], w: u32, h: u32, limit: u32) -> Vec<u32> {
    let mut d: Vec<u32> = mask.iter().map(|p| if p[3] >= 0.999 { 0 } else { limit }).collect();
    for y in 0..h {
        for x in 0..w {
            let i = y as usize * w as usize + x as usize;
            let mut v = d.get(i).copied().unwrap_or(0);
            for j in [x.checked_sub(1).map(|x| y as usize * w as usize + x as usize), y.checked_sub(1).map(|y| y as usize * w as usize + x as usize)]
                .into_iter()
                .flatten()
            {
                v = v.min(d.get(j).copied().unwrap_or(0).saturating_add(1));
            }
            if let Some(p) = d.get_mut(i) {
                *p = v;
            }
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y as usize * w as usize + x as usize;
            let mut v = d.get(i).copied().unwrap_or(0);
            for j in [(x + 1 < w).then_some(y as usize * w as usize + (x + 1) as usize), (y + 1 < h).then_some((y + 1) as usize * w as usize + x as usize)]
                .into_iter()
                .flatten()
            {
                v = v.min(d.get(j).copied().unwrap_or(0).saturating_add(1));
            }
            if let Some(p) = d.get_mut(i) {
                *p = v;
            }
        }
    }
    d
}
/// Return straight sRGB RGBA in document geometry. The native selection is enforced later at native depth.
pub fn process(capture: &mut Diagnostic, response: Response, ctx: &JobCtx) -> Result<Vec<f32>> {
    capture.validate()?;
    capture.retag();
    capture.alignment = None;
    capture.error = None;
    capture.stages.remove("mapped-result");
    capture.stages.remove("final-result");
    response.image.bytes(photocraft_ai::MAX_IMAGE_BYTES).map_err(error)?;
    capture.response = Some(response.clone());
    if response.version != VERSION || [response.image.width, response.image.height] != capture.plan.working {
        return Err(error("provider returned an unexpected size; result was not stretched or applied"));
    }
    if response.quality.is_some_and(|q| capture.request.options.quality != photocraft_ai::Quality::Auto && q != capture.request.options.quality) {
        return Err(error("provider reported a different quality; no silent downgrade was applied"));
    }
    let generated = pixels(&response.image)?;
    let original = pixels(&capture.request.source)?;
    let mask = pixels(&capture.request.mask)?;
    let alignment = align(&original, &generated, &mask, capture.plan, &capture.local, ctx)?;
    capture.alignment = Some(alignment.clone());
    if alignment.status == "geometric-deviation" {
        return Err(error(format!(
            "generated preserved context differs substantially from the source (alignment correlation {:.2}); result was not applied",
            alignment.correlation.unwrap_or(0.0)
        )));
    }
    if alignment.status == "translation-detected" {
        return Err(error("generated context is shifted; enable automatic translation correction in AI settings or inspect the diagnostic capture"));
    }
    let [w, h] = capture.plan.document;
    let [ww, wh] = capture.plan.working;
    let shift = if alignment.status == "corrected-translation" { alignment.translation } else { [0, 0] };
    let source = pixels(capture.stages.get("source-document").ok_or_else(|| error("diagnostic source is missing"))?)?;
    let insertion = pixels(capture.stages.get("insertion-mask").ok_or_else(|| error("insertion mask is missing"))?)?;
    if source.len() != w as usize * h as usize || insertion.len() != source.len() {
        return Err(error("diagnostic document geometry mismatch"));
    }
    // Colour matching has its own 32-pixel falloff, independent of the feather width.
    let distance_limit = if capture.local.match_boundary_color { capture.local.transition_width.max(32) } else { capture.local.transition_width };
    let distances = distance(&insertion, w, h, distance_limit);
    let color_field = if capture.local.match_boundary_color
        && matches!(alignment.status.as_str(), "plausible" | "corrected-translation")
        && alignment.samples >= 64
        && alignment.color_offset.iter().all(|v| v.abs() <= 0.12)
    {
        Some(boundary_colors(&source, &insertion, &generated, capture.plan, shift, ctx)?)
    } else {
        None
    };
    let protected = if capture.request.operation == Operation::Expand {
        let mut bounds = [w, h, 0, 0];
        let mut found = false;
        for (i, p) in insertion.iter().enumerate() {
            if p[3] >= 0.999 {
                let x = (i % w as usize) as u32;
                let y = (i / w as usize) as u32;
                bounds = [bounds[0].min(x), bounds[1].min(y), bounds[2].max(x), bounds[3].max(y)];
                found = true;
            }
        }
        found.then_some(bounds)
    } else {
        None
    };
    let mut mapped = Vec::with_capacity(source.len());
    let mut final_pixels = Vec::with_capacity(source.len());
    for y in 0..h {
        ctx.check()?;
        for x in 0..w {
            let i = y as usize * w as usize + x as usize;
            let q = capture.plan.to_working(x as f64, y as f64);
            let sx = q[0] + f64::from(shift[0]);
            let sy = q[1] + f64::from(shift[1]);
            let editable = insertion.get(i).map(|p| 1. - p[3]).unwrap_or(0.);
            if editable > 0. && (sx < -0.5 || sy < -0.5 || sx > f64::from(ww) - 0.5 || sy > f64::from(wh) - 0.5) {
                return Err(error("translation would leave editable pixels without image coverage"));
            }
            let mut pixel = sample(&generated, ww, wh, sx, sy);
            let baseline = source.get(i).copied().unwrap_or([0.; 4]);
            let d = distances.get(i).copied().unwrap_or(0);
            // Optional bounded colour offset, only near the boundary and only for a reliable match.
            if let Some(field) = &color_field {
                let offset = sample(field, w.div_ceil(8), h.div_ceil(8), (f64::from(x) + 0.5) / 8. - 0.5, (f64::from(y) + 0.5) / 8. - 0.5);
                let weight = 1. - (d as f32 / 32.).min(1.);
                for (c, value) in pixel.iter_mut().take(3).enumerate() {
                    *value = (*value + offset[c] * weight).clamp(0., 1.);
                }
            }
            // Hard selections blend inside their boundary; existing soft selection alpha is not feathered twice.
            // Expansion uses opaque generated margins without fading them into transparent padding.
            if capture.request.operation != Operation::Expand && editable >= 0.999 && capture.local.transition_width > 0 && baseline[3] >= 0.999 {
                let weight = (d as f32 / capture.local.transition_width as f32).min(1.);
                for c in 0..3 {
                    pixel[c] = baseline[c] * (1. - weight) + pixel[c] * weight;
                }
            }
            if let Some([x0, y0, x1, y1]) = protected
                && (x < x0 || x > x1 || y < y0 || y > y1)
                && editable >= 0.999
                && capture.local.transition_width > 0
                && d < capture.local.transition_width
            {
                let bx = x.clamp(x0, x1);
                let by = y.clamp(y0, y1);
                let edge = source.get(by as usize * w as usize + bx as usize).copied().unwrap_or([0.; 4]);
                let ix = if x < x0 {
                    (bx + 1).min(x1)
                } else if x > x1 {
                    bx.saturating_sub(1).max(x0)
                } else {
                    bx
                };
                let iy = if y < y0 {
                    (by + 1).min(y1)
                } else if y > y1 {
                    by.saturating_sub(1).max(y0)
                } else {
                    by
                };
                let inner = source.get(iy as usize * w as usize + ix as usize).copied().unwrap_or([0.; 4]);
                if edge[3] >= 0.999 && inner[3] >= 0.999 && (0..3).all(|c| (edge[c] - inner[c]).abs() < 0.01) {
                    let weight = d as f32 / capture.local.transition_width as f32;
                    for c in 0..3 {
                        pixel[c] = edge[c] * (1. - weight) + pixel[c] * weight;
                    }
                }
            }
            mapped.push(pixel);
            let a = pixel[3] * editable;
            let out_a = a + baseline[3] * (1. - a);
            let mut composed = [0.; 4];
            composed[3] = out_a;
            if out_a > 0. {
                for c in 0..3 {
                    composed[c] = (pixel[c] * a + baseline[c] * baseline[3] * (1. - a)) / out_a;
                }
            }
            // Exact protection, including RGB under fully transparent pixels in diagnostic output.
            final_pixels.push(if editable == 0. { baseline } else { composed });
        }
    }
    if capture.local.diagnostics {
        capture.stages.insert("mapped-result".into(), png(w, h, &mapped)?);
        capture.stages.insert("final-result".into(), png(w, h, &final_pixels)?);
        if let Err(e) = capture.validate() {
            capture.stages.remove("mapped-result");
            capture.stages.remove("final-result");
            return Err(e);
        }
    }
    Ok(mapped.iter().flat_map(|p| p.iter().copied()).collect())
}

/// Called only after the job's document/revision guard accepts its edit.
pub(crate) fn finish(store: &Capture, result: serde_json::Value) -> serde_json::Value {
    if let Some(capture) = store.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut()
        && result.get("captureId").and_then(serde_json::Value::as_u64) == Some(u64::from(capture.capture_id))
    {
        capture.applied = true;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_ai::ApiOptions;
    fn patterned(w: u32, h: u32) -> Vec<[f32; 4]> {
        (0..w * h)
            .map(|i| {
                let x = i % w;
                let y = i / w;
                let v = ((x * 73 + y * 97 + x * y * 13) % 251) as f32 / 255.;
                [v, v * 0.8, 1. - v, 1.]
            })
            .collect()
    }
    fn capture(w: u32, h: u32, source: Vec<[f32; 4]>, soft: bool, caps: Capabilities) -> Diagnostic {
        let mut mask = vec![[1., 1., 1., 1.]; w as usize * h as usize];
        for y in h / 3..2 * h / 3 {
            for x in w / 3..2 * w / 3 {
                mask[y as usize * w as usize + x as usize][3] = if soft { 0.5 } else { 0. };
            }
        }
        prepare(
            Request {
                version: VERSION,
                source: png(w, h, &source).unwrap(),
                mask: png(w, h, &mask).unwrap(),
                operation: Operation::Fill,
                prompt: "synthetic edge".into(),
                model: photocraft_ai::DEFAULT_MODEL.into(),
                options: ApiOptions::default(),
            },
            caps,
            LocalOptions { diagnostics: true, ..Default::default() },
            &JobCtx::default(),
        )
        .unwrap()
    }
    fn response(c: &Diagnostic, dx: i32, dy: i32) -> Response {
        let [w, h] = c.plan.working;
        let src = pixels(&c.request.source).unwrap();
        let px = (0..h)
            .flat_map(|y| {
                let src = &src;
                (0..w).map(move |x| sample(src, w, h, x as f64 - f64::from(dx), y as f64 - f64::from(dy)))
            })
            .collect::<Vec<_>>();
        Response { quality: None, version: VERSION, image: png(w, h, &px).unwrap() }
    }
    #[test]
    fn image_mask_and_inverse_preserve_778x600_thin_lines_and_alpha() {
        let (w, h) = (778, 600);
        let mut px = vec![[0.2, 0.4, 0.6, 1.]; w * h];
        for y in 0..h {
            px[y * w + 155] = [1., 0., 0., 1.];
            px[y * w + 156] = [0., 0., 0., 0.];
        }
        let mut c = capture(w as u32, h as u32, px, false, Capabilities::native());
        assert_eq!(c.plan.scale, 1.);
        assert_eq!(c.plan.working, [778, 600]);
        let output = process(&mut c.clone(), response(&c, 0, 0), &JobCtx::default()).unwrap();
        let src = pixels(c.stages.get("source-document").unwrap()).unwrap();
        for (p, q) in output.as_chunks::<4>().0.iter().zip(&src) {
            assert_eq!(p, q);
        }
        c.response = Some(response(&c, 0, 0));
        c.validate().unwrap();
        let transmitted = pixels(&c.request.mask).unwrap();
        assert_eq!(transmitted[0][3], 1.0, "padding is protected");
        let q = c.plan.to_working((w / 2) as f64, (h / 2) as f64);
        assert_eq!(transmitted[q[1] as usize * c.plan.working[0] as usize + q[0] as usize][3], 0.0);
    }
    #[test]
    fn upscaled_transport_has_opaque_padding_and_replay_keeps_protected_pixels_exact() {
        let mut c = capture(517, 600, vec![[0.2, 0.4, 0.6, 1.]; 517 * 600], false, Capabilities::image2(true));
        assert_eq!(c.plan.working, [752, 880]);
        assert!(c.plan.scale > 1.);
        assert!(pixels(&c.request.source).unwrap().iter().all(|p| p[3] == 1.));
        let r = response(&c, 0, 0);
        process(&mut c, r.clone(), &JobCtx::default()).unwrap();
        c.validate().unwrap();
        let original = pixels(c.stages.get("source-document").unwrap()).unwrap();
        let insertion = pixels(c.stages.get("insertion-mask").unwrap()).unwrap();
        let final_px = pixels(c.stages.get("final-result").unwrap()).unwrap();
        for ((src, mask), dst) in original.iter().zip(insertion).zip(&final_px) {
            if mask[3] == 1. {
                assert_eq!(src, dst);
            }
        }
        let expected = c.stages.get("final-result").unwrap().clone();
        process(&mut c, r, &JobCtx::default()).unwrap();
        assert_eq!(c.stages.get("final-result").unwrap(), &expected);
    }
    #[test]
    fn boundary_color_field_tracks_opposite_local_casts_instead_of_a_cancelled_global_mean() {
        let (w, h) = (128_u32, 64_u32);
        let source = vec![[0.5, 0.5, 0.5, 1.]; (w * h) as usize];
        let result = (0..h).flat_map(|_| (0..w).map(|x| [if x < 64 { 0.58 } else { 0.42 }, 0.5, 0.5, 1.])).collect::<Vec<_>>();
        let insertion = (0..h).flat_map(|_| (0..w).map(|x| [1., 1., 1., if (40..88).contains(&x) { 0. } else { 1. }])).collect::<Vec<_>>();
        let plan = GeometryPlan::new(w, h, &ApiOptions::default(), Capabilities::native()).unwrap();
        let field = boundary_colors(&source, &insertion, &result, plan, [0, 0], &JobCtx::default()).unwrap();
        let left = sample(&field, 16, 8, 5., 4.);
        let right = sample(&field, 16, 8, 10., 4.);
        assert!(left[0] < -0.025, "{left:?}");
        assert!(right[0] > 0.025, "{right:?}");
        assert!(field.iter().all(|p| p[3] == 1. && p.iter().take(3).all(|v| v.abs() <= 0.12)));
    }
    #[test]
    fn premultiplied_sampling_does_not_leak_hidden_transparent_color() {
        let p = sample(&[[1., 0., 0., 0.], [0., 0., 1., 1.]], 2, 1, 0.5, 0.);
        assert_eq!(p, [0., 0., 1., 0.5]);
    }
    #[test]
    fn downscaled_editable_document_edges_use_pixel_footprints_for_coverage() {
        let (w, h) = (1025, 33);
        for caps in [Capabilities::native(), Capabilities::image2(true)] {
            let mut c = prepare(
                Request {
                    version: VERSION,
                    source: png(w, h, &vec![[0.2, 0.4, 0.6, 1.]; (w * h) as usize]).unwrap(),
                    mask: png(w, h, &vec![[1., 1., 1., 0.]; (w * h) as usize]).unwrap(),
                    operation: Operation::Fill,
                    prompt: "synthetic edge".into(),
                    model: photocraft_ai::DEFAULT_MODEL.into(),
                    options: ApiOptions { max_working_edge: 1024, ..Default::default() },
                },
                caps,
                LocalOptions::default(),
                &JobCtx::default(),
            )
            .unwrap();
            assert!(c.plan.scale < 1.);
            let r = response(&c, 0, 0);
            let output = process(&mut c, r, &JobCtx::default()).unwrap();
            assert_eq!(output.len(), (w * h * 4) as usize);
            assert!(output.iter().all(|v| v.is_finite()));
        }
    }
    #[test]
    fn shifted_context_is_corrected_only_when_unique_and_within_limit() {
        let mut c = capture(97, 81, patterned(97, 81), false, Capabilities::native());
        let r = response(&c, 2, -1);
        let expected = pixels(c.stages.get("source-document").unwrap()).unwrap();
        let output = process(&mut c, r, &JobCtx::default()).unwrap();
        assert_eq!(c.alignment.as_ref().unwrap().status, "corrected-translation");
        assert_eq!(c.alignment.as_ref().unwrap().translation, [2, -1]);
        let i = 40 * 97 + 40;
        assert_eq!(output.get(i * 4..i * 4 + 4).unwrap(), expected[i]);
        let mut disabled = c.clone();
        disabled.local.correct_translation = false;
        assert!(process(&mut disabled, response(&c, 2, -1), &JobCtx::default()).is_err());
        let mut outside = c.clone();
        assert!(process(&mut outside, response(&c, 7, 0), &JobCtx::default()).is_err());
    }
    #[test]
    fn flat_and_ambiguous_context_is_not_rejected_as_geometric_failure() {
        let mut flat = capture(96, 80, vec![[0.5, 0.5, 0.5, 1.]; 96 * 80], false, Capabilities::native());
        process(&mut flat.clone(), response(&flat, 0, 0), &JobCtx::default()).unwrap();
        let r = response(&flat, 0, 0);
        process(&mut flat, r, &JobCtx::default()).unwrap();
        assert_eq!(flat.alignment.unwrap().status, "insufficient-context");
        let stripes = (0..96 * 80).map(|i| if (i % 96) % 4 < 2 { [0.1, 0.1, 0.1, 1.] } else { [0.9, 0.9, 0.9, 1.] }).collect();
        let mut c = capture(96, 80, stripes, false, Capabilities::native());
        let r = response(&c, 1, 0);
        process(&mut c, r, &JobCtx::default()).unwrap();
        assert_eq!(c.alignment.unwrap().status, "insufficient-context");
    }
    #[test]
    fn bad_geometry_and_unexpected_sizes_leave_raw_diagnostics_available() {
        let mut c = capture(96, 80, patterned(96, 80), false, Capabilities::native());
        let bad = Response { quality: None, version: VERSION, image: png(96, 80, &vec![[0.2, 0.2, 0.2, 1.]; 96 * 80]).unwrap() };
        assert!(process(&mut c, bad, &JobCtx::default()).is_err());
        assert!(c.response.is_some());
        assert_eq!(c.alignment.as_ref().unwrap().status, "geometric-deviation");
        let wrong = Response { quality: None, version: VERSION, image: png(97, 80, &vec![[0.; 4]; 97 * 80]).unwrap() };
        assert!(process(&mut c, wrong, &JobCtx::default()).is_err());
        c.stages.insert("source-document".into(), png(80, 96, &vec![[0.; 4]; 80 * 96]).unwrap());
        assert!(c.validate().is_err(), "equal pixel counts cannot hide transposed diagnostic stages");
        c.stages.insert("source-document".into(), png(96, 80, &vec![[0.; 4]; 96 * 80]).unwrap());
        c.plan.scale = f64::NAN;
        assert!(c.validate().is_err());
    }
    #[test]
    fn soft_masks_are_not_feathered_twice_and_protected_pixels_are_exact() {
        let mut c = capture(48, 36, vec![[0.2, 0.4, 0.6, 1.]; 48 * 36], true, Capabilities::native());
        let r = Response { quality: None, version: VERSION, image: png(48, 36, &vec![[1., 0., 0., 1.]; 48 * 36]).unwrap() };
        process(&mut c, r, &JobCtx::default()).unwrap();
        let final_px = pixels(c.stages.get("final-result").unwrap()).unwrap();
        let original = pixels(c.stages.get("source-document").unwrap()).unwrap();
        let mask = pixels(c.stages.get("insertion-mask").unwrap()).unwrap();
        for ((p, q), m) in final_px.iter().zip(original).zip(mask) {
            if m[3] == 1. {
                assert_eq!(*p, q);
            } else {
                assert!((p[0] - 0.6).abs() < 0.005);
            }
        }
    }
    #[test]
    fn color_matching_has_a_bounded_measured_effect_and_is_off_by_default() {
        let mut c = capture(
            240,
            210,
            patterned(240, 210).iter().map(|p| [p[0] * 0.7 + 0.1, p[1] * 0.7 + 0.1, p[2] * 0.7 + 0.1, 1.]).collect(),
            false,
            Capabilities::native(),
        );
        let source = pixels(&c.request.source).unwrap();
        let changed: Vec<[f32; 4]> = source.iter().map(|p| [p[0] + 0.06, p[1] + 0.06, p[2] + 0.06, 1.]).collect();
        let r = Response { quality: None, version: VERSION, image: png(240, 210, &changed).unwrap() };
        let uncorrected = process(&mut c.clone(), r.clone(), &JobCtx::default()).unwrap();
        c.local.match_boundary_color = true;
        let corrected = process(&mut c, r, &JobCtx::default()).unwrap();
        let i = 70 * 240 + 80;
        assert!((corrected[i * 4] - source[i][0]).abs() < (uncorrected[i * 4] - source[i][0]).abs());
        let interior = (105 * 240 + 120) * 4;
        assert_eq!(&corrected[interior..interior + 4], &uncorrected[interior..interior + 4], "colour correction stops at 32 document pixels");
        assert!(!LocalOptions::default().match_boundary_color);
    }
    #[test]
    fn hard_boundary_blends_inside_only_and_expand_protects_original() {
        let mut c = capture(48, 36, vec![[0.2, 0.2, 0.2, 1.]; 48 * 36], false, Capabilities::native());
        let r = Response { quality: None, version: VERSION, image: png(48, 36, &vec![[0.8, 0.8, 0.8, 1.]; 48 * 36]).unwrap() };
        let output = process(&mut c, r.clone(), &JobCtx::default()).unwrap();
        assert!((output[(12 * 48 + 16) * 4] - 0.5).abs() < 0.01);
        c.request.operation = Operation::Expand;
        let output = process(&mut c, r, &JobCtx::default()).unwrap();
        assert_eq!(output[(12 * 48 + 16) * 4], 0.8, "an interior hole is not a canvas margin");
    }
    #[test]
    fn chromatic_edges_with_constant_mean_luminance_still_support_alignment() {
        let colors = patterned(97, 81).iter().map(|p| [p[0], 1. - p[0], 0., 1.]).collect();
        let mut c = capture(97, 81, colors, false, Capabilities::native());
        let r = response(&c, 2, 0);
        process(&mut c, r, &JobCtx::default()).unwrap();
        assert_eq!(c.alignment.as_ref().unwrap().status, "corrected-translation");
        assert_eq!(c.alignment.as_ref().unwrap().translation, [2, 0]);
    }
    #[test]
    fn preserved_geometry_with_changed_contrast_and_color_is_not_rejected() {
        for operation in [Operation::Fill, Operation::Remove, Operation::Expand] {
            let mut c = capture(97, 81, patterned(97, 81), false, Capabilities::native());
            c.request.operation = operation;
            let source = pixels(&c.request.source).unwrap();
            let changed: Vec<_> = source.iter().map(|p| [p[0] * 0.55 + 0.18, p[1] * 0.7 + 0.1, p[2] * 0.6 + 0.15, p[3]]).collect();
            let r = Response { quality: None, version: VERSION, image: png(97, 81, &changed).unwrap() };
            process(&mut c, r, &JobCtx::default()).unwrap();
            assert_eq!(c.alignment.as_ref().unwrap().status, "plausible");
        }
    }
    #[test]
    fn changed_contrast_does_not_hide_a_reliable_translation() {
        let mut c = capture(97, 81, patterned(97, 81), false, Capabilities::native());
        let mut r = response(&c, 2, -1);
        let changed: Vec<_> = pixels(&r.image).unwrap().iter().map(|p| [p[0] * 0.55 + 0.18, p[1] * 0.7 + 0.1, p[2] * 0.6 + 0.15, p[3]]).collect();
        r.image = png(97, 81, &changed).unwrap();
        process(&mut c, r, &JobCtx::default()).unwrap();
        assert_eq!(c.alignment.as_ref().unwrap().status, "corrected-translation");
        assert_eq!(c.alignment.as_ref().unwrap().translation, [2, -1]);
    }
    #[test]
    fn moderately_regenerated_texture_is_uncertain_instead_of_a_geometry_error() {
        let mut c = capture(97, 81, patterned(97, 81), false, Capabilities::native());
        let changed: Vec<_> = pixels(&c.request.source)
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let noise = ((i * 31 + i / 97 * 67) % 251) as f32 / 251.;
                [p[0] * 0.4 + noise * 0.3 + 0.15, p[1] * 0.4 + noise * 0.3 + 0.15, p[2] * 0.4 + noise * 0.3 + 0.15, p[3]]
            })
            .collect();
        let r = Response { quality: None, version: VERSION, image: png(97, 81, &changed).unwrap() };
        process(&mut c, r, &JobCtx::default()).unwrap();
        let report = c.alignment.unwrap();
        assert_eq!(report.status, "insufficient-context");
        assert!((0.35..0.9).contains(&report.correlation.unwrap()));
        assert_eq!(report.translation, [0, 0]);
    }
    #[test]
    fn clear_translation_beyond_the_limit_is_still_rejected() {
        let mut c = capture(97, 81, patterned(97, 81), false, Capabilities::native());
        let r = response(&c, 7, 0);
        assert!(process(&mut c, r, &JobCtx::default()).is_err());
        assert_eq!(c.alignment.as_ref().unwrap().status, "geometric-deviation");
        assert!(c.response.is_some());
    }
}
