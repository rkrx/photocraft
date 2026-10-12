//! Generative editing and shared AI settings. Credentials stay outside inspectable JSON fields.
use crate::{PhotocraftApp, state::DialogKind};
use serde_json::{Map, Value, json};

fn provider_name(app: &PhotocraftApp) -> &'static str {
    match app.session.generative_backend.as_ref().map(|b| b.id()) {
        None | Some("openai") => "OpenAI",
        Some(id) => id,
    }
}

pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key("__generative")
}

fn command(fields: &Map<String, Value>) -> Option<&'static str> {
    match fields.get("__command").and_then(Value::as_str) {
        Some(photocraft_engine::generative_cmds::FILL) => Some(photocraft_engine::generative_cmds::FILL),
        Some(photocraft_engine::generative_cmds::REMOVE) => Some(photocraft_engine::generative_cmds::REMOVE),
        Some(photocraft_engine::generative_expand_cmds::EXPAND) => Some(photocraft_engine::generative_expand_cmds::EXPAND),
        _ => None,
    }
}

fn valid_prompt(command: &str, text: &str) -> bool {
    let fill = command == photocraft_engine::generative_cmds::FILL;
    let limit = if fill { 32_000 } else { 31_000 };
    text.chars().count() <= limit && (!fill || !text.trim().is_empty())
}

fn last_prompt(app: &PhotocraftApp, command: &str) -> String {
    if !app.session.prefs().integrations.remember_dialog_texts {
        return String::new();
    }
    app.session
        .prefs()
        .dialogs
        .get(command)
        .and_then(|v| v.get("prompt"))
        .and_then(Value::as_str)
        .filter(|text| valid_prompt(command, text))
        .unwrap_or("")
        .to_string()
}

pub fn open(app: &mut PhotocraftApp, command: &str) -> Result<u64, String> {
    let label = match command {
        photocraft_engine::generative_cmds::FILL => "Generative Fill",
        photocraft_engine::generative_cmds::REMOVE => "Generative Remove",
        photocraft_engine::generative_expand_cmds::EXPAND => "Generative Expand",
        _ => return Err("unknown generative operation".into()),
    };
    let spec = photocraft_engine::commands::find(command).ok_or("generative editing is unavailable")?;
    (spec.enabled)(&app.session)?;
    let st = app.session.active().ok_or("no document open")?;
    let fields = json!({"__generative": true, "__command": spec.id, "__label": label,
        "document": st.doc.id.0, "revision": st.revision,
        "prompt": last_prompt(app, command), "consent": false});
    let mut map = fields.as_object().cloned().ok_or("could not open generative editing")?;
    if command == photocraft_engine::generative_expand_cmds::EXPAND {
        let size = st.doc.size;
        let target = photocraft_engine::generative_expand_cmds::banner_size(size).unwrap_or_else(|| {
            if size.width < photocraft_ai::MAX_EDGE {
                photocraft_doc::Size::new(size.width.saturating_add((size.width / 2).max(1)).min(photocraft_ai::MAX_EDGE), size.height)
            } else {
                photocraft_doc::Size::new(size.width, size.height.saturating_add((size.height / 2).max(1)).min(photocraft_ai::MAX_EDGE))
            }
        });
        map.insert("width".into(), json!(target.width));
        map.insert("height".into(), json!(target.height));
        map.insert("anchor".into(), json!("center"));
    }
    Ok(app.ui.open_dialog(DialogKind::Command, map))
}

/// Runtime credentials for Preferences › Integrations; the model uses normal preferences.
pub fn settings_body(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if crate::widgets::secondary_button(ui, tl!("Generation diagnostics…"), 200.0).clicked() {
        crate::generative_diagnostics_ui::open(app);
    }
    let backend = provider_name(app);
    ui.label(crate::i18n::fmt(tl!("Image provider: {provider}"), &[("provider", backend)]));
    if app.services.generative_backend.is_some() {
        ui.label(tl!("API key (session only)"));
        ui.add(egui::TextEdit::singleline(&mut app.ui.generative_key.0).password(true).char_limit(1024));
        if app.session.generative_backend.is_some() {
            ui.small(tl!("Leave empty to keep the connected backend."));
        }
    } else if app.session.generative_backend.is_none() {
        ui.label(tl!("No image provider installed. Use the desktop app or configure a backend."));
    }
}

/// The normal Preferences draft, so Apply/OK/Cancel and persistence are shared with all settings.
pub fn options_body(ui: &mut egui::Ui, obj: &mut Map<String, Value>, fields: &mut Map<String, Value>) {
    let mut api: photocraft_ai::ApiOptions = serde_json::from_value(obj.get("imageOptions").cloned().unwrap_or(Value::Null)).unwrap_or_default();
    let mut local: photocraft_engine::generative_pipeline::LocalOptions =
        serde_json::from_value(obj.get("imageProcessing").cloned().unwrap_or(Value::Null)).unwrap_or_default();
    ui.label(tl!("Model"));
    let mut selected_model = obj.get("imageModel").and_then(Value::as_str).unwrap_or(photocraft_ai::DEFAULT_MODEL).to_string();
    let mut models: Vec<(String, &str)> = [
        "gpt-image-2.5-sunburst",
        "gpt-image-2.5-flare",
        "gpt-image-2.5-sunburst-2026-09-08",
        "gpt-image-2.5-flare-2026-09-08",
        "gpt-image-2",
        "gpt-image-2-2026-04-21",
        "gpt-image-1.5",
        "gpt-image-1",
        "gpt-image-1-mini",
    ]
    .into_iter()
    .map(|m| (m.to_string(), m))
    .collect();
    let custom = selected_model.clone();
    if !models.iter().any(|(m, _)| m == &selected_model) {
        models.push((custom.clone(), &custom));
    }
    crate::widgets::dropdown(ui, "generation-model", &mut selected_model, &models, 300.0);
    obj.insert("imageModel".into(), json!(selected_model));
    ui.label(tl!("Generation quality"));
    let model = obj.get("imageModel").and_then(Value::as_str).unwrap_or(photocraft_ai::DEFAULT_MODEL);
    let extended = photocraft_ai::Capabilities::for_model(model).is_some_and(|caps| caps.extended_quality);
    let mut quality = api.quality.name().to_string();
    let choices: Vec<(String, &str)> = photocraft_ai::Quality::ALL
        .iter()
        .filter(|q| extended || !matches!(q, photocraft_ai::Quality::Xhigh | photocraft_ai::Quality::Max))
        .map(|q| (q.name().into(), q.name()))
        .collect();
    crate::widgets::dropdown(ui, "generation-quality", &mut quality, &choices, 160.0);
    if let Ok(q) = serde_json::from_value(json!(quality)) {
        api.quality = q;
    }
    ui.small(tl!("Higher quality does not guarantee better transitions."));
    let mut advanced = fields.get("advanced").and_then(Value::as_bool).unwrap_or(false);
    ui.checkbox(&mut advanced, tl!("Advanced"));
    fields.insert("advanced".into(), json!(advanced));
    if advanced {
        ui.label(tl!("API output"));
        ui.small(tl!("Working size is calculated locally and sent explicitly."));
        ui.horizontal(|ui| {
            ui.label(tl!("Working edge limit (pixels)"));
            ui.add(egui::DragValue::new(&mut api.max_working_edge).range(1024..=2048));
        });
        ui.separator();
        ui.label(tl!("Local processing"));
        ui.checkbox(&mut local.check_alignment, tl!("Check preserved context alignment"));
        ui.add_enabled_ui(local.check_alignment, |ui| {
            ui.checkbox(&mut local.correct_translation, tl!("Correct reliable small translations"));
            ui.horizontal(|ui| {
                ui.label(tl!("Maximum translation (working pixels)"));
                ui.add(egui::DragValue::new(&mut local.max_translation).range(0..=8));
            });
        });
        ui.horizontal(|ui| {
            ui.label(tl!("Transition width (document pixels)"));
            ui.add(egui::DragValue::new(&mut local.transition_width).range(0..=32));
        });
        ui.add_enabled_ui(local.check_alignment, |ui| {
            ui.checkbox(&mut local.match_boundary_color, tl!("Match boundary color (experimental)"));
        });
        ui.checkbox(&mut local.diagnostics, tl!("Keep diagnostics for explicit export"));
        ui.small(tl!("Diagnostics contain images and prompts. Nothing is exported automatically."));
    }
    obj.insert("imageOptions".into(), json!(api));
    obj.insert("imageProcessing".into(), json!(local));
}

/// Prepare a replacement without modifying the connected provider or exposing the key.
pub fn settings_backend(app: &PhotocraftApp) -> Result<Option<std::sync::Arc<dyn photocraft_ai::Backend>>, String> {
    let key = &app.ui.generative_key.0;
    if key.trim().is_empty() {
        return Ok(None);
    }
    let factory = app.services.generative_backend.as_ref().ok_or("no image provider in this build")?;
    factory(key).map(Some)
}

/// Whether Enter must stay in this form (editing a field or opening settings).
pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, fields: &mut Map<String, Value>) -> bool {
    let backend = provider_name(app);
    ui.label(crate::i18n::fmt(tl!("Image provider: {provider}"), &[("provider", backend)]));
    ui.label(format!("{}: {}", tl!("Model"), app.session.prefs().integrations.image_model));
    let settings = &app.session.prefs().integrations;
    ui.label(format!("{}: {}", tl!("Generation quality"), settings.image_options.quality.name()));
    if let Some(st) = app.session.active() {
        let w = fields.get("width").and_then(Value::as_u64).unwrap_or(u64::from(st.doc.size.width)) as u32;
        let h = fields.get("height").and_then(Value::as_u64).unwrap_or(u64::from(st.doc.size.height)) as u32;
        if let Some(backend) = app.session.generative_backend.as_ref()
            && let Ok(caps) = backend.capabilities(&settings.image_model)
            && let Ok(plan) = photocraft_ai::GeometryPlan::new(w, h, &settings.image_options, caps)
        {
            ui.label(format!("{}: {} × {}", tl!("Working surface"), plan.working[0], plan.working[1]));
        }
    }
    if app.session.generative_backend.is_none() {
        if app.services.generative_backend.is_some() {
            ui.small(tl!("Configure an image provider in AI Integrations."));
        } else {
            ui.small(tl!("No image provider installed. Use the desktop app or configure a backend."));
        }
    }
    let settings_opened = crate::widgets::secondary_button(ui, tl!("AI Integrations…"), 150.0).clicked();
    if settings_opened {
        crate::prefs_ui::open_preferences(app, "integrations");
    }
    let mut prompt = fields.get("prompt").and_then(Value::as_str).unwrap_or("").to_string();
    let remove = command(fields) == Some(photocraft_engine::generative_cmds::REMOVE);
    let expand = command(fields) == Some(photocraft_engine::generative_expand_cmds::EXPAND);
    let mut geometry_focused = false;
    if remove {
        ui.label(tl!("Select the object with a small margin of background. The background will be reconstructed."));
        ui.label(tl!("Objects to remove (optional)"));
    } else if expand {
        geometry_focused = expansion_body(app, ui, fields);
        ui.label(tl!("Scene to continue (optional)"));
    } else {
        ui.label(tl!("Describe what to change in the selection"));
    }
    let prompt_focused = ui
        .add(
            egui::TextEdit::multiline(&mut prompt)
                .id(egui::Id::new("generative-prompt"))
                .desired_rows(if expand { 2 } else { 4 })
                .desired_width(f32::INFINITY)
                .char_limit(if remove || expand { 31_000 } else { 32_000 }),
        )
        .has_focus();
    fields.insert("prompt".into(), json!(prompt));
    let mut consent = fields.get("consent").and_then(Value::as_bool).unwrap_or(false);
    ui.checkbox(&mut consent, tl!("Send the visible image and selection mask to this provider"));
    fields.insert("consent".into(), json!(consent));
    if expand {
        ui.small(tl!("API charges may apply. Canvas expansion and the new layer form one undo step."));
    } else {
        ui.small(tl!("API charges may apply. The result becomes a new layer within your selection."));
    }
    ui.small(tl!("Images up to 2048 × 2048. Cancellation discards results; remote charges may still apply."));
    prompt_focused || settings_opened || geometry_focused
}

fn expansion_body(app: &PhotocraftApp, ui: &mut egui::Ui, fields: &mut Map<String, Value>) -> bool {
    let Some(st) = app.session.active() else { return false };
    let size = st.doc.size;
    ui.label(tl!("The original image is not scaled. Only new canvas areas are generated."));
    let banner = photocraft_engine::generative_expand_cmds::banner_size(size);
    if ui.add_enabled(banner.is_some(), egui::Button::new(tl!("16:9 Banner"))).clicked()
        && let Some(target) = banner
    {
        fields.insert("width".into(), json!(target.width));
        fields.insert("height".into(), json!(target.height));
    }
    if banner.is_none() {
        ui.small(tl!("A 16:9 canvas at the original image size exceeds the 2048-pixel limit."));
    }
    let mut editing_dimension = false;
    ui.horizontal(|ui| {
        for (key, label, fallback) in [("width", tl!("Width:"), size.width), ("height", tl!("Height:"), size.height)] {
            let label = ui.label(label);
            let mut value = fields.get(key).and_then(Value::as_u64).unwrap_or(u64::from(fallback));
            let response = ui.add(egui::DragValue::new(&mut value).range(1..=u64::from(photocraft_ai::MAX_EDGE)).suffix(" px")).labelled_by(label.id);
            editing_dimension |= response.has_focus() || response.lost_focus();
            if response.changed() {
                fields.insert(key.into(), json!(value));
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label(tl!("Anchor:"));
        let anchor = fields.get("anchor").and_then(Value::as_str).unwrap_or("center").to_string();
        let w = fields.get("width").and_then(Value::as_f64).unwrap_or(f64::from(size.width));
        let h = fields.get("height").and_then(Value::as_f64).unwrap_or(f64::from(size.height));
        if let Some(anchor) = crate::sizing::anchor_grid(ui, &anchor, w - f64::from(size.width), h - f64::from(size.height)) {
            fields.insert("anchor".into(), json!(anchor));
        }
    });
    if photocraft_engine::generative_expand_cmds::geometry(size, &json!(fields)).is_err() {
        ui.small(tl!("Increase at least one dimension without reducing the original image."));
    }
    editing_dimension
}

fn confirm_inner(app: &mut PhotocraftApp, fields: &Map<String, Value>) -> Result<Value, String> {
    let command = command(fields).ok_or("unknown generative operation")?;
    let st = app.session.active().ok_or("no document open")?;
    if fields.get("document").and_then(Value::as_u64) != Some(st.doc.id.0) || fields.get("revision").and_then(Value::as_u64) != Some(st.revision) {
        return Err("The document changed. Open the generative dialog again before uploading.".into());
    }
    let mut params = json!({"prompt": fields.get("prompt"), "consent": fields.get("consent")});
    if command == photocraft_engine::generative_expand_cmds::EXPAND {
        for key in ["width", "height", "anchor"] {
            params[key] = json!(fields.get(key));
        }
    }
    // Remember submitted text, including a failed/cancelled provider request. Drafts and
    // upload consent stay transient; only this allow-listed field crosses into preferences.
    if app.session.prefs().integrations.remember_dialog_texts && can_confirm(app, fields) {
        let value = json!({"prompt": fields.get("prompt")});
        if app.session.prefs().dialogs.get(command) != Some(&value) {
            app.session.prefs.edit(|p| p.dialogs.insert(command.into(), value));
        }
    }
    app.run(command, params)
}

pub fn confirm(app: &mut PhotocraftApp, fields: &Map<String, Value>) -> Result<Value, String> {
    let result = confirm_inner(app, fields);
    if let Err(error) = &result {
        app.ui.status.clone_from(error);
        app.ui.status_error = true;
    }
    result
}

pub fn can_confirm(app: &PhotocraftApp, fields: &Map<String, Value>) -> bool {
    let prompt_valid = match command(fields) {
        Some(photocraft_engine::generative_cmds::REMOVE) => {
            fields.get("prompt").and_then(Value::as_str).is_some_and(|s| valid_prompt(photocraft_engine::generative_cmds::REMOVE, s))
        }
        Some(photocraft_engine::generative_expand_cmds::EXPAND) => {
            fields.get("prompt").and_then(Value::as_str).is_some_and(|s| valid_prompt(photocraft_engine::generative_expand_cmds::EXPAND, s))
                && app.session.active().is_some_and(|st| photocraft_engine::generative_expand_cmds::geometry(st.doc.size, &json!(fields)).is_ok())
        }
        Some(photocraft_engine::generative_cmds::FILL) => {
            fields.get("prompt").and_then(Value::as_str).is_some_and(|s| valid_prompt(photocraft_engine::generative_cmds::FILL, s))
        }
        _ => false,
    };
    prompt_valid && fields.get("consent").and_then(Value::as_bool) == Some(true) && app.session.generative_backend.is_some()
}

pub fn confirm_label(fields: &Map<String, Value>) -> &'static str {
    match command(fields) {
        Some(photocraft_engine::generative_cmds::REMOVE) => tl!("Remove"),
        Some(photocraft_engine::generative_expand_cmds::EXPAND) => tl!("Extend"),
        _ => tl!("Generate"),
    }
}

#[cfg(test)]
mod remember_tests;

#[cfg(test)]
mod tests {
    use super::*;
    struct Echo;
    impl photocraft_ai::Backend for Echo {
        fn id(&self) -> &'static str {
            "test"
        }
        fn generate(&self, request: &photocraft_ai::Request, _: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<photocraft_ai::Response> {
            request.validate()?;
            Ok(photocraft_ai::Response { quality: None, version: photocraft_ai::VERSION, image: request.source.clone() })
        }
    }
    #[test]
    fn expansion_dialog_presets_banner_without_selection_and_confirms_an_atomic_edit() {
        struct Outpaint;
        impl photocraft_ai::Backend for Outpaint {
            fn id(&self) -> &'static str {
                "expand-test"
            }
            fn generate(&self, request: &photocraft_ai::Request, _: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<photocraft_ai::Response> {
                request.validate()?;
                assert_eq!(request.operation, photocraft_ai::Operation::Expand);
                let pixels = [255, 0, 0, 255].repeat(request.source.width as usize * request.source.height as usize);
                let image =
                    photocraft_codecs::Image::from_u8(request.source.width, request.source.height, photocraft_codecs::ChannelLayout::Rgba, pixels).unwrap();
                let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap();
                Ok(photocraft_ai::Response {
                    quality: None,
                    version: photocraft_ai::VERSION,
                    image: photocraft_ai::PngImage::from_bytes(image.width(), image.height(), &png)?,
                })
            }
        }
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        let ctx = egui::Context::default();
        let result = crate::menus::invoke(&mut app, &ctx, photocraft_engine::generative_expand_cmds::EXPAND, json!({})).unwrap();
        let id = result["dialog"].as_u64().unwrap();
        let mut fields = app.ui.dialog_mut(id).unwrap().fields.clone();
        assert_eq!(fields["__label"], "Generative Expand");
        assert_eq!((fields["width"].as_u64(), fields["height"].as_u64()), (Some(16), Some(9)));
        assert_eq!(fields["anchor"], "center");
        assert_eq!(confirm_label(&fields), tl!("Extend"));
        assert!(!can_confirm(&app, &fields));
        fields.insert("consent".into(), json!(true));
        assert!(!can_confirm(&app, &fields));
        app.session.generative_backend = Some(std::sync::Arc::new(Outpaint));
        assert!(can_confirm(&app, &fields));
        let mut invalid = fields.clone();
        invalid.insert("width".into(), json!(7));
        assert!(!can_confirm(&app, &invalid));
        assert!(confirm(&mut app, &invalid).is_err());
        assert_eq!(app.session.active().unwrap().doc.size, photocraft_doc::Size::new(8, 8));
        app.ui.dialog_mut(id).unwrap().fields = fields;
        let result = crate::dialogs::confirm(&mut app, id).unwrap();
        assert_eq!(result["operation"], "expand");
        assert_eq!(app.session.active().unwrap().doc.size, photocraft_doc::Size::new(16, 9));
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 2);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.active().unwrap().doc.size, photocraft_doc::Size::new(8, 8));
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 1);
    }
    #[test]
    fn large_square_uses_custom_size_and_stale_expansion_cannot_upload() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        app.run("file.new", json!({"width": 1200, "height": 1200})).unwrap();
        let id = open(&mut app, photocraft_engine::generative_expand_cmds::EXPAND).unwrap();
        let fields = app.ui.dialog_mut(id).unwrap().fields.clone();
        assert_eq!(fields["width"], 1800);
        assert_eq!(fields["height"], 1200);
        assert!(photocraft_engine::generative_expand_cmds::geometry(app.session.active().unwrap().doc.size, &json!(fields)).is_ok());
        app.run("image.canvasSize", json!({"width": 1210, "height": 1200})).unwrap();
        assert!(confirm(&mut app, &fields).unwrap_err().contains("document changed"));
    }
    #[test]
    fn removal_dialog_needs_consent_but_no_description_and_dispatches_remove() {
        struct Remove;
        impl photocraft_ai::Backend for Remove {
            fn id(&self) -> &'static str {
                "test"
            }
            fn generate(&self, request: &photocraft_ai::Request, _: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<photocraft_ai::Response> {
                request.validate()?;
                assert_eq!(request.operation, photocraft_ai::Operation::Remove);
                assert!(request.prompt.contains("Reconstruct the background"));
                Ok(photocraft_ai::Response { quality: None, version: photocraft_ai::VERSION, image: request.source.clone() })
            }
        }
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        app.run("select.all", json!({})).unwrap();
        let ctx = egui::Context::default();
        let result = crate::menus::invoke(&mut app, &ctx, photocraft_engine::generative_cmds::REMOVE, json!({})).unwrap();
        let id = result["dialog"].as_u64().unwrap();
        let mut fields = app.ui.dialog_mut(id).unwrap().fields.clone();
        assert_eq!(fields["__label"], "Generative Remove");
        assert_eq!(fields["prompt"], "");
        assert_eq!(confirm_label(&fields), tl!("Remove"));
        assert!(!can_confirm(&app, &fields));
        fields.insert("consent".into(), json!(true));
        assert!(!can_confirm(&app, &fields));
        app.session.generative_backend = Some(std::sync::Arc::new(Remove));
        assert!(can_confirm(&app, &fields));
        let mut invalid = fields.clone();
        invalid.insert("__command".into(), json!("file.close"));
        assert!(!can_confirm(&app, &invalid));
        assert!(confirm(&mut app, &invalid).is_err());
        assert!(app.session.active().is_some());
        invalid.insert("__command".into(), json!(photocraft_engine::generative_cmds::FILL));
        assert!(!can_confirm(&app, &invalid));
        app.ui.dialog_mut(id).unwrap().fields = fields;
        let result = crate::dialogs::confirm(&mut app, id).unwrap();
        assert_eq!(result["operation"], "remove");
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 2);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 1);
    }
    #[test]
    fn enter_in_expansion_dimensions_does_not_start_an_upload() {
        use egui_kittest::{Harness, kittest::Queryable};
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        app.session.generative_backend = Some(std::sync::Arc::new(Echo));
        let id = open(&mut app, photocraft_engine::generative_expand_cmds::EXPAND).unwrap();
        app.ui.dialog_mut(id).unwrap().fields.insert("consent".into(), json!(true));
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            app
        });
        h.run_steps(4);
        for label in [tl!("Width:"), tl!("Height:")] {
            h.get_by_label(label).click();
            h.run_steps(4);
            h.key_press(egui::Key::Enter);
            h.run_steps(4);
            assert!(h.state().ui.dialogs.iter().any(|dialog| dialog.id == id));
            assert_eq!(h.state().session.active().unwrap().doc.size, photocraft_doc::Size::new(8, 8));
            assert_eq!(h.state().session.active().unwrap().doc.layers.len(), 1);
            assert!(!h.state().ui.status_error, "dimension confirmation must not call the provider");
        }
    }
    #[test]
    fn enter_in_prompt_does_not_start_an_upload() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        app.run("select.all", json!({})).unwrap();
        app.session.generative_backend = Some(std::sync::Arc::new(Echo));
        let id = open(&mut app, photocraft_engine::generative_cmds::FILL).unwrap();
        let fields = &mut app.ui.dialog_mut(id).unwrap().fields;
        fields.insert("prompt".into(), json!("red vase"));
        fields.insert("consent".into(), json!(true));
        let ctx = egui::Context::default();
        PhotocraftApp::setup_context(&ctx, Default::default());
        ctx.run_ui(Default::default(), |ui| crate::dialogs::show(&mut app, ui.ctx())).textures_delta.clear();
        ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("generative-prompt")));
        let input = egui::RawInput {
            events: vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| crate::dialogs::show(&mut app, ui.ctx())).textures_delta.clear();
        assert!(app.ui.dialog_mut(id).unwrap().fields["prompt"].as_str().unwrap().contains('\n'));
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 1);
    }
    #[test]
    fn enter_in_settings_does_not_submit_the_underlying_fill_dialog() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        app.run("select.all", json!({})).unwrap();
        app.session.generative_backend = Some(std::sync::Arc::new(Echo));
        let fill = open(&mut app, photocraft_engine::generative_cmds::FILL).unwrap();
        app.ui.dialog_mut(fill).unwrap().fields.insert("prompt".into(), json!("vase"));
        app.ui.dialog_mut(fill).unwrap().fields.insert("consent".into(), json!(true));
        let settings = crate::prefs_ui::open_preferences(&mut app, "integrations");
        let ctx = egui::Context::default();
        PhotocraftApp::setup_context(&ctx, Default::default());
        for _ in 0..2 {
            ctx.run_ui(Default::default(), |ui| crate::dialogs::show(&mut app, ui.ctx())).textures_delta.clear();
        }
        let input = egui::RawInput {
            events: vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| crate::dialogs::show(&mut app, ui.ctx())).textures_delta.clear();
        assert!(app.ui.dialog_mut(settings).is_none());
        assert!(app.ui.dialog_mut(fill).is_some());
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 1);
    }
    #[test]
    fn integrated_dialog_configures_host_and_runs_the_engine_command() {
        let services = crate::Services {
            generative_backend: Some(Box::new(|key| {
                assert_eq!(key, "private-test-key");
                Ok(std::sync::Arc::new(Echo))
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(Default::default(), services);
        let settings = crate::prefs_ui::open_preferences(&mut app, "integrations");
        app.ui.generative_key.0 = "private-test-key".into();
        assert!(crate::prefs_ui::preferences_changed(&app, &app.ui.dialogs.last().unwrap().fields), "key-only edits enable Apply");
        app.ui.dialog_mut(settings).unwrap().fields.get_mut("values").unwrap()["integrations"]["imageModel"] = json!("test-model");
        assert!(crate::prefs_ui::preferences_changed(&app, &app.ui.dialogs.last().unwrap().fields));
        assert!(!serde_json::to_string(&app.ui).unwrap().contains("private-test-key"));
        assert!(!format!("{:?}", app.ui).contains("private-test-key"));
        crate::prefs_ui::apply(&mut app, settings).unwrap();
        assert!(app.ui.dialog_mut(settings).is_some());
        assert!(!crate::prefs_ui::preferences_changed(&app, &app.ui.dialogs.last().unwrap().fields));
        assert!(app.ui.generative_key.0.is_empty());
        assert_eq!(app.session.prefs().integrations.image_model, "test-model");
        let saved = app.session.prefs_to_json();
        assert!(!saved.contains("private-test-key"));
        let mut restored = photocraft_engine::Session::new();
        restored.load_prefs_json(&saved).unwrap();
        assert_eq!(restored.prefs().integrations.image_model, "test-model");
        crate::dialogs::confirm(&mut app, settings).unwrap();
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        app.run("select.all", json!({})).unwrap();
        let ctx = egui::Context::default();
        let opened = crate::menus::invoke(&mut app, &ctx, photocraft_engine::generative_cmds::FILL, json!({})).unwrap();
        let id = opened["dialog"].as_u64().unwrap();
        let fields = app.ui.dialog_mut(id).unwrap().fields.clone();
        assert!(owns(&fields));
        assert!(!can_confirm(&app, &fields));
        assert!(!fields.contains_key("model"));
        let fields = &mut app.ui.dialog_mut(id).unwrap().fields;
        fields.insert("prompt".into(), json!("red vase"));
        fields.insert("consent".into(), json!(true));
        let before = app.session.active().unwrap().doc.layers.len();
        let pending = app.ui.dialog_mut(id).unwrap().fields.clone();
        assert!(can_confirm(&app, &pending));
        let result = crate::dialogs::confirm(&mut app, id).unwrap();
        assert!(result.get("layer").is_some());
        assert_eq!(app.session.active().unwrap().doc.layers.len(), before + 1);
        assert!(app.ui.generative_key.0.is_empty());
        assert!(!serde_json::to_string(&app.session.journal).unwrap().contains("private-test-key"));
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.active().unwrap().doc.layers.len(), before);
    }
    #[test]
    fn dialog_is_inspectable_without_credentials_and_rejects_stale_document() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        assert!(open(&mut app, photocraft_engine::generative_cmds::FILL).is_err());
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        app.run("select.all", json!({})).unwrap();
        let id = open(&mut app, photocraft_engine::generative_cmds::FILL).unwrap();
        let fields = app.ui.dialog_mut(id).unwrap().fields.clone();
        app.run("select.deselect", json!({})).unwrap();
        assert!(confirm(&mut app, &fields).unwrap_err().contains("document changed"));
        assert!(app.ui.generative_key.0.is_empty());
    }
    #[test]
    fn cancelled_settings_discard_draft_key_and_model() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        let settings = crate::prefs_ui::open_preferences(&mut app, "integrations");
        app.ui.generative_key.0 = "discard-me".into();
        app.ui.dialog_mut(settings).unwrap().fields.get_mut("values").unwrap()["integrations"]["imageModel"] = json!("other-model");
        let ctx = egui::Context::default();
        let (req, _) = crate::control::ControlRequest::new("ui.dialog.cancel", json!({"dialog": settings}));
        crate::control::handle(&mut app, &ctx, &req);
        assert!(app.ui.generative_key.0.is_empty());
        assert_eq!(app.session.prefs().integrations.image_model, photocraft_ai::DEFAULT_MODEL);
        assert!(app.session.generative_backend.is_none());
    }
    #[test]
    fn invalid_settings_leave_provider_and_preferences_unchanged() {
        let services = crate::Services {
            generative_backend: Some(Box::new(|key| if key == "valid-key" { Ok(std::sync::Arc::new(Echo)) } else { Err("invalid key".into()) })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(Default::default(), services);
        let settings = crate::prefs_ui::open_preferences(&mut app, "integrations");
        app.ui.generative_key.0 = "valid-key".into();
        app.ui.dialog_mut(settings).unwrap().fields.get_mut("values").unwrap()["integrations"]["imageModel"] = json!("");
        assert!(crate::dialogs::confirm(&mut app, settings).is_err());
        assert!(app.ui.dialog_mut(settings).is_some());
        assert!(app.session.generative_backend.is_none());
        assert_eq!(app.session.prefs().integrations.image_model, photocraft_ai::DEFAULT_MODEL);
        assert_eq!(app.ui.generative_key.0, "valid-key");
        app.ui.dialog_mut(settings).unwrap().fields.get_mut("values").unwrap()["integrations"]["imageModel"] = json!("new-model");
        app.ui.generative_key.0 = "bad-key".into();
        assert!(crate::prefs_ui::apply(&mut app, settings).is_err());
        assert!(app.ui.status_error);
        assert!(app.session.generative_backend.is_none());
        assert_eq!(app.session.prefs().integrations.image_model, photocraft_ai::DEFAULT_MODEL);
        app.ui.generative_key.0 = "valid-key".into();
        crate::dialogs::confirm(&mut app, settings).unwrap();
        assert!(app.session.generative_backend.is_some());
        assert_eq!(app.session.prefs().integrations.image_model, "new-model");
        assert!(app.ui.generative_key.0.is_empty());
    }
}
