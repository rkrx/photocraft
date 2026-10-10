use super::*;
use photocraft_engine::{
    generative_cmds::{FILL, REMOVE},
    generative_expand_cmds::EXPAND,
};
use std::sync::{Arc, Mutex};

struct Paint;
impl photocraft_ai::Backend for Paint {
    fn id(&self) -> &'static str {
        "test"
    }
    fn generate(&self, request: &photocraft_ai::Request, _: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<photocraft_ai::Response> {
        request.validate()?;
        let image = photocraft_codecs::Image::from_u8(
            request.source.width,
            request.source.height,
            photocraft_codecs::ChannelLayout::Rgba,
            [128, 128, 128, 255].repeat(request.source.width as usize * request.source.height as usize),
        )
        .unwrap();
        let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap();
        Ok(photocraft_ai::Response { version: photocraft_ai::VERSION, image: photocraft_ai::PngImage::from_bytes(image.width(), image.height(), &png)? })
    }
}

fn app(store: &Arc<Mutex<Option<String>>>) -> PhotocraftApp {
    let (read, write) = (store.clone(), store.clone());
    let services = crate::Services {
        load_prefs: Some(Box::new(move || read.lock().unwrap().clone())),
        save_prefs: Some(Box::new(move |text| {
            *write.lock().unwrap() = Some(text.into());
            Ok(())
        })),
        ..Default::default()
    };
    let mut app = PhotocraftApp::new(Default::default(), services);
    app.session.generative_backend = Some(Arc::new(Paint));
    app.run("file.new", json!({"width":8,"height":8})).unwrap();
    app.run("select.all", json!({})).unwrap();
    app
}

fn fields(app: &mut PhotocraftApp, command: &str, text: &str) -> Map<String, Value> {
    let id = open(app, command).unwrap();
    let fields = &mut app.ui.dialog_mut(id).unwrap().fields;
    fields.insert("prompt".into(), json!(text));
    fields.insert("consent".into(), json!(true));
    fields.clone()
}

#[test]
fn submitted_texts_are_separate_and_restore_after_restart_without_consent_or_geometry() {
    let store = Arc::default();
    let mut first = app(&store);
    for (command, text) in [(FILL, "A red vase\non the table"), (REMOVE, "Das Kabel – einschließlich Schatten"), (EXPAND, "背景を自然に広げる")] {
        let mut fields = fields(&mut first, command, text);
        fields.insert("apiKey".into(), json!("never-save-this"));
        confirm(&mut first, &fields).unwrap();
    }
    crate::prefs_ui::save_preferences(&mut first).unwrap();
    let saved = store.lock().unwrap().clone().unwrap();
    assert!(!saved.contains("never-save-this"));
    let value: Value = serde_json::from_str(&saved).unwrap();
    let mut restored = app(&store);
    for (command, text) in [(FILL, "A red vase\non the table"), (REMOVE, "Das Kabel – einschließlich Schatten"), (EXPAND, "背景を自然に広げる")] {
        assert_eq!(value["dialogs"][command], json!({"prompt": text}));
        let id = open(&mut restored, command).unwrap();
        let fields = &restored.ui.dialog_mut(id).unwrap().fields;
        assert_eq!(fields["prompt"], text);
        assert_eq!(fields["consent"], false);
        assert!(!fields.contains_key("apiKey"));
        assert!(!fields.contains_key("model"));
        if command == EXPAND {
            assert_eq!(fields["width"], 16);
            assert_eq!(fields["height"], 9);
        }
    }
    // An explicitly submitted empty optional text replaces the previous text.
    let empty = fields(&mut restored, REMOVE, "");
    confirm(&mut restored, &empty).unwrap();
    assert_eq!(last_prompt(&restored, REMOVE), "");
    assert_eq!(last_prompt(&restored, FILL), "A red vase\non the table");
}

#[test]
fn settings_cancel_keeps_texts_and_apply_off_clears_them_across_restarts() {
    let store = Arc::default();
    let mut current = app(&store);
    let submitted = fields(&mut current, FILL, "Remember me");
    confirm(&mut current, &submitted).unwrap();
    current.session.prefs.edit(|p| {
        p.dialogs.insert("edit.fill".into(), json!({"contents":"background"}));
    });
    let ctx = egui::Context::default();
    let settings = crate::prefs_ui::open_preferences(&mut current, "integrations");
    current.ui.dialog_mut(settings).unwrap().fields.get_mut("values").unwrap()["integrations"]["rememberDialogTexts"] = json!(false);
    let (request, _) = crate::control::ControlRequest::new("ui.dialog.cancel", json!({"dialog": settings}));
    crate::control::handle(&mut current, &ctx, &request);
    assert!(current.session.prefs().integrations.remember_dialog_texts);
    assert_eq!(last_prompt(&current, FILL), "Remember me");
    let settings = crate::prefs_ui::open_preferences(&mut current, "integrations");
    current.ui.dialog_mut(settings).unwrap().fields.get_mut("values").unwrap()["integrations"]["rememberDialogTexts"] = json!(false);
    crate::prefs_ui::apply(&mut current, settings).unwrap();
    assert!(!current.session.prefs().integrations.remember_dialog_texts);
    let ignored = fields(&mut current, FILL, "Do not remember me");
    confirm(&mut current, &ignored).unwrap();
    assert_eq!(last_prompt(&current, FILL), "");
    crate::prefs_ui::save_preferences(&mut current).unwrap();
    assert!(!store.lock().unwrap().as_ref().unwrap().contains("Remember me"));
    assert!(!store.lock().unwrap().as_ref().unwrap().contains("Do not remember me"));
    let mut restored = app(&store);
    assert!(!restored.session.prefs().integrations.remember_dialog_texts);
    assert_eq!(restored.session.prefs().dialogs["edit.fill"], json!({"contents":"background"}));
    restored.run("prefs.set", json!({"path":"integrations.rememberDialogTexts","value":true})).unwrap();
    assert_eq!(last_prompt(&restored, FILL), "");
}

#[test]
fn cancel_invalid_and_stale_dialogs_do_not_replace_text_but_provider_failure_keeps_it() {
    struct Fail;
    impl photocraft_ai::Backend for Fail {
        fn id(&self) -> &'static str {
            "fail"
        }
        fn generate(&self, _: &photocraft_ai::Request, _: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<photocraft_ai::Response> {
            Err(photocraft_ai::Error::Provider("synthetic failure".into()))
        }
    }
    let mut current = app(&Arc::default());
    current.session.generative_backend = Some(Arc::new(Fail));
    let submitted = fields(&mut current, FILL, "Keep this failed request");
    assert!(confirm(&mut current, &submitted).is_err());
    assert_eq!(last_prompt(&current, FILL), "Keep this failed request");
    let ctx = egui::Context::default();
    let draft = open(&mut current, FILL).unwrap();
    current.ui.dialog_mut(draft).unwrap().fields.insert("prompt".into(), json!("cancelled draft"));
    let (request, _) = crate::control::ControlRequest::new("ui.dialog.cancel", json!({"dialog": draft}));
    crate::control::handle(&mut current, &ctx, &request);
    assert_eq!(last_prompt(&current, FILL), "Keep this failed request");
    for bad in [json!(""), json!(7), json!("x".repeat(32_001))] {
        let mut invalid = submitted.clone();
        invalid.insert("prompt".into(), bad);
        assert!(confirm(&mut current, &invalid).is_err());
        assert_eq!(last_prompt(&current, FILL), "Keep this failed request");
    }
    let mut no_consent = submitted.clone();
    no_consent.insert("prompt".into(), json!("without consent"));
    no_consent.insert("consent".into(), json!(false));
    assert!(confirm(&mut current, &no_consent).is_err());
    current.run("select.deselect", json!({})).unwrap();
    assert!(confirm(&mut current, &submitted).is_err());
    assert_eq!(last_prompt(&current, FILL), "Keep this failed request");
}

#[test]
fn malformed_or_oversized_remembered_values_are_ignored_and_unicode_limits_are_characters() {
    let mut current = app(&Arc::default());
    for command in [FILL, REMOVE, EXPAND] {
        let limit = if command == FILL { 32_000 } else { 31_000 };
        for bad in [json!("wrong shape"), json!({"prompt":7}), json!({"prompt":"界".repeat(limit+1)})] {
            current.session.prefs.edit(|p| p.dialogs.insert(command.into(), bad));
            assert_eq!(last_prompt(&current, command), "");
        }
        let text = "界".repeat(limit);
        current.session.prefs.edit(|p| p.dialogs.insert(command.into(), json!({"prompt":text})));
        assert_eq!(last_prompt(&current, command), text);
    }
}
