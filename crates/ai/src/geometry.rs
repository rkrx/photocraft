//! Provider capabilities and pixel-centre mapping. Coordinates refer to pixel centres at x+0.5.
use crate::{Error, MAX_EDGE, Result, dimensions};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Auto,
    Low,
    Medium,
    #[default]
    High,
    Xhigh,
    Max,
}
impl Quality {
    pub const ALL: &'static [Self] = &[Self::Auto, Self::Low, Self::Medium, Self::High, Self::Xhigh, Self::Max];
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiOptions {
    pub quality: Quality,
    pub max_working_edge: u32,
}
impl Default for ApiOptions {
    fn default() -> Self {
        Self { quality: Quality::High, max_working_edge: MAX_EDGE }
    }
}
impl ApiOptions {
    pub fn validate(&self) -> Result<()> {
        if !(1024..=MAX_EDGE).contains(&self.max_working_edge) {
            return Err(Error::Invalid("working edge limit must be 1024–2048 pixels".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub custom_sizes: bool,
    pub multiple: u32,
    pub min_pixels: u64,
    pub max_ratio: u32,
    pub extended_quality: bool,
}
impl Capabilities {
    /// Documented built-in model profiles; unknown external models remain backend-owned.
    pub fn for_model(model: &str) -> Option<Self> {
        match model {
            "gpt-image-2.5-sunburst" | "gpt-image-2.5-flare" | "gpt-image-2.5-sunburst-2026-09-08" | "gpt-image-2.5-flare-2026-09-08" => {
                Some(Self::image2(true))
            }
            "gpt-image-2" | "gpt-image-2-2026-04-21" => Some(Self::image2(false)),
            "gpt-image-1" | "gpt-image-1.5" | "gpt-image-1-mini" => Some(Self::image1()),
            _ => None,
        }
    }
    pub const fn native() -> Self {
        Self { custom_sizes: true, multiple: 1, min_pixels: 1, max_ratio: MAX_EDGE, extended_quality: true }
    }
    pub const fn image2(extended_quality: bool) -> Self {
        Self { custom_sizes: true, multiple: 16, min_pixels: 655_360, max_ratio: 3, extended_quality }
    }
    pub const fn image1() -> Self {
        Self { custom_sizes: false, multiple: 16, min_pixels: 655_360, max_ratio: 3, extended_quality: false }
    }
    pub fn validate(&self, options: &ApiOptions, width: u32, height: u32) -> Result<()> {
        options.validate()?;
        dimensions(width, height)?;
        if self.multiple == 0
            || self.max_ratio == 0
            || width > options.max_working_edge
            || height > options.max_working_edge
            || !width.is_multiple_of(self.multiple)
            || !height.is_multiple_of(self.multiple)
            || u64::from(width) * u64::from(height) < self.min_pixels
            || width.max(height) as u64 > width.min(height) as u64 * u64::from(self.max_ratio)
            || (!self.custom_sizes && !matches!((width, height), (1024, 1024) | (1536, 1024) | (1024, 1536)))
        {
            return Err(Error::Invalid("output size is unsupported by this provider or working edge limit".into()));
        }
        if !self.extended_quality && matches!(options.quality, Quality::Xhigh | Quality::Max) {
            return Err(Error::Invalid("this model does not support xhigh or max quality".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeometryPlan {
    pub document: [u32; 2],
    pub working: [u32; 2],
    pub scale: f64,
    pub offset: [f64; 2],
}
impl GeometryPlan {
    pub fn new(width: u32, height: u32, options: &ApiOptions, caps: Capabilities) -> Result<Self> {
        dimensions(width, height)?;
        options.validate()?;
        if !caps.custom_sizes {
            let mut best: Option<(f64, u32, u32)> = None;
            for (w, h) in [(1024, 1024), (1536, 1024), (1024, 1536)] {
                if caps.validate(options, w, h).is_err() {
                    continue;
                }
                let scale = (f64::from(w) / f64::from(width)).min(f64::from(h) / f64::from(height)).min(1.);
                if best.is_none_or(|b| scale > b.0 || (scale == b.0 && w * h < b.1 * b.2)) {
                    best = Some((scale, w, h));
                }
            }
            let (scale, w, h) = best.ok_or_else(|| Error::Invalid("no supported model size fits the working edge limit and quality".into()))?;
            return Ok(Self {
                document: [width, height],
                working: [w, h],
                scale,
                offset: [((f64::from(w) - f64::from(width) * scale) / 2.).floor(), ((f64::from(h) - f64::from(height) * scale) / 2.).floor()],
            });
        }
        // Fill the provider's minimum pixel budget with image content rather than a large
        // transparent border. One scale applies to both image and mask; native backends keep 1:1.
        let limit_scale = f64::from(options.max_working_edge) / f64::from(width.max(height));
        let scale = if caps == Capabilities::native() {
            limit_scale.min(1.0)
        } else {
            (caps.min_pixels as f64 / (f64::from(width) * f64::from(height))).sqrt().max(1.0).min(limit_scale)
        };
        if !caps.extended_quality && matches!(options.quality, Quality::Xhigh | Quality::Max) {
            return Err(Error::Invalid("this model does not support xhigh or max quality".into()));
        }
        if caps.multiple == 0 || caps.multiple > MAX_EDGE || caps.max_ratio == 0 {
            return Err(Error::Invalid("invalid provider capabilities".into()));
        }
        if caps == Capabilities::native() {
            return Ok(Self {
                document: [width, height],
                working: [(f64::from(width) * scale).ceil() as u32, (f64::from(height) * scale).ceil() as u32],
                scale,
                offset: [0., 0.],
            });
        }
        let min_w = (f64::from(width) * scale).ceil() as u32;
        let min_h = (f64::from(height) * scale).ceil() as u32;
        let mut best: Option<(f64, u32, u32)> = None;
        for w in (min_w.div_ceil(caps.multiple) * caps.multiple..=options.max_working_edge).step_by(caps.multiple as usize) {
            for h in (min_h.div_ceil(caps.multiple) * caps.multiple..=options.max_working_edge).step_by(caps.multiple as usize) {
                if u64::from(w) * u64::from(h) < caps.min_pixels
                    || w.max(h) as u64 > w.min(h) as u64 * u64::from(caps.max_ratio)
                    || (!caps.custom_sizes && !matches!((w, h), (1024, 1024) | (1536, 1024) | (1024, 1536)))
                {
                    continue;
                }
                // Smallest area first; break ties by balanced padding. No asymmetric stretching.
                let score = f64::from(w) * f64::from(h) + ((f64::from(w) - f64::from(min_w)) - (f64::from(h) - f64::from(min_h))).abs() / 8192.0;
                if best.is_none_or(|b| score < b.0) {
                    best = Some((score, w, h));
                }
            }
        }
        let (_, w, h) = best.ok_or_else(|| Error::Invalid("no valid working surface fits these settings".into()))?;
        Ok(Self {
            document: [width, height],
            working: [w, h],
            scale,
            offset: [((f64::from(w) - f64::from(width) * scale) / 2.0).floor(), ((f64::from(h) - f64::from(height) * scale) / 2.0).floor()],
        })
    }
    pub fn to_working(self, x: f64, y: f64) -> [f64; 2] {
        [(x + 0.5) * self.scale + self.offset[0] - 0.5, (y + 0.5) * self.scale + self.offset[1] - 0.5]
    }
    pub fn to_document(self, x: f64, y: f64) -> [f64; 2] {
        [(x + 0.5 - self.offset[0]) / self.scale - 0.5, (y + 0.5 - self.offset[1]) / self.scale - 0.5]
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn odd_small_extreme_and_limit_sizes_have_one_exact_inverse() {
        for (w, h) in [(778, 600), (779, 601), (1, 1), (2048, 2048), (2048, 1), (1, 2048)] {
            let p = GeometryPlan::new(w, h, &ApiOptions::default(), Capabilities::image2(true)).unwrap();
            Capabilities::image2(true).validate(&ApiOptions::default(), p.working[0], p.working[1]).unwrap();
            assert!(p.scale >= 1.0);
            for (x, y) in [(0., 0.), (w as f64 - 1., h as f64 - 1.)] {
                let q = p.to_working(x, y);
                let inverse = p.to_document(q[0], q[1]);
                assert!((inverse[0] - x).abs() < 1e-9 && (inverse[1] - y).abs() < 1e-9);
            }
        }
        assert!(GeometryPlan::new(0, 600, &ApiOptions::default(), Capabilities::image2(true)).is_err());
        assert!(GeometryPlan::new(2049, 600, &ApiOptions::default(), Capabilities::image2(true)).is_err());
    }
    #[test]
    fn small_photograph_uses_minimal_padding_and_uniform_upscale() {
        let plan = GeometryPlan::new(517, 600, &ApiOptions::default(), Capabilities::image2(true)).unwrap();
        assert_eq!(plan.working, [752, 880]);
        assert!((plan.scale - (655360.0_f64 / (517.0 * 600.0)).sqrt()).abs() < 1e-9);
        let content_area = 517.0 * 600.0 * plan.scale * plan.scale;
        assert!(content_area / (f64::from(plan.working[0]) * f64::from(plan.working[1])) > 0.98);
        assert_eq!(GeometryPlan::new(517, 600, &ApiOptions::default(), Capabilities::native()).unwrap().scale, 1.0);
    }
    #[test]
    fn lower_limit_scales_uniformly_and_model_quality_is_checked() {
        let options = ApiOptions { max_working_edge: 1024, ..Default::default() };
        let p = GeometryPlan::new(2001, 1700, &options, Capabilities::image2(true)).unwrap();
        assert_eq!(p.scale, 1024. / 2001.);
        let o = ApiOptions { quality: Quality::Xhigh, ..options };
        assert!(GeometryPlan::new(778, 600, &o, Capabilities::image2(false)).is_err());
    }
}
