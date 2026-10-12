//! Explicit diagnostic viewing/export through host services. Never uses ambient filesystem access.
use crate::{PhotocraftApp, state::DialogKind};
use photocraft_engine::generative_pipeline::Diagnostic;
use serde_json::{Map, Value, json};
use std::sync::PoisonError;

pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key("__generativeDiagnostics")
}
pub fn open(app: &mut PhotocraftApp) -> u64 {
    app.ui.open_dialog(
        DialogKind::Command,
        json!({"__generativeDiagnostics":true,"__label":"Generation diagnostics…","stage":"source-document"}).as_object().cloned().unwrap_or_default(),
    )
}
fn bytes(app: &PhotocraftApp) -> Result<Vec<u8>, String> {
    let guard = app.session.generative_diagnostic.lock().unwrap_or_else(PoisonError::into_inner);
    let capture = guard.as_ref().ok_or_else(|| tl!("No diagnostic capture. Enable diagnostics before generating, or import a capture.").to_string())?;
    capture.validate().map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(capture).map_err(|e| e.to_string())?;
    if bytes.len() > Diagnostic::MAX_BYTES {
        return Err("diagnostic export exceeds the size limit".into());
    }
    Ok(bytes)
}
pub fn run(app: &mut PhotocraftApp, id: &str, p: &Value) -> Result<Value, String> {
    let result = run_inner(app, id, p);
    action_notice(app, &result);
    result
}
// Status-bar messages sit behind the modal. Keep action feedback in its inspectable fields,
// including file-picker completions that arrive on a later frame.
fn action_notice(app: &mut PhotocraftApp, result: &Result<Value, String>) {
    let notice = match result {
        Ok(value) => value.get("path").and_then(Value::as_str).map(|path| (crate::i18n::fmt(tl!("Diagnostics saved to {path}."), &[("path", path)]), false)),
        Err(error) if error != crate::file_dialog::CANCELLED => Some((error.clone(), true)),
        _ => None,
    };
    for dialog in app.ui.dialogs.iter_mut().filter(|d| owns(&d.fields)) {
        dialog.fields.remove("__notice");
        dialog.fields.remove("__noticeError");
        if let Some((message, error)) = &notice {
            dialog.fields.insert("__notice".into(), json!(message));
            dialog.fields.insert("__noticeError".into(), json!(error));
        }
    }
}
fn watch_picker(app: &mut PhotocraftApp, result: Result<Value, String>) -> Result<Value, String> {
    if result.is_ok() {
        app.after_file_dialog(|app, result| {
            action_notice(app, &result);
            result
        });
    }
    result
}
fn run_inner(app: &mut PhotocraftApp, id: &str, p: &Value) -> Result<Value, String> {
    match id {
        "ai.diagnostics.show" => Ok(json!({"dialog":open(app)})),
        "ai.diagnostics.export" => {
            let content = bytes(app)?;
            if let Some(path) = p.get("path").and_then(Value::as_str) {
                if app.automation_input {
                    app.services.automation_write.as_mut().ok_or("automation write authority is not configured")?(path, &content)?;
                } else {
                    app.services.write.as_mut().ok_or("no writer configured")?(path, &content)?;
                }
                Ok(json!({"path":path}))
            } else if app.automation_input {
                Err("automation export requires an explicit path".into())
            } else {
                if app.services.file_dialog.is_none() {
                    return Err(tl!("Diagnostic file picker is unavailable.").into());
                }
                let result = app.pick_save("generation-diagnostics.json", move |app, path| {
                    app.services.write.as_mut().ok_or("no writer configured")?(&path, &content)?;
                    Ok(json!({"path":path}))
                });
                watch_picker(app, result)
            }
        }
        "ai.diagnostics.import" => {
            if let Some(path) = p.get("path").and_then(Value::as_str) {
                if !app.automation_input {
                    return Err("use the diagnostic file picker".into());
                }
                let (_, bytes) = app.services.automation_read.as_mut().ok_or("automation read authority is not configured")?(path)?;
                import(app, &bytes)
            } else if app.automation_input {
                Err("automation import requires an explicit path".into())
            } else {
                if app.services.file_dialog.is_none() {
                    return Err(tl!("Diagnostic file picker is unavailable.").into());
                }
                let result = app.pick_file_bytes(|app, _, bytes| import(app, &bytes));
                watch_picker(app, result)
            }
        }
        _ => Err("unknown diagnostic UI command".into()),
    }
}
fn import(app: &mut PhotocraftApp, bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > Diagnostic::MAX_BYTES {
        return Err("diagnostic import exceeds the size limit".into());
    }
    let mut capture: Diagnostic = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    capture.validate().map_err(|e| e.to_string())?;
    capture.retag();
    *app.session.generative_diagnostic.lock().unwrap_or_else(PoisonError::into_inner) = Some(capture);
    Ok(json!({"imported":true}))
}
pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, fields: &mut Map<String, Value>) {
    let mut stage = fields.get("stage").and_then(Value::as_str).unwrap_or("source-document").to_string();
    let mut replay = false;
    let mut export = false;
    let mut import_file = false;
    let mut clear = false;
    let (has_capture, has_response) = {
        let guard = app.session.generative_diagnostic.lock().unwrap_or_else(PoisonError::into_inner);
        (guard.is_some(), guard.as_ref().is_some_and(|c| c.response.is_some()))
    };
    let picking = app.file_dialog_open();
    ui.horizontal_wrapped(|ui| {
        export = ui.add_enabled(has_capture && !picking, egui::Button::new(tl!("Export diagnostics…"))).clicked();
        import_file = ui.add_enabled(!picking, egui::Button::new(tl!("Import diagnostics…"))).clicked();
        replay = ui.add_enabled(has_response && !picking, egui::Button::new(tl!("Replay offline"))).clicked();
        clear = ui.add_enabled(has_capture && !picking, egui::Button::new(tl!("Clear"))).clicked();
    });
    ui.small(tl!("Diagnostics contain images and prompts. Nothing is exported automatically."));
    if let Some(message) = fields.get("__notice").and_then(Value::as_str) {
        let t = crate::theme::Tokens::get(ui.ctx());
        let color = if fields.get("__noticeError").and_then(Value::as_bool) == Some(true) { t.danger } else { t.text };
        ui.label(egui::RichText::new(message).color(color));
    }
    let [checker_light, checker_dark] = app.session.prefs().transparency_and_gamut.colors();
    {
        let guard = app.session.generative_diagnostic.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(capture) = guard.as_ref() {
            ui.label(format!(
                "{} · {} · {} × {} → {} × {} · {} ms",
                capture.request.model,
                capture.request.options.quality.name(),
                capture.plan.document[0],
                capture.plan.document[1],
                capture.plan.working[0],
                capture.plan.working[1],
                capture.elapsed_ms
            ));
            if let Some(response) = &capture.response {
                ui.label(format!(
                    "{}: {} → {} · {} × {}",
                    tl!("Generation quality"),
                    capture.request.options.quality.name(),
                    response.quality.map(|q| q.name()).unwrap_or("?"),
                    response.image.width,
                    response.image.height
                ));
            }
            if let Some(report) = &capture.alignment {
                ui.label(format!("{}: {} ({}, {})", tl!("Alignment"), status_label(&report.status), report.translation[0], report.translation[1]));
            }
            if let Some(error) = &capture.error {
                ui.label(error);
            }
            ui.label(if capture.applied { tl!("Result applied") } else { tl!("Preview only") });
            let names = [
                ("source-document", tl!("Document source")),
                ("transmitted-source", tl!("Transmitted source")),
                ("generation-mask", tl!("Generation mask (alpha)")),
                ("raw-response", tl!("Unmodified response")),
                ("mapped-result", tl!("Mapped result")),
                ("insertion-mask", tl!("Insertion mask (alpha)")),
                ("final-result", tl!("Final result")),
            ];
            let choices: Vec<(String, &str)> =
                names.iter().filter(|(key, _)| image(capture, key).is_some()).map(|(key, label)| (key.to_string(), *label)).collect();
            crate::widgets::dropdown(ui, "diagnostic-stage", &mut stage, &choices, 280.0);
            if stage.ends_with("mask") {
                ui.small(tl!("Transparent areas are editable; opaque areas are protected."));
            }
            if let Some(image) = image(capture, &stage) {
                let key = format!("{}:{}:{}:{:?}", image.base64.as_ptr() as usize, capture.capture_id, stage, [checker_light, checker_dark]);
                let id = egui::Id::new("generative-diagnostic-texture");
                let cached: Option<(String, egui::TextureHandle)> = ui.ctx().data(|m| m.get_temp(id));
                let texture = if let Some((cached_key, texture)) = cached
                    && cached_key == key
                {
                    Some(texture)
                } else {
                    match photocraft_engine::generative_pipeline::pixels(image) {
                        Ok(px) => {
                            // Bake a checkerboard into this preview only, making transparent mask areas explicit.
                            let colors: Vec<egui::Color32> = px
                                .iter()
                                .enumerate()
                                .map(|(i, p)| {
                                    let background = if ((i % image.width as usize) / 8 + (i / image.width as usize) / 8).is_multiple_of(2) {
                                        checker_light
                                    } else {
                                        checker_dark
                                    }
                                    .map(|v| f32::from(v) / 255.);
                                    egui::Color32::from_rgb(
                                        ((p[0] * p[3] + background[0] * (1. - p[3])) * 255.) as u8,
                                        ((p[1] * p[3] + background[1] * (1. - p[3])) * 255.) as u8,
                                        ((p[2] * p[3] + background[2] * (1. - p[3])) * 255.) as u8,
                                    )
                                })
                                .collect();
                            let tex = ui.ctx().load_texture(
                                "generation-diagnostics",
                                egui::ColorImage::new([image.width as usize, image.height as usize], colors),
                                egui::TextureOptions::NEAREST,
                            );
                            ui.ctx().data_mut(|m| m.insert_temp(id, (key, tex.clone())));
                            Some(tex)
                        }
                        Err(e) => {
                            ui.label(e.to_string());
                            None
                        }
                    }
                };
                if let Some(texture) = texture {
                    ui.add(egui::Image::new(&texture).max_size(egui::vec2(ui.available_width().min(600.), 320.)));
                }
            }
        } else {
            ui.label(tl!("No diagnostic capture. Enable diagnostics before generating, or import a capture."));
            ui.small(tl!("Apply the diagnostic setting before generating. Earlier attempts were not retained."));
        }
    }
    fields.insert("stage".into(), json!(stage));
    for (clicked, id) in [(export, "ai.diagnostics.export"), (import_file, "ai.diagnostics.import"), (replay, "ai.replay"), (clear, "ai.diagnostics")] {
        if clicked {
            let params = if clear && id == "ai.diagnostics" {
                json!({"clear":true})
            } else if id == "ai.replay" {
                json!({"local":app.session.prefs().integrations.image_processing})
            } else {
                json!({})
            };
            let result = app.run(id, params);
            // Engine replay/clear commands also need visible feedback; shell file commands
            // arrange their own later callback for asynchronous picker errors/success.
            action_notice(app, &result);
            if let Some(dialog) = app.ui.dialogs.iter().find(|d| owns(&d.fields)) {
                for key in ["__notice", "__noticeError"] {
                    fields.remove(key);
                    if let Some(value) = dialog.fields.get(key) {
                        fields.insert(key.into(), value.clone());
                    }
                }
            }
        }
    }
}
fn image<'a>(capture: &'a Diagnostic, stage: &str) -> Option<&'a photocraft_ai::PngImage> {
    match stage {
        "transmitted-source" => Some(&capture.request.source),
        "generation-mask" => Some(&capture.request.mask),
        "raw-response" => capture.response.as_ref().map(|r| &r.image),
        _ => capture.stages.get(stage),
    }
}
fn status_label(status: &str) -> &str {
    match status {
        "plausible" => tl!("Plausible"),
        "corrected-translation" => tl!("Translation corrected"),
        "translation-detected" => tl!("Translation requires correction"),
        "geometric-deviation" => tl!("Geometric deviation"),
        "insufficient-context" => tl!("Insufficient context"),
        _ => tl!("Disabled"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Echo;
    impl photocraft_ai::Backend for Echo {
        fn id(&self) -> &'static str {
            "offline"
        }
        fn generate(&self, r: &photocraft_ai::Request, _: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<photocraft_ai::Response> {
            Ok(photocraft_ai::Response { quality: Some(r.options.quality), version: photocraft_ai::VERSION, image: r.source.clone() })
        }
    }
    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        app.run("file.new", json!({"width":32,"height":32})).unwrap();
        app.run("select.all", json!({})).unwrap();
        app.session.generative_backend = Some(std::sync::Arc::new(Echo));
        app.session.edit_prefs(|p| p.integrations.image_processing.diagnostics = true);
        app.run("edit.generativeFill", json!({"prompt":"synthetic","consent":true})).unwrap();
        app
    }
    #[test]
    fn export_errors_are_regular_and_automation_uses_only_scoped_writer() {
        let mut app = app();
        let content = bytes(&app).unwrap();
        assert!(!String::from_utf8_lossy(&content).contains("api_key"));
        app.services.write = Some(Box::new(|_, _| Err("disk full".into())));
        assert_eq!(app.run("ai.diagnostics.export", json!({"path":"unused.json"})).unwrap_err(), "disk full");
        app.automation_input = true;
        assert!(app.run("ai.diagnostics.export", json!({"path":"unused.json"})).unwrap_err().contains("authority"));
        app.services.automation_write = Some(Box::new(|path, bytes| {
            assert_eq!(path, "allowed/capture.json");
            assert!(bytes.starts_with(b"{"));
            Ok(())
        }));
        app.run("ai.diagnostics.export", json!({"path":"allowed/capture.json"})).unwrap();
        assert!(app.run("ai.diagnostics.export", json!({})).is_err());
        let before = app.session.active().unwrap().doc.clone();
        import(&mut app, &content).unwrap();
        app.session.generative_backend = None;
        app.run("ai.replay", json!({})).unwrap();
        assert_eq!(*before, *app.session.active().unwrap().doc);
        assert!(import(&mut app, b"{}").is_err());
    }
    #[test]
    fn settings_cancel_preserves_api_local_and_text_preferences() {
        let mut app = app();
        let before = app.session.prefs().clone();
        let id = crate::prefs_ui::open_preferences(&mut app, "integrations");
        let dialog = app.ui.dialog_mut(id).unwrap();
        dialog.fields.get_mut("values").unwrap()["integrations"]["imageOptions"]["quality"] = json!("xhigh");
        dialog.fields.get_mut("values").unwrap()["integrations"]["imageProcessing"]["transitionWidth"] = json!(24);
        app.ui.close_dialog(id);
        assert_eq!(app.session.prefs(), &before);
    }
    #[test]
    fn settings_apply_updates_nested_typed_values_together() {
        let mut app = app();
        let id = crate::prefs_ui::open_preferences(&mut app, "integrations");
        let dialog = app.ui.dialog_mut(id).unwrap();
        dialog.fields.get_mut("values").unwrap()["integrations"]["imageOptions"]["quality"] = json!("xhigh");
        dialog.fields.get_mut("values").unwrap()["integrations"]["imageProcessing"]["maxTranslation"] = json!(3);
        crate::prefs_ui::apply(&mut app, id).unwrap();
        assert_eq!(app.session.prefs().integrations.image_options.quality, photocraft_ai::Quality::Xhigh);
        assert_eq!(app.session.prefs().integrations.image_processing.max_translation, 3);
    }
    #[test]
    fn interactive_export_opens_picker_writes_chosen_path_and_shows_feedback() {
        use crate::file_dialog::FileDialogAnswer;
        use egui_kittest::{Harness, kittest::Queryable};
        let mut app = app();
        app.run("prefs.set", json!({"path":"interface.language","value":"en"})).unwrap();
        let (picker, asked) = crate::file_dialog::fake(vec![Some(FileDialogAnswer::SaveTo("chosen.json".into())), None]);
        app.services.file_dialog = Some(picker);
        let written = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let output = written.clone();
        app.services.write = Some(Box::new(move |path, bytes| {
            let capture: Diagnostic = serde_json::from_slice(bytes).unwrap();
            capture.validate().unwrap();
            output.borrow_mut().push(path.to_string());
            Ok(())
        }));
        let id = open(&mut app);
        let mut h = Harness::builder().with_size(egui::vec2(1024., 768.)).build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            app
        });
        h.run_steps(4);
        h.get_by_label("Export diagnostics…").click();
        h.run_steps(4);
        assert!(
            matches!(asked.borrow().first(), Some(crate::file_dialog::FileDialogRequest::Save { suggested }) if suggested.ends_with("generation-diagnostics.json"))
        );
        assert_eq!(*written.borrow(), ["chosen.json"]);
        assert_eq!(h.state().ui.dialogs.iter().find(|d| d.id == id).unwrap().fields["__noticeError"], false);
        h.get_by_label("Diagnostics saved to chosen.json.");
        h.get_by_label("Export diagnostics…").click();
        h.run_steps(4);
        assert_eq!(written.borrow().len(), 1, "cancel must not write or discard the capture");
        assert!(h.state().session.generative_diagnostic.lock().unwrap().is_some());
        assert!(!h.state().ui.dialogs.iter().find(|d| d.id == id).unwrap().fields.contains_key("__notice"));
    }
    #[test]
    fn asynchronous_export_failure_is_visible_inside_diagnostics_dialog() {
        use egui_kittest::{Harness, kittest::Queryable};
        let mut app = app();
        app.run("prefs.set", json!({"path":"interface.language","value":"en"})).unwrap();
        let (picker, _) = crate::file_dialog::fake(vec![Some(crate::file_dialog::FileDialogAnswer::SaveTo("chosen.json".into()))]);
        app.services.file_dialog = Some(picker);
        app.services.write = Some(Box::new(|_, _| Err("disk full".into())));
        let id = open(&mut app);
        app.run("ai.diagnostics.export", json!({})).unwrap();
        app.poll_file_dialog(&egui::Context::default(), None);
        assert_eq!(app.ui.dialog_mut(id).unwrap().fields["__noticeError"], true);
        let mut h = Harness::builder().with_size(egui::vec2(1024., 768.)).build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            app
        });
        h.run_steps(4);
        assert_eq!(h.query_all_by_label("disk full").count(), 2, "the error must appear in both the modal and the status bar");
        assert!(h.state().ui.status_error);
    }
    #[test]
    fn missing_capture_disables_export_and_explains_non_retained_attempts() {
        use egui_kittest::{
            Harness,
            kittest::{NodeT, Queryable},
        };
        let mut app = app();
        app.run("prefs.set", json!({"path":"interface.language","value":"en"})).unwrap();
        app.run("ai.diagnostics", json!({"clear":true})).unwrap();
        open(&mut app);
        let mut h = Harness::builder().with_size(egui::vec2(1024., 768.)).build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            app
        });
        h.run_steps(4);
        for label in ["Export diagnostics…", "Replay offline", "Clear"] {
            assert!(h.get_by_label(label).accesskit_node().is_disabled());
        }
        assert!(!h.get_by_label("Import diagnostics…").accesskit_node().is_disabled());
        h.get_by_label("Apply the diagnostic setting before generating. Earlier attempts were not retained.");
    }
}
