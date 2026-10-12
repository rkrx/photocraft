use super::*;
use photocraft_ai::Backend;
use photocraft_color::ColorMode;
use photocraft_color::SampleType;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Solid {
    calls: Arc<AtomicUsize>,
    failure: bool,
}
impl Backend for Solid {
    fn id(&self) -> &'static str {
        "test"
    }
    fn generate(&self, request: &Request, cancelled: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<Response> {
        request.validate()?;
        if cancelled() {
            return Err(photocraft_ai::Error::Cancelled);
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.failure {
            return Err(photocraft_ai::Error::Provider("synthetic failure".into()));
        }
        let mask = photocraft_codecs::decode(&request.mask.bytes(photocraft_ai::MAX_MASK_BYTES)?).unwrap();
        assert_eq!(mask.get(0, 0, 3), 1.0);
        assert_eq!(mask.get(2, 2, 3), 0.0);
        let source = photocraft_codecs::decode(&request.source.bytes(photocraft_ai::MAX_IMAGE_BYTES)?).unwrap();
        assert_eq!(source.width(), request.source.width);
        let png = encode(request.source.width, request.source.height, [255, 0, 0, 255].repeat(request.source.width as usize * request.source.height as usize))
            .unwrap();
        Ok(Response { quality: None, version: VERSION, image: png })
    }
}
fn session(depth: u32) -> Session {
    let mut s = Session::new();
    s.edit_prefs(|p| p.integrations.image_processing.transition_width = 0);
    s.execute("file.new", json!({"width": 8, "height": 8, "depth": depth, "background": "white"})).unwrap();
    s.execute("select.rect", json!({"x": 2, "y": 2, "width": 3, "height": 3})).unwrap();
    s.generative_backend = Some(Arc::new(Solid { calls: Arc::new(AtomicUsize::new(0)), failure: false }));
    s
}
fn params() -> Value {
    json!({"prompt": "red object", "consent": true})
}

#[test]
fn generative_edits_use_saved_model_and_allow_explicit_command_override() {
    struct Recording(Arc<Mutex<Vec<String>>>);
    impl Backend for Recording {
        fn id(&self) -> &'static str {
            "recording"
        }
        fn generate(&self, request: &Request, _: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<Response> {
            self.0.lock().unwrap().push(request.model.clone());
            Ok(Response { quality: None, version: VERSION, image: request.source.clone() })
        }
    }
    let mut s = session(8);
    let models = Arc::new(Mutex::new(Vec::new()));
    s.generative_backend = Some(Arc::new(Recording(models.clone())));
    s.execute("prefs.set", json!({"path": "integrations.imageModel", "value": "saved-model"})).unwrap();
    for command in [FILL, REMOVE] {
        s.execute(command, params()).unwrap();
        s.execute(command, json!({"prompt": "vase", "model": "override-model", "consent": true})).unwrap();
    }
    assert_eq!(*models.lock().unwrap(), vec!["saved-model", "override-model", "saved-model", "override-model"]);
}

#[test]
fn generative_edits_create_masked_layers_at_multiple_depths_and_undo_once() {
    for (command, operation) in [(FILL, "fill"), (REMOVE, "remove")] {
        for depth in [8, 16, 32] {
            let mut s = session(depth);
            let original = s.active().unwrap().doc.clone();
            let before_history = s.active().unwrap().history.entries().len();
            let result = s.execute(command, params()).unwrap();
            assert_eq!(result["operation"], operation);
            let st = s.active().unwrap();
            assert_eq!(st.doc.layers.len(), original.layers.len() + 1);
            assert_eq!(st.history.entries().len(), before_history + 1);
            let layer = st.doc.layer(photocraft_doc::LayerId(result["layer"].as_u64().unwrap())).unwrap();
            let surface = layer.surface().unwrap();
            assert_eq!(surface.format().sample, st.doc.depth);
            assert_eq!(surface.sample_channel(0, 0, 3), 0.0);
            assert_eq!(surface.sample_channel(2, 2, 3), 1.0);
            let outside = photocraft_compose::render(&st.doc, photocraft_geom::Rect::new(0, 0, 1, 1)).px[0];
            assert_eq!(outside, photocraft_compose::render(&original, photocraft_geom::Rect::new(0, 0, 1, 1)).px[0]);
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(s.active().unwrap().doc.layers.len(), original.layers.len());
            s.execute("edit.redo", json!({})).unwrap();
            assert_eq!(s.active().unwrap().doc.layers.len(), original.layers.len() + 1);
        }
    }
}

#[test]
fn invalid_params_fail_before_provider_or_edit() {
    let mut s = session(8);
    let calls = Arc::new(AtomicUsize::new(0));
    s.generative_backend = Some(Arc::new(Solid { calls: calls.clone(), failure: false }));
    let doc = s.active().unwrap().doc.clone();
    for p in [
        json!({}),
        json!({"prompt": false, "consent": true}),
        json!({"prompt": "", "consent": true}),
        json!({"prompt": "x", "consent": false}),
        json!({"prompt": "x", "consent": true, "model": 5}),
        json!({"prompt": "x", "consent": true, "apiKey": "secret"}),
        json!({"prompt": "x", "consent": true, "model": "\n"}),
        json!({"prompt": "x".repeat(32_001), "consent": true}),
    ] {
        assert!(s.execute(FILL, p).is_err());
        assert!(Arc::ptr_eq(&doc, &s.active().unwrap().doc));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    s.generative_backend = None;
    assert!(s.execute(FILL, params()).is_err());
    s.execute("select.deselect", json!({})).unwrap();
    assert!(!s.is_enabled(FILL));
    assert!(s.execute(FILL, params()).is_err());
    assert!(!Session::new().is_enabled(FILL));
}

#[test]
fn failed_provider_leaves_original_and_history_unchanged() {
    for command in [FILL, REMOVE] {
        let mut s = session(8);
        s.generative_backend = Some(Arc::new(Solid { calls: Arc::new(AtomicUsize::new(0)), failure: true }));
        let doc = s.active().unwrap().doc.clone();
        assert!(s.execute(command, params()).is_err());
        assert!(Arc::ptr_eq(&doc, &s.active().unwrap().doc));
    }
}

#[test]
fn result_validation_and_cancellation_never_modify_document() {
    let s = session(16);
    let doc = &s.active().unwrap().doc;
    let ctx = JobCtx::new();
    let mut response = Response { quality: None, version: VERSION + 1, image: encode(8, 8, [255, 0, 0, 255].repeat(64)).unwrap() };
    assert!(result_surface(doc, &response, &ctx).is_err());
    response.version = VERSION;
    response.image.width = 9;
    assert!(result_surface(doc, &response, &ctx).is_err());
    response.image = encode(8, 4, [255, 0, 0, 255].repeat(32)).unwrap();
    assert!(result_surface(doc, &response, &ctx).is_err());
    response.image = encode(16, 16, [255, 0, 0, 255].repeat(256)).unwrap();
    assert!(result_surface(doc, &response, &ctx).is_err(), "even an exact multiple is an unexpected size");
    response.image = encode(8, 8, [255, 0, 0, 255].repeat(64)).unwrap();
    let surface = result_surface(doc, &response, &ctx).unwrap();
    assert_eq!(surface.sample_channel(2, 2, 3), 1.0);
    ctx.cancel();
    assert!(prepare(doc, Operation::Fill, "red object".into(), photocraft_ai::DEFAULT_MODEL.into(), &ctx).is_err());
    assert!(result_surface(doc, &response, &ctx).is_err());
}

#[test]
fn feathered_selection_is_enforced_locally_and_color_modes_are_preserved() {
    for operation in [Operation::Fill, Operation::Remove] {
        for mode in [ColorMode::Rgb, ColorMode::Grayscale, ColorMode::Cmyk, ColorMode::Lab] {
            let mut doc = Document::new("test", photocraft_doc::Size { width: 8, height: 8 }, mode, SampleType::U16);
            let mut mask = Surface::new(PixelFormat::GRAY8);
            mask.write_pixel(2, 2, &[0.5]);
            doc.selection = Some(mask);
            let ctx = JobCtx::new();
            let request = prepare(&doc, operation, "red object".into(), photocraft_ai::DEFAULT_MODEL.into(), &ctx).unwrap();
            request.validate().unwrap();
            let response = Response { quality: None, version: VERSION, image: encode(8, 8, [255, 0, 0, 255].repeat(64)).unwrap() };
            let surface = result_surface(&doc, &response, &ctx).unwrap();
            assert_eq!(surface.format().mode, mode);
            assert!((surface.sample_channel(2, 2, surface.format().channels() - 1) - 0.5).abs() < 0.005);
            assert_eq!(surface.sample_channel(0, 0, surface.format().channels() - 1), 0.0);
        }
    }
}

struct Blocking {
    ready: std::sync::mpsc::Sender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl Backend for Blocking {
    fn id(&self) -> &'static str {
        "blocking-test"
    }
    fn generate(&self, request: &Request, cancelled: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<Response> {
        self.ready.send(()).unwrap();
        self.release.lock().unwrap().recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        Solid { calls: Arc::new(AtomicUsize::new(0)), failure: false }.generate(request, cancelled)
    }
}
#[test]
fn background_result_rejects_changed_document_and_cancel_discards_it() {
    for command in [FILL, REMOVE] {
        for cancel in [false, true] {
            let mut s = session(8);
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            s.generative_backend = Some(Arc::new(Blocking { ready: ready_tx, release: Mutex::new(release_rx) }));
            let crate::jobs::Started::Job(job) = s.start(command, params()).unwrap() else { panic!("expected background job") };
            ready_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            if cancel {
                assert!(s.cancel_job(job));
            } else {
                s.edit("synthetic concurrent edit", |doc, _| {
                    doc.name = "changed".into();
                    Ok(())
                })
                .unwrap();
            }
            release_tx.send(()).unwrap();
            assert!(s.wait_job(job).is_err());
            assert_eq!(s.active().unwrap().doc.layers.len(), 1);
        }
    }
}

#[test]
fn remove_reconstructs_the_selection_with_optional_detail_and_keeps_original_pixels() {
    struct Reconstruct(Arc<Mutex<Vec<Request>>>);
    impl Backend for Reconstruct {
        fn id(&self) -> &'static str {
            "reconstruct-test"
        }
        fn generate(&self, request: &Request, _: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<Response> {
            request.validate()?;
            assert_eq!(request.operation, Operation::Remove);
            assert!(request.prompt.starts_with(REMOVE_PROMPT));
            let source = photocraft_codecs::decode(&request.source.bytes(photocraft_ai::MAX_IMAGE_BYTES)?).unwrap();
            assert_eq!(source.get(3, 3, 0), 0.0, "visible object is included in the source");
            self.0.lock().unwrap().push(request.clone());
            Ok(Response { quality: None, version: VERSION, image: encode(8, 8, [255, 255, 255, 255].repeat(64)).unwrap() })
        }
    }
    for detail in [None, Some(""), Some("  cable and its shadow  ")] {
        let mut s = session(16);
        s.edit("synthetic object", |doc, active| {
            doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().write_pixel(3, 3, &[0.0, 0.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
        let original = s.active().unwrap().doc.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        s.generative_backend = Some(Arc::new(Reconstruct(requests.clone())));
        let mut p = json!({"consent": true});
        if let Some(detail) = detail {
            p["prompt"] = json!(detail);
        }
        let result = s.execute(REMOVE, p).unwrap();
        assert_eq!(result["operation"], "remove");
        let doc = &s.active().unwrap().doc;
        let layer = doc.layer(photocraft_doc::LayerId(result["layer"].as_u64().unwrap())).unwrap();
        assert_eq!(layer.name, "Generative Remove");
        let after = photocraft_compose::render(doc, doc.bounds());
        let before = photocraft_compose::render(&original, original.bounds());
        for (i, (a, b)) in after.px.iter().zip(&before.px).enumerate() {
            if i == 27 {
                assert!(a[0] > 0.99);
                assert_eq!(b[0], 0.0);
            } else {
                assert_eq!(a, b, "background stays unchanged at pixel {i}");
            }
        }
        assert_eq!(doc.layers[0].surface().unwrap().sample_channel(3, 3, 0), 0.0);
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].model, photocraft_ai::DEFAULT_MODEL);
        if detail.is_some_and(|s| !s.trim().is_empty()) {
            assert!(recorded[0].prompt.ends_with("Objects to remove: cable and its shadow"));
        } else {
            assert_eq!(recorded[0].prompt, REMOVE_PROMPT);
        }
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(photocraft_compose::render(&s.active().unwrap().doc, original.bounds()).px, before.px);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(photocraft_compose::render(&s.active().unwrap().doc, original.bounds()).px, after.px);
    }
}

#[test]
fn remove_rejects_invalid_input_and_requires_selection_backend_and_consent() {
    let mut s = session(8);
    let calls = Arc::new(AtomicUsize::new(0));
    s.generative_backend = Some(Arc::new(Solid { calls: calls.clone(), failure: false }));
    let doc = s.active().unwrap().doc.clone();
    for p in [
        json!({}),
        json!([]),
        json!({"consent": false}),
        json!({"consent": "true"}),
        json!({"consent": true, "prompt": false}),
        json!({"consent": true, "prompt": null}),
        json!({"consent": true, "prompt": "x".repeat(MAX_REMOVE_DETAIL + 1)}),
        json!({"consent": true, "model": 5}),
        json!({"consent": true, "model": ""}),
        json!({"consent": true, "apiKey": "secret"}),
    ] {
        assert!(s.execute(REMOVE, p).is_err());
        assert!(Arc::ptr_eq(&doc, &s.active().unwrap().doc));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    s.generative_backend = None;
    assert!(s.execute(REMOVE, json!({"consent": true})).is_err());
    s.execute("select.deselect", json!({})).unwrap();
    assert!(!s.is_enabled(REMOVE));
    assert!(s.execute(REMOVE, json!({"consent": true})).is_err());
    assert!(!Session::new().is_enabled(REMOVE));
    assert_eq!(s.execute("ai.status", json!({})).unwrap()["operations"], json!(["fill", "remove", "expand"]));
    assert!(REMOVE_PROMPT.chars().count() + MAX_REMOVE_DETAIL + "\nObjects to remove: ".chars().count() <= 32_000);
}

#[test]
fn composite_edits_are_visible_above_the_stack_when_a_hidden_child_is_active() {
    for command in [FILL, REMOVE] {
        let mut s = session(8);
        s.edit("synthetic hidden group", |doc, active| {
            let child = Layer::raster("hidden child", doc.pixel_format());
            *active = Some(child.id);
            let mut group = Layer::group("hidden group", vec![child]);
            group.visible = false;
            doc.layers.push(group);
            let mut foreground = Layer::raster("visible object", doc.pixel_format());
            foreground.surface_mut().unwrap().write_pixel(3, 3, &[0.0, 0.0, 0.0, 1.0]);
            doc.layers.push(foreground);
            Ok(())
        })
        .unwrap();
        let original = s.active().unwrap().doc.clone();
        let result = s.execute(command, params()).unwrap();
        let doc = &s.active().unwrap().doc;
        assert_eq!(Some(doc.layers.last().unwrap().id.0), result["layer"].as_u64());
        assert_eq!(&doc.layers[..original.layers.len()], &original.layers);
        let visible = photocraft_compose::render(doc, photocraft_geom::Rect::new(3, 3, 4, 4)).px[0];
        assert!(visible[0] > 0.99);
        assert_eq!(visible[1], 0.0);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(*s.active().unwrap().doc, *original);
    }
}

// Exercise the shared pipeline, including strict response validation before native colour conversion.
fn result_surface(doc: &Document, response: &Response, ctx: &JobCtx) -> Result<Surface> {
    let request = prepare(doc, Operation::Fill, "test".into(), photocraft_ai::DEFAULT_MODEL.into(), ctx)?;
    let mut capture = crate::generative_pipeline::prepare(request, photocraft_ai::Capabilities::native(), Default::default(), ctx)?;

    let rgba = crate::generative_pipeline::process(&mut capture, response.clone(), ctx)?;
    super::result_surface(doc, &rgba, ctx)
}

#[test]
fn diagnostic_replay_and_quality_comparison_are_offline_and_opt_in() {
    struct Echo;
    impl Backend for Echo {
        fn id(&self) -> &'static str {
            "synthetic"
        }
        fn capabilities(&self, _: &str) -> photocraft_ai::Result<photocraft_ai::Capabilities> {
            Ok(photocraft_ai::Capabilities::image2(true))
        }
        fn generate(&self, r: &Request, _: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<Response> {
            Ok(Response { version: VERSION, quality: Some(r.options.quality), image: r.source.clone() })
        }
    }
    let mut s = session(16);
    s.generative_backend = Some(Arc::new(Echo));
    s.execute(FILL, json!({"prompt":"offline fixture","consent":true})).unwrap();
    assert!(s.execute("ai.diagnostics", json!({})).unwrap().is_null());
    s.edit_prefs(|p| p.integrations.image_processing.diagnostics = true);
    let result = s.execute(FILL, json!({"prompt":"offline fixture","consent":true})).unwrap();
    assert_eq!(result["used"]["quality"], "high");
    let capture = s.execute("ai.diagnostics", json!({})).unwrap();
    assert_eq!(capture["applied"], true);
    assert!(capture["response"].is_object());
    assert!(capture["stages"]["final-result"].is_object());
    let doc = s.active().unwrap().doc.clone();
    let journal = s.journal.clone();
    s.generative_backend = None;
    let replay = s.execute("ai.replay", json!({"capture":capture})).unwrap();
    assert_eq!(replay["applied"], false);
    assert_eq!(replay["stages"]["final-result"], capture["stages"]["final-result"]);
    assert!(Arc::ptr_eq(&doc, &s.active().unwrap().doc));
    assert_eq!(s.journal, journal);
    let pair = s.execute("ai.comparison", json!({})).unwrap();
    let mut high = pair["high"].clone();
    high["options"]["quality"] = json!("xhigh");
    assert_eq!(high, pair["xhigh"], "only quality differs");
    assert_eq!(pair["networkCalled"], false);
    let text = pair.to_string();
    assert!(!text.contains("authorization"));
    assert!(!text.contains("api_key"));
    s.execute("ai.diagnostics", json!({"clear":true})).unwrap();
    assert!(s.execute("ai.replay", json!({})).is_err());
}

#[test]
fn running_generation_snapshots_api_and_local_options() {
    struct Snapshot {
        ready: std::sync::mpsc::Sender<Request>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl Backend for Snapshot {
        fn id(&self) -> &'static str {
            "snapshot"
        }
        fn generate(&self, r: &Request, _: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<Response> {
            self.ready.send(r.clone()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            Ok(Response { quality: Some(r.options.quality), version: VERSION, image: r.source.clone() })
        }
    }
    let mut s = session(16);
    s.edit_prefs(|p| p.integrations.image_processing.diagnostics = true);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    s.generative_backend = Some(Arc::new(Snapshot { ready: ready_tx, release: Mutex::new(release_rx) }));

    let crate::jobs::Started::Job(job) = s.start(FILL, json!({"prompt":"snapshot","consent":true})).unwrap() else { panic!("not a job") };
    let request = ready_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(request.options.quality, photocraft_ai::Quality::High);
    s.execute("prefs.set", json!({"values":{"integrations.imageOptions.quality":"xhigh","integrations.imageProcessing.maxTranslation":8}})).unwrap();
    release_tx.send(()).unwrap();
    let result = s.wait_job(job).unwrap();
    assert_eq!(result["used"]["quality"], "high");
    let capture = s.execute("ai.diagnostics", json!({})).unwrap();
    assert_eq!(capture["local"]["maxTranslation"], 4);
}
