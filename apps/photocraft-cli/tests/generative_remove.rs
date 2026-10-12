//! Exercise the actual CLI/selection/action/export path without an API request.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use photocraft_ai::{ApiOptions, Capabilities, DEFAULT_MODEL, GeometryPlan};
use photocraft_codecs::{ChannelLayout, Format, Image};
use serde_json::Value;

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("pc-cli-remove-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let f = Self(dir);
        png(&f.path("input.png"), &Image::from_u8(16, 12, ChannelLayout::Rgba, [30, 60, 90, 255].repeat(16 * 12)).unwrap());
        let mask = (0..12).flat_map(|y| (0..16).map(move |x| if (3..13).contains(&x) && (2..10).contains(&y) { 255 } else { 0 })).collect();
        png(&f.path("mask.png"), &Image::from_u8(16, 12, ChannelLayout::Gray, mask).unwrap());
        let plan = GeometryPlan::new(16, 12, &ApiOptions::default(), Capabilities::for_model(DEFAULT_MODEL).unwrap()).unwrap();
        png(
            &f.path("response.png"),
            &Image::from_u8(plan.working[0], plan.working[1], ChannelLayout::Rgba, [180, 90, 30, 255].repeat((plan.working[0] * plan.working[1]) as usize))
                .unwrap(),
        );
        f
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn command(&self) -> Command {
        self.command_for("generative-remove")
    }
    fn command_for(&self, operation: &str) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_photocraft-cli"));
        // An invalid key proves the offline adapter never initializes the HTTP provider.
        cmd.env("OPENAI_API_KEY", "invalid\nkey")
            .arg(operation)
            .arg(self.path("input.png"))
            .arg("--mask")
            .arg(self.path("mask.png"))
            .arg("--out")
            .arg(self.path("result.png"))
            .arg("--response")
            .arg(self.path("response.png"));
        cmd
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn png(path: &Path, image: &Image) {
    std::fs::write(path, photocraft_codecs::encode(image, Format::Png, &Default::default()).unwrap()).unwrap();
}
fn success(output: Output) -> Value {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}
fn failed(output: Output, message: &str) {
    assert_eq!(output.status.code(), Some(1), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stderr).contains(message), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn offline_remove_preserves_protected_pixels_and_exports_real_capture() {
    let f = Fixture::new("apply");
    let result = success(
        f.command()
            .args(["--prompt", "bottle", "--model", "gpt-image-2.5-sunburst-2026-09-08", "--quality", "xhigh", "--diagnostics"])
            .arg(f.path("diagnostics"))
            .output()
            .unwrap(),
    );
    assert_eq!(result["offline"], true);
    let before = photocraft_codecs::decode(&std::fs::read(f.path("input.png")).unwrap()).unwrap().to_rgba8();
    let after = photocraft_codecs::decode(&std::fs::read(f.path("result.png")).unwrap()).unwrap().to_rgba8();
    for y in 0..12 {
        for x in 0..16 {
            let i = (y * 16 + x) * 4;
            if !(3..13).contains(&x) || !(2..10).contains(&y) {
                assert_eq!(before[i..i + 4], after[i..i + 4], "protected pixel {x},{y}");
            }
        }
    }
    let center = (6 * 16 + 8) * 4;
    assert_ne!(before[center..center + 4], after[center..center + 4]);
    let capture: Value = serde_json::from_slice(&std::fs::read(f.path("diagnostics/capture.json")).unwrap()).unwrap();
    assert_eq!(capture["applied"], true);
    assert_eq!(capture["request"]["operation"], "remove");
    assert_eq!(capture["request"]["model"], "gpt-image-2.5-sunburst-2026-09-08");
    assert_eq!(capture["request"]["options"]["quality"], "xhigh");
    assert!(capture["request"]["prompt"].as_str().unwrap().contains("bottle"));
    assert!(capture["response"]["quality"].is_null(), "do not fabricate provider quality");
    for name in ["request-source.png", "request-mask.png", "response.png", "final-result.png"] {
        assert!(f.path(&format!("diagnostics/{name}")).exists(), "{name}");
    }
}

#[test]
fn incompatible_geometry_keeps_response_diagnostic_but_writes_no_result() {
    let f = Fixture::new("geometry");
    std::fs::copy(f.path("input.png"), f.path("response.png")).unwrap();
    failed(f.command().arg("--diagnostics").arg(f.path("diagnostics")).output().unwrap(), "unexpected size");
    assert!(!f.path("result.png").exists());
    assert_eq!(std::fs::read(f.path("diagnostics/response.png")).unwrap(), std::fs::read(f.path("response.png")).unwrap());
    let capture: Value = serde_json::from_slice(&std::fs::read(f.path("diagnostics/capture.json")).unwrap()).unwrap();
    assert_eq!(capture["applied"], false);
    assert!(capture["error"].is_string());
}

#[test]
fn rejects_wrong_mask_dimensions_and_empty_selection() {
    for (name, width, height, message) in [("mask-size", 8, 6, "size"), ("mask-empty", 16, 12, "selection")] {
        let f = Fixture::new(name);
        png(&f.path("mask.png"), &Image::from_u8(width, height, ChannelLayout::Gray, vec![0; (width * height) as usize]).unwrap());
        failed(f.command().output().unwrap(), message);
        assert!(!f.path("result.png").exists());
    }
}

#[test]
fn preserves_existing_files_and_requires_explicit_overwrite() {
    let f = Fixture::new("overwrite");
    std::fs::write(f.path("result.png"), b"keep me").unwrap();
    failed(f.command().output().unwrap(), "already exists");
    assert_eq!(std::fs::read(f.path("result.png")).unwrap(), b"keep me");
    success(f.command().arg("--overwrite").output().unwrap());
    for input in ["input.png", "mask.png", "response.png"] {
        let bytes = std::fs::read(f.path(input)).unwrap();
        failed(f.command().arg("--out").arg(f.path(input)).arg("--overwrite").output().unwrap(), "must not replace");
        assert_eq!(std::fs::read(f.path(input)).unwrap(), bytes);
    }
}

#[test]
fn gray_mask_produces_partial_coverage_and_invalid_output_fails_before_processing() {
    let f = Fixture::new("gray");
    let mask = (0..12).flat_map(|y| (0..16).map(move |x| if (3..13).contains(&x) && (2..10).contains(&y) { 128 } else { 0 })).collect();
    png(&f.path("mask.png"), &Image::from_u8(16, 12, ChannelLayout::Gray, mask).unwrap());
    success(f.command().output().unwrap());
    let result = photocraft_codecs::decode(&std::fs::read(f.path("result.png")).unwrap()).unwrap().to_rgba8();
    let center = (6 * 16 + 8) * 4;
    assert!(result[center] > 30 && result[center] < 180);
    assert_eq!(&result[0..4], &[30, 60, 90, 255]);
    failed(f.command().arg("--out").arg(f.path("result.unknown")).output().unwrap(), "unsupported output format");
    assert!(!f.path("result.unknown").exists());
}

#[test]
fn rejects_invalid_quality_corrupt_response_and_reused_diagnostic_folder() {
    let f = Fixture::new("invalid");
    failed(f.command().args(["--quality", "ultra"]).output().unwrap(), "quality");
    failed(f.command().args(["--model", "gpt-image-2", "--quality", "xhigh"]).output().unwrap(), "support");
    std::fs::create_dir(f.path("diagnostics")).unwrap();
    std::fs::write(f.path("diagnostics/keep"), b"keep").unwrap();
    failed(f.command().arg("--diagnostics").arg(f.path("diagnostics")).output().unwrap(), "fresh diagnostic directory");
    assert_eq!(std::fs::read(f.path("diagnostics/keep")).unwrap(), b"keep");
    std::fs::write(f.path("response.png"), b"not a PNG").unwrap();
    assert!(!f.command().output().unwrap().status.success());
    assert!(!f.path("result.png").exists());
}

#[test]
fn live_upload_requires_consent_then_a_key() {
    let f = Fixture::new("consent");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_photocraft-cli"));
    cmd.env_remove("OPENAI_API_KEY")
        .arg("generative-remove")
        .arg(f.path("input.png"))
        .arg("--mask")
        .arg(f.path("mask.png"))
        .arg("--out")
        .arg(f.path("result.png"));
    failed(cmd.output().unwrap(), "--consent");
    failed(cmd.arg("--consent").output().unwrap(), "OPENAI_API_KEY");
    assert!(!f.path("result.png").exists());
}

#[test]
fn native_sixteen_bit_document_and_layer_are_preserved() {
    let f = Fixture::new("sixteen");
    png(&f.path("input.png"), &Image::from_u16(16, 12, ChannelLayout::Rgba, &[7000, 15000, 22000, 65535].repeat(16 * 12)).unwrap());
    success(f.command().arg("--out").arg(f.path("result.pcraft")).output().unwrap());
    let output = Command::new(env!("CARGO_BIN_EXE_photocraft-cli")).arg("info").arg(f.path("result.pcraft")).output().unwrap();
    let info = success(output);
    assert_eq!(info["depth"], 16, "{info}");
    assert!(info.to_string().contains("Generative Remove"));
}

#[test]
fn fill_uses_engine_selection_and_requires_a_description() {
    let f = Fixture::new("fill");
    failed(f.command_for("generative-fill").output().unwrap(), "nonempty --prompt");
    let output = success(f.command_for("generative-fill").args(["--prompt", "small red object", "--diagnostics"]).arg(f.path("diagnostics")).output().unwrap());
    assert_eq!(output["operation"], "generative-fill");
    let capture: Value = serde_json::from_slice(&std::fs::read(f.path("diagnostics/capture.json")).unwrap()).unwrap();
    assert_eq!(capture["request"]["operation"], "fill");
    assert_eq!(capture["request"]["prompt"], "small red object");
    let before = photocraft_codecs::decode(&std::fs::read(f.path("input.png")).unwrap()).unwrap().to_rgba8();
    let after = photocraft_codecs::decode(&std::fs::read(f.path("result.png")).unwrap()).unwrap().to_rgba8();
    assert_eq!(&before[0..4], &after[0..4]);
    assert_ne!(before, after);
}

#[test]
fn expand_preserves_original_pixels_and_uses_requested_anchor() {
    let f = Fixture::new("expand");
    // The small test target has the same model working geometry as the recorded PNG.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_photocraft-cli"));
    cmd.env_remove("OPENAI_API_KEY")
        .arg("generative-expand")
        .arg(f.path("input.png"))
        .args(["--width", "24", "--height", "16", "--anchor", "bottomRight", "--out"])
        .arg(f.path("expanded.png"))
        .arg("--response")
        .arg(f.path("response.png"));
    let plan = GeometryPlan::new(24, 16, &ApiOptions::default(), Capabilities::for_model(DEFAULT_MODEL).unwrap()).unwrap();
    png(
        &f.path("response.png"),
        &Image::from_u8(plan.working[0], plan.working[1], ChannelLayout::Rgba, [180, 90, 30, 255].repeat((plan.working[0] * plan.working[1]) as usize))
            .unwrap(),
    );
    let output = success(cmd.output().unwrap());
    assert_eq!(output["action"]["offset"], serde_json::json!([8, 4]));
    let image = photocraft_codecs::decode(&std::fs::read(f.path("expanded.png")).unwrap()).unwrap();
    assert_eq!((image.width(), image.height()), (24, 16));
    let pixels = image.to_rgba8();
    for y in 4..16 {
        for x in 8..24 {
            let i = (y * 24 + x) * 4;
            assert_eq!(&pixels[i..i + 4], &[30, 60, 90, 255]);
        }
    }
    assert_eq!(&pixels[0..4], &[180, 90, 30, 255]);
}

#[test]
fn expand_banner_preset_and_invalid_dimensions_are_checked_offline() {
    let f = Fixture::new("banner");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_photocraft-cli"));
    cmd.arg("generative-expand")
        .arg(f.path("input.png"))
        .args(["--aspect", "16:9", "--out"])
        .arg(f.path("banner.png"))
        .arg("--response")
        .arg(f.path("response.png"));
    let plan = GeometryPlan::new(32, 18, &ApiOptions::default(), Capabilities::for_model(DEFAULT_MODEL).unwrap()).unwrap();
    png(
        &f.path("response.png"),
        &Image::from_u8(plan.working[0], plan.working[1], ChannelLayout::Rgba, [180, 90, 30, 255].repeat((plan.working[0] * plan.working[1]) as usize))
            .unwrap(),
    );
    let output = success(cmd.output().unwrap());
    assert_eq!(output["action"]["width"], 32);
    assert_eq!(output["action"]["height"], 18);
    let mut invalid = Command::new(env!("CARGO_BIN_EXE_photocraft-cli"));
    invalid
        .arg("generative-expand")
        .arg(f.path("input.png"))
        .args(["--width", "8", "--height", "12", "--out"])
        .arg(f.path("invalid.png"))
        .arg("--response")
        .arg(f.path("response.png"));
    failed(invalid.output().unwrap(), "without shrinking");
    failed(invalid.args(["--width", "no"]).output().unwrap(), "positive integer");
    assert!(!f.path("invalid.png").exists());
}

#[test]
fn processing_options_and_transport_choice_are_typed_and_exported() {
    let f = Fixture::new("processing");
    failed(f.command().args(["--mask-mode", "unknown"]).output().unwrap(), "--mask-mode");
    failed(f.command().args(["--processing", "{\"maxTranslation\":9}"]).output().unwrap(), "translation");
    failed(f.command().args(["--processing", "{\"typo\":true}"]).output().unwrap(), "typo");
    success(
        f.command()
            .args(["--mask-mode", "reference", "--processing", "{\"transitionWidth\":5,\"matchBoundaryColor\":true}", "--diagnostics"])
            .arg(f.path("diagnostics"))
            .output()
            .unwrap(),
    );
    let capture: Value = serde_json::from_slice(&std::fs::read(f.path("diagnostics/capture.json")).unwrap()).unwrap();
    assert_eq!(capture["local"]["transitionWidth"], 5);
    assert_eq!(capture["local"]["matchBoundaryColor"], true);
    let transport: Value = serde_json::from_slice(&std::fs::read(f.path("diagnostics/transport.json")).unwrap()).unwrap();
    assert_eq!(transport["resolvedMaskMode"], "reference");
    assert_eq!(transport["offline"], true);
    assert!(f.path("diagnostics/reference-mask.png").exists());
}
