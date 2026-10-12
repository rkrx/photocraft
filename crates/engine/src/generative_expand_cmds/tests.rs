use super::*;
use photocraft_ai::{Backend, Response};
use std::sync::{Arc, Mutex};

struct Paint(Arc<Mutex<Vec<Request>>>);
impl Backend for Paint {
    fn id(&self) -> &'static str {
        "expand-test"
    }
    fn generate(&self, request: &Request, cancelled: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<Response> {
        request.validate()?;
        assert_eq!(request.operation, Operation::Expand);
        assert!(request.prompt.starts_with(PROMPT));
        assert!(!cancelled());
        self.0.lock().unwrap().push(request.clone());
        Ok(Response {
            quality: None,
            version: VERSION,
            image: shared::encode(
                request.source.width,
                request.source.height,
                [255, 0, 0, 255].repeat(request.source.width as usize * request.source.height as usize),
            )
            .unwrap(),
        })
    }
}
fn session(depth: u32, mode: &str) -> (Session, Arc<Mutex<Vec<Request>>>) {
    let mut s = Session::new();
    s.edit_prefs(|p| p.integrations.image_processing.transition_width = 0);
    s.execute("file.new", json!({"width": 8, "height": 8, "depth": depth, "mode": mode, "background": "white"})).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    s.generative_backend = Some(Arc::new(Paint(calls.clone())));
    (s, calls)
}
fn params() -> Value {
    json!({"width": 16, "height": 9, "consent": true})
}

#[test]
fn geometry_preserves_size_and_validates_anchors_and_banner_limits() {
    let size = Size::new(8, 8);
    for (anchor, offset) in [
        ("topLeft", [0, 0]),
        ("top", [4, 0]),
        ("topRight", [8, 0]),
        ("left", [0, 1]),
        ("center", [4, 1]),
        ("right", [8, 1]),
        ("bottomLeft", [0, 1]),
        ("bottom", [4, 1]),
        ("bottomRight", [8, 1]),
    ] {
        let mut p = params();
        p["anchor"] = json!(anchor);
        assert_eq!(geometry(size, &p).unwrap(), Geometry { width: 16, height: 9, offset });
    }
    for original in [Size::new(8, 8), Size::new(900, 900), Size::new(1024, 1024), Size::new(1600, 900), Size::new(2048, 500)] {
        let banner = banner_size(original).unwrap();
        assert_eq!(banner.width * 9, banner.height * 16);
        assert!(banner.width >= original.width && banner.height >= original.height && banner != original);
        assert!(banner.width <= MAX_EDGE && banner.height <= MAX_EDGE);
    }
    assert_eq!(banner_size(Size::new(900, 900)), Some(Size::new(1600, 900)));
    assert!(banner_size(Size::new(1200, 1200)).is_none());
    assert!(banner_size(Size::new(0, 0)).is_none());
    let mut s = Session::new();
    s.edit_prefs(|p| p.integrations.image_processing.transition_width = 0);
    assert!(!s.is_enabled(EXPAND));
    s.execute("file.new", json!({"width": 2048, "height": 2048})).unwrap();
    assert!(!s.is_enabled(EXPAND));
}

#[test]
fn expands_margins_in_one_undo_preserving_original_depth_color_and_selection() {
    for depth in [8, 16, 32] {
        for mode in ["rgb", "grayscale", "cmyk", "lab"] {
            let (mut s, calls) = session(depth, mode);
            s.execute("select.rect", json!({"x": 2, "y": 2, "width": 2, "height": 2})).unwrap();
            let original = s.active().unwrap().doc.clone();
            let original_active = s.active().unwrap().active_layer;
            let original_count = s.active().unwrap().history.entries().len();
            let result = s.execute(EXPAND, params()).unwrap();
            assert_eq!(result["operation"], "expand");
            assert_eq!(result["offset"], json!([4, 1]));
            let expanded = s.active().unwrap().doc.clone();
            assert_eq!(expanded.size, Size::new(16, 9));
            assert_eq!(expanded.mode, original.mode);
            assert_eq!(expanded.depth, original.depth);
            assert_eq!(expanded.icc_profile, original.icc_profile);
            assert_eq!(expanded.layers.len(), original.layers.len() + 1);
            assert_eq!(s.active().unwrap().history.entries().len(), original_count + 1);
            assert_eq!(s.active().unwrap().history.undo_label(), Some("Generative Expand"));
            let old_rect = Rect::from_xywh(4, 1, 8, 8);
            let source = original.layers[0].surface().unwrap();
            assert_eq!(source.read_region(original.bounds()), expanded.layers[0].surface().unwrap().read_region(old_rect));
            assert_eq!(expanded.selection.as_ref().unwrap().sample_channel(6, 3, 0), 1.0);
            assert_eq!(expanded.selection.as_ref().unwrap().sample_channel(0, 0, 0), 0.0);
            let layer = expanded.layers.last().unwrap();
            assert_eq!(layer.name, "Generative Expand");
            let surface = layer.surface().unwrap();
            assert_eq!(surface.format().sample, original.depth);
            assert_eq!(surface.format().mode, original.mode);
            for y in 0..9 {
                for x in 0..16 {
                    assert_eq!(surface.sample_channel(x, y, surface.format().channels() - 1), if old_rect.contains(x, y) { 0.0 } else { 1.0 });
                }
            }
            let before = photocraft_compose::render(&original, original.bounds());
            let after = photocraft_compose::render(&expanded, old_rect);
            assert_eq!(before.px, after.px, "provider must not repaint the original {mode}/{depth} image");
            let recorded = calls.lock().unwrap();
            assert_eq!(recorded.len(), 1);
            let request = &recorded[0];
            let mask = photocraft_codecs::decode(&request.mask.bytes(photocraft_ai::MAX_MASK_BYTES).unwrap()).unwrap();
            for y in 0..9 {
                for x in 0..16 {
                    assert_eq!(mask.get(x, y, 3), if old_rect.contains(x as i32, y as i32) { 1.0 } else { 0.0 });
                }
            }
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(*s.active().unwrap().doc, *original);
            assert_eq!(s.active().unwrap().active_layer, original_active);
            s.execute("edit.redo", json!({})).unwrap();
            assert_eq!(*s.active().unwrap().doc, *expanded);
        }
    }
}

#[test]
fn selection_is_optional_and_transparent_original_pixels_are_protected() {
    let (mut s, calls) = session(32, "rgb");
    s.edit("transparent original", |doc, active| {
        let surface = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        surface.write_pixel(0, 0, &[0.125, 0.375, 0.625, 0.5]);
        surface.write_pixel(1, 0, &[0.0; 4]);
        Ok(())
    })
    .unwrap();
    let original = s.active().unwrap().doc.clone();
    assert!(s.is_enabled(EXPAND));
    s.execute(EXPAND, params()).unwrap();
    assert!(s.active().unwrap().doc.selection.is_none());
    let old_rect = Rect::from_xywh(4, 1, 8, 8);
    assert_eq!(photocraft_compose::render(&original, original.bounds()).px, photocraft_compose::render(&s.active().unwrap().doc, old_rect).px);
    let recorded = calls.lock().unwrap();
    let request = &recorded[0];
    let mask = photocraft_codecs::decode(&request.mask.bytes(photocraft_ai::MAX_MASK_BYTES).unwrap()).unwrap();
    let image = photocraft_codecs::decode(&request.source.bytes(photocraft_ai::MAX_IMAGE_BYTES).unwrap()).unwrap();
    assert_eq!(mask.get(5, 1, 3), 1.0, "original transparency does not make that pixel editable");
    assert_eq!(image.get(5, 1, 3), 0.0);
    assert_eq!(image.get(0, 0, 3), 0.0, "new margins use transparent padding");
}

#[test]
fn translates_geometry_channels_masks_and_preserves_the_central_model() {
    let (mut s, calls) = session(16, "rgb");
    s.execute("view.newGuide", json!({"orientation": "vertical", "position": 3})).unwrap();
    s.execute("path.set", json!({"name": "Saved", "path": {"subpaths": [{"knots": [[1, 1], [3, 1], [3, 3]]}]}})).unwrap();
    s.execute("prefs.set", json!({"path": "integrations.imageModel", "value": "saved-model"})).unwrap();
    s.edit("synthetic marks", |doc, _| {
        doc.notes.push(photocraft_doc::analysis::Note { position: [2.0, 3.0], ..Default::default() });
        let mut channel = Surface::new(photocraft_color::PixelFormat::new(photocraft_color::ColorMode::Grayscale, photocraft_color::SampleType::U16, false));
        channel.write_pixel(1, 2, &[0.5]);
        doc.channels.push(photocraft_doc::AlphaChannel::new("saved channel", channel));
        Ok(())
    })
    .unwrap();
    let mut p = params();
    p["prompt"] = json!("  continue the studio wall  ");
    s.execute(EXPAND, p).unwrap();
    let doc = &s.active().unwrap().doc;
    assert_eq!(doc.guides.vertical, vec![7.0]);
    assert_eq!(doc.notes[0].position, [6.0, 4.0]);
    assert!((doc.channels[0].surface.sample_channel(5, 3, 0) - 0.5).abs() < 0.001);
    assert_eq!(calls.lock().unwrap()[0].model, "saved-model");
    assert!(calls.lock().unwrap()[0].prompt.ends_with("Scene to continue: continue the studio wall"));
    let mut p = json!({"width": 24, "height": 9, "model": "override-model", "consent": true});
    p["anchor"] = json!("left");
    s.execute(EXPAND, p).unwrap();
    assert_eq!(calls.lock().unwrap()[1].model, "override-model");
}

#[test]
fn rejects_invalid_geometry_consent_and_params_before_upload_or_edit() {
    let (mut s, calls) = session(8, "rgb");
    let doc = s.active().unwrap().doc.clone();
    let mut cases = vec![json!({}), json!([]), json!({"width": 16, "height": 9})];
    for (key, value) in [
        ("width", json!(-1)),
        ("width", json!(0)),
        ("width", json!(2049)),
        ("width", json!(u64::MAX)),
        ("width", json!(16.5)),
        ("width", json!(7)),
        ("height", json!(7)),
        ("height", json!(null)),
        ("anchor", json!("nonsense")),
        ("anchor", json!(true)),
        ("consent", json!(false)),
        ("consent", json!("true")),
        ("prompt", json!(null)),
        ("prompt", json!("x".repeat(31_001))),
        ("model", json!("")),
        ("apiKey", json!("secret")),
    ] {
        let mut p = params();
        p[key] = value;
        cases.push(p);
    }
    cases.push(json!({"width": 8, "height": 8, "consent": true}));
    for p in cases {
        assert!(s.execute(EXPAND, p).is_err());
        assert!(Arc::ptr_eq(&doc, &s.active().unwrap().doc));
    }
    assert!(calls.lock().unwrap().is_empty());
    s.generative_backend = None;
    assert!(s.execute(EXPAND, params()).is_err());
    assert!(Arc::ptr_eq(&doc, &s.active().unwrap().doc));
}

#[test]
fn failed_malformed_or_transparent_provider_results_never_resize_the_document() {
    struct Bad(u8);
    impl Backend for Bad {
        fn id(&self) -> &'static str {
            "bad"
        }
        fn generate(&self, request: &Request, _: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<Response> {
            match self.0 {
                0 => Err(photocraft_ai::Error::Provider("synthetic failure".into())),
                1 => Ok(Response { quality: None, version: VERSION + 1, image: request.source.clone() }),
                2 => Ok(Response { quality: None, version: VERSION, image: shared::encode(8, 8, [255; 4].repeat(64)).unwrap() }),
                _ => Ok(Response { quality: None, version: VERSION, image: request.source.clone() }),
            }
        }
    }
    for failure in 0..4 {
        let (mut s, _) = session(8, "rgb");
        s.generative_backend = Some(Arc::new(Bad(failure)));
        let original = s.active().unwrap().doc.clone();
        let history = s.active().unwrap().history.entries().len();
        assert!(s.execute(EXPAND, params()).is_err());
        assert!(Arc::ptr_eq(&original, &s.active().unwrap().doc));
        assert_eq!(s.active().unwrap().history.entries().len(), history);
    }
}

#[test]
fn cancelling_or_changing_the_source_discards_the_whole_expansion() {
    struct Blocking {
        ready: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl Backend for Blocking {
        fn id(&self) -> &'static str {
            "blocking"
        }
        fn generate(&self, request: &Request, cancelled: &(dyn Fn() -> bool + Sync)) -> photocraft_ai::Result<Response> {
            self.ready.send(()).unwrap();
            self.release.lock().unwrap().recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            if cancelled() {
                return Err(photocraft_ai::Error::Cancelled);
            }
            Paint(Arc::new(Mutex::new(Vec::new()))).generate(request, cancelled)
        }
    }
    for cancel in [true, false] {
        let (mut s, _) = session(8, "rgb");
        let original = s.active().unwrap().doc.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        s.generative_backend = Some(Arc::new(Blocking { ready: ready_tx, release: Mutex::new(release_rx) }));
        let crate::jobs::Started::Job(job) = s.start(EXPAND, params()).unwrap() else { panic!("expected background job") };
        ready_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert!(Arc::ptr_eq(&original, &s.active().unwrap().doc), "canvas must stay original while waiting for the provider");
        if cancel {
            assert!(s.cancel_job(job));
        } else {
            s.edit("concurrent edit", |doc, _| {
                doc.name = "changed".into();
                Ok(())
            })
            .unwrap();
        }
        release_tx.send(()).unwrap();
        assert!(s.wait_job(job).is_err());
        assert_eq!(s.active().unwrap().doc.size, original.size);
        assert_eq!(s.active().unwrap().doc.layers.len(), original.layers.len());
    }
}

#[test]
fn canvas_relative_content_is_rejected_before_upload_if_it_changes_the_original() {
    let (mut s, calls) = session(8, "rgb");
    s.execute("layer.newFillLayer.gradient", json!({"from": "#000000", "to": "#ffffff", "angle": 0})).unwrap();
    let original = s.active().unwrap().doc.clone();
    let history = s.active().unwrap().history.entries().len();
    let error = s.execute(EXPAND, params()).unwrap_err().to_string();
    assert!(error.contains("canvas-relative"), "{error}");
    assert!(calls.lock().unwrap().is_empty());
    assert!(Arc::ptr_eq(&original, &s.active().unwrap().doc));
    assert_eq!(s.active().unwrap().history.entries().len(), history);
}
