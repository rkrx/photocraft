//! Generative Fill and shared AI settings. Credentials stay outside inspectable JSON fields.
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

pub fn open(app: &mut PhotocraftApp) -> Result<u64, String> {
    let spec = photocraft_engine::commands::find(photocraft_engine::generative_cmds::FILL).ok_or("Generative Fill is unavailable")?;
    (spec.enabled)(&app.session)?;
    let st = app.session.active().ok_or("no document open")?;
    let fields = json!({"__generative": true, "__command": spec.id, "__label": "Generative Fill",
        "document": st.doc.id.0, "revision": st.revision,
        "prompt": "", "consent": false});
    let map = fields.as_object().cloned().ok_or("could not open Generative Fill")?;
    Ok(app.ui.open_dialog(DialogKind::Command, map))
}

/// Runtime credentials for Preferences › Integrations; the model uses normal preferences.
pub fn settings_body(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
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

/// Prepare a replacement without modifying the connected provider or exposing the key.
pub fn settings_backend(app: &PhotocraftApp) -> Result<Option<std::sync::Arc<dyn photocraft_ai::Backend>>, String> {
    let key = &app.ui.generative_key.0;
    if key.trim().is_empty() {
        return Ok(None);
    }
    let factory = app.services.generative_backend.as_ref().ok_or("no image provider in this build")?;
    factory(key).map(Some)
}

/// Whether Enter must stay in this form (prompt newline or opening settings).
pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, fields: &mut Map<String, Value>) -> bool {
    let backend = provider_name(app);
    ui.label(crate::i18n::fmt(tl!("Image provider: {provider}"), &[("provider", backend)]));
    ui.label(format!("{}: {}", tl!("Model"), app.session.prefs().integrations.image_model));
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
    ui.label(tl!("Describe what to change in the selection"));
    let prompt_focused = ui
        .add(egui::TextEdit::multiline(&mut prompt).id(egui::Id::new("generative-prompt")).desired_rows(4).desired_width(f32::INFINITY).char_limit(32_000))
        .has_focus();
    fields.insert("prompt".into(), json!(prompt));
    let mut consent = fields.get("consent").and_then(Value::as_bool).unwrap_or(false);
    ui.checkbox(&mut consent, tl!("Send the visible image and selection mask to this provider"));
    fields.insert("consent".into(), json!(consent));
    ui.small(tl!("API charges may apply. The result becomes a new layer within your selection."));
    ui.small(tl!("Images up to 2048 × 2048. Cancellation discards results; remote charges may still apply."));
    prompt_focused || settings_opened
}

fn confirm_inner(app: &mut PhotocraftApp, fields: &Map<String, Value>) -> Result<Value, String> {
    let st = app.session.active().ok_or("no document open")?;
    if fields.get("document").and_then(Value::as_u64) != Some(st.doc.id.0) || fields.get("revision").and_then(Value::as_u64) != Some(st.revision) {
        return Err("The document changed. Open Generative Fill again before uploading.".into());
    }
    app.run(
        photocraft_engine::generative_cmds::FILL,
        json!({
            "prompt": fields.get("prompt"), "consent": fields.get("consent")
        }),
    )
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
    fields.get("prompt").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty())
        && fields.get("consent").and_then(Value::as_bool) == Some(true)
        && app.session.generative_backend.is_some()
}

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
            Ok(photocraft_ai::Response { version: photocraft_ai::VERSION, image: request.source.clone() })
        }
    }
    #[test]
    fn enter_in_prompt_does_not_start_an_upload() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        app.run("select.all", json!({})).unwrap();
        app.session.generative_backend = Some(std::sync::Arc::new(Echo));
        let id = open(&mut app).unwrap();
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
        let fill = open(&mut app).unwrap();
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
        assert!(open(&mut app).is_err());
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        app.run("select.all", json!({})).unwrap();
        let id = open(&mut app).unwrap();
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
