//! The Vision screen: a photo (the bundled sample or a picked one), the three tasks driven by
//! `VisionHandle`, their overlays and the status line.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use frust::authoring::{PaintCtx, PaintScene};
use frust::kurbo::{Point, Rect, Size};
use frust::{
    Color, Component, EdgeInsets, Get, ImageSource, Padding, RwSignal, Set, View, canvas, column,
    spawn_blocking, spawn_local, text, wrap,
};
use frust_material::{filled_button, outlined_button, tonal_button};
use frust_onnx::api::VisionStatus;
use frust_onnx::catalog::{VISION_CATALOG, VisionTask};
use frust_onnx::{Face, Plane};

use crate::format::{task_label, timing_line};
use crate::imaging::{self, Rgba};
use crate::services::Services;

/// Every photo gets a new number, so a result can be matched to the photo it was asked for.
static PHOTO_GENERATION: AtomicU64 = AtomicU64::new(1);

/// The Vision destination.
pub struct VisionScreen {
    /// The shared handles.
    pub services: Rc<Services>,
}

/// The photo on screen: the pixels inference reads and the image the canvas draws.
#[derive(Clone)]
pub struct Photo {
    /// RGBA8, shared with the vision requests without a copy.
    pub pixels: Arc<[u8]>,
    /// What the canvas draws.
    pub image: ImageSource,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Shown under the image.
    pub label: String,
    /// See [`PHOTO_GENERATION`].
    pub generation: u64,
}

impl Photo {
    fn new(rgba: Rgba, label: String) -> Self {
        let (pixels, image, width, height) = rgba.into_parts();
        Self {
            pixels,
            image,
            width,
            height,
            label,
            generation: PHOTO_GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }
}

/// Which result the image area shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Show {
    /// The photo alone.
    Photo,
    /// The photo under the tinted subject mask.
    Mask,
    /// The photo with face boxes and landmarks.
    Faces,
    /// The depth map in place of the photo.
    Depth,
}

/// An overlay image and the photo it was computed for.
#[derive(Clone)]
struct Overlay {
    generation: u64,
    image: ImageSource,
}

/// The Vision screen's retained state.
pub struct VisionState {
    services: Rc<Services>,
    photo: RwSignal<Option<Photo>>,
    /// Decode and pick progress or errors.
    note: RwSignal<String>,
    show: Show,
    /// The photo generation each task was last asked about (segment, faces, depth).
    requested: [u64; 3],
    mask_seen: Option<Arc<Plane>>,
    depth_seen: Option<Arc<Plane>>,
    faces_seen: Arc<Vec<Face>>,
    faces_for: u64,
    mask: RwSignal<Option<Overlay>>,
    depth: RwSignal<Option<Overlay>>,
}

fn slot(task: VisionTask) -> usize {
    match task {
        VisionTask::Segmentation => 0,
        VisionTask::FaceDetection => 1,
        VisionTask::Depth => 2,
    }
}

fn same<T>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// Converts a result plane into an overlay image off the UI thread, then publishes it.
fn convert(
    plane: Arc<Plane>,
    generation: u64,
    to: fn(&Plane) -> Rgba,
    out: RwSignal<Option<Overlay>>,
) {
    spawn_local(async move {
        let image = spawn_blocking(move || {
            let rgba = to(&plane);
            ImageSource::from_rgba8(rgba.pixels, rgba.width, rgba.height)
        })
        .await;
        if let Ok(image) = image {
            out.set(Some(Overlay { generation, image }));
        }
    });
}

/// Decodes the bundled sample off the UI thread and shows it.
fn show_sample(photo: RwSignal<Option<Photo>>, note: RwSignal<String>) {
    note.set("Decoding the sample...".to_owned());
    spawn_local(async move {
        match spawn_blocking(imaging::load_sample).await {
            Ok(Ok(rgba)) => {
                photo.set(Some(Photo::new(rgba, "Sample (generated)".to_owned())));
                note.set(String::new());
            }
            Ok(Err(err)) => note.set(format!("The sample did not decode: {err}")),
            Err(err) => note.set(format!("The sample did not decode: {err}")),
        }
    });
}

/// `width` x `height` scaled to fit `size`, centred.
fn contain(width: u32, height: u32, size: Size) -> Rect {
    let (w, h) = (f64::from(width), f64::from(height));
    if w <= 0.0 || h <= 0.0 || size.width <= 0.0 || size.height <= 0.0 {
        return Rect::ZERO;
    }
    let scale = (size.width / w).min(size.height / h);
    let (dw, dh) = (w * scale, h * scale);
    let x = (size.width - dw) / 2.0;
    let y = (size.height - dh) / 2.0;
    Rect::new(x, y, x + dw, y + dh)
}

impl Component for VisionScreen {
    type State = VisionState;

    fn init(&self) -> VisionState {
        let photo = RwSignal::new(None);
        let note = RwSignal::new(String::new());
        show_sample(photo, note);
        let vision = &self.services.vision;
        VisionState {
            services: Rc::clone(&self.services),
            photo,
            note,
            show: Show::Photo,
            requested: [0; 3],
            mask_seen: vision.mask().get(),
            depth_seen: vision.depth().get(),
            faces_seen: vision.faces().get(),
            faces_for: 0,
            mask: RwSignal::new(None),
            depth: RwSignal::new(None),
        }
    }

    fn build(&self, state: &mut VisionState) -> impl View<VisionState> {
        let services = Rc::clone(&state.services);
        let vision = &services.vision;
        let status = vision.status().get();
        let stored = services.list.models().get();
        let photo = state.photo.get();

        // New results: convert planes to overlay images off the UI thread, tagged with the photo
        // they were asked for (the newest request of a task wins, so that is `requested`).
        let mask = vision.mask().get();
        if !same(&mask, &state.mask_seen) {
            state.mask_seen = mask.clone();
            if let Some(plane) = mask {
                convert(plane, state.requested[0], imaging::mask_overlay, state.mask);
            }
        }
        let depth = vision.depth().get();
        if !same(&depth, &state.depth_seen) {
            state.depth_seen = depth.clone();
            if let Some(plane) = depth {
                convert(plane, state.requested[2], imaging::depth_image, state.depth);
            }
        }
        let faces = vision.faces().get();
        if !Arc::ptr_eq(&faces, &state.faces_seen) {
            state.faces_seen = Arc::clone(&faces);
            state.faces_for = state.requested[1];
        }

        let generation = photo.as_ref().map_or(0, |p| p.generation);
        let current = |overlay: Option<Overlay>| overlay.filter(|o| o.generation == generation);
        let mask_image = current(state.mask.get()).map(|o| o.image);
        let depth_image = current(state.depth.get()).map(|o| o.image);
        let faces = (state.faces_for == generation).then_some(faces);

        // What the canvas draws, and a key that changes whenever any of it does.
        let shown = state.show;
        let base = photo.as_ref().map(|p| (p.image.clone(), p.width, p.height));
        let draw_mask = mask_image.filter(|_| shown == Show::Mask);
        let draw_depth = depth_image.filter(|_| shown == Show::Depth);
        let draw_faces = faces.clone().filter(|_| shown == Show::Faces);
        let key = (
            generation,
            shown,
            draw_mask.as_ref().map(|i| i.image_data().data.id()),
            draw_depth.as_ref().map(|i| i.image_data().data.id()),
            draw_faces.as_ref().map(|f| Arc::as_ptr(f) as usize),
        );
        let image_area = canvas(
            move |scene: &mut dyn PaintScene, size: Size, _ctx: &PaintCtx| {
                scene.fill_rect(Point::ZERO, size, Color::from_rgb8(0x1C, 0x1B, 0x1F));
                let Some((image, width, height)) = &base else {
                    return;
                };
                let dest = contain(*width, *height, size);
                match &draw_depth {
                    Some(depth) => scene.draw_image(depth.image_data(), dest),
                    None => scene.draw_image(image.image_data(), dest),
                }
                if let Some(mask) = &draw_mask {
                    scene.draw_image(mask.image_data(), dest);
                }
                if let Some(faces) = &draw_faces {
                    let k = dest.width() / f64::from(*width);
                    let at = |x: f32, y: f32| {
                        Point::new(dest.x0 + f64::from(x) * k, dest.y0 + f64::from(y) * k)
                    };
                    let line = Color::from_rgb8(0x7C, 0xFC, 0x00);
                    for face in faces.iter() {
                        let [x, y, w, h] = face.rect;
                        let corners = [at(x, y), at(x + w, y), at(x + w, y + h), at(x, y + h)];
                        for i in 0..4 {
                            scene.stroke_line(corners[i], corners[(i + 1) % 4], 2.0, line);
                        }
                        for [lx, ly] in face.landmarks {
                            let p = at(lx, ly);
                            scene.fill_rect(
                                Point::new(p.x - 3.0, p.y - 3.0),
                                Size::new(6.0, 6.0),
                                line,
                            );
                        }
                    }
                }
            },
        )
        .expand()
        .repaint_key(key);

        let running = matches!(status, VisionStatus::Running(_));
        let mut status_line = match &status {
            VisionStatus::Running(task) => format!("Running {}...", task_label(*task)),
            VisionStatus::Failed(message) => format!("Failed: {message}"),
            VisionStatus::Idle => services.last_run.get().map_or_else(
                || "Choose a task to run a model".to_owned(),
                |t| timing_line(&t),
            ),
        };
        if shown == Show::Faces
            && let Some(faces) = &faces
        {
            status_line.push_str(&format!(" · {} face(s) found", faces.len()));
        }

        let photo_line = match (&photo, state.note.get()) {
            (_, note) if !note.is_empty() => note,
            (Some(p), _) => format!("{} · {}x{}", p.label, p.width, p.height),
            (None, _) => String::new(),
        };

        let sources = wrap()
            .spacing(8.0)
            .run_spacing(8.0)
            .child(outlined_button("Sample", |s: &mut VisionState| {
                show_sample(s.photo, s.note);
            }))
            .child(outlined_button("Pick photo", |s: &mut VisionState| {
                let (photo, note) = (s.photo, s.note);
                note.set("Waiting for the picker...".to_owned());
                spawn_local(async move {
                    // The picker and the decode block: both run off the UI thread.
                    match spawn_blocking(imaging::pick_photo).await {
                        Ok(Ok(Some((rgba, name)))) => {
                            photo.set(Some(Photo::new(rgba, name)));
                            note.set(String::new());
                        }
                        Ok(Ok(None)) => note.set(String::new()),
                        Ok(Err(err)) => note.set(format!("No photo: {err}")),
                        Err(err) => note.set(format!("No photo: {err}")),
                    }
                });
            }))
            .child(tonal_button("Photo only", |s: &mut VisionState| {
                s.show = Show::Photo;
            }));

        let mut actions = wrap().spacing(8.0).run_spacing(8.0);
        let mut hints = column().gap(4.0);
        for model in VISION_CATALOG {
            let task = model.task;
            let installed = Services::installed(&stored, task);
            let enabled = installed && photo.is_some() && !running;
            actions = actions.child(
                filled_button(task_label(task), move |s: &mut VisionState| {
                    let Some(photo) = s.photo.get() else {
                        return;
                    };
                    s.requested[slot(task)] = photo.generation;
                    let vision = &s.services.vision;
                    let (pixels, w, h) = (photo.pixels, photo.width, photo.height);
                    s.show = match task {
                        VisionTask::Segmentation => {
                            vision.segment(pixels, w, h);
                            Show::Mask
                        }
                        VisionTask::FaceDetection => {
                            vision.detect_faces(pixels, w, h, None);
                            Show::Faces
                        }
                        VisionTask::Depth => {
                            vision.estimate_depth(pixels, w, h);
                            Show::Depth
                        }
                    };
                })
                .enabled(enabled),
            );
            if !installed {
                hints = hints.child(text(format!(
                    "{}: download the model first ({} on the Models tab)",
                    task_label(task),
                    model.display
                )));
            }
        }

        Padding(
            EdgeInsets::all(12.0),
            column()
                .gap(8.0)
                .flex(1, image_area)
                .child(text(photo_line))
                .child(text(status_line))
                .child(sources)
                .child(actions)
                .child(hints),
        )
    }
}
