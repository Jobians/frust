//! The Models screen: one row per catalog model with its download controls, the storage line
//! and the low-memory note.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::mem::discriminant;
use std::rc::Rc;

use frust::{Component, EdgeInsets, Get, Padding, Set, View, column, row, scroll_view, text};
use frust_material::{ProgressValue, filled_button, linear_progress, outlined_button, text_button};
use frust_models::api::DownloadState;

use crate::format::{bytes, low_memory_note, partial_line, progress_line, storage_line, task_kind};
use crate::services::Services;

/// The Models destination.
pub struct ModelsScreen {
    /// The shared handles.
    pub services: Rc<Services>,
}

/// The Models screen's retained state.
pub struct ModelsState {
    services: Rc<Services>,
    /// What the store looked like when its disk use was last measured.
    measured: Option<u64>,
}

impl Component for ModelsScreen {
    type State = ModelsState;

    fn init(&self) -> ModelsState {
        ModelsState {
            services: Rc::clone(&self.services),
            measured: None,
        }
    }

    fn build(&self, state: &mut ModelsState) -> impl View<ModelsState> {
        let services = Rc::clone(&state.services);
        let stored = services.list.models().get();
        let disk = services.disk.get();

        // Measure the disk again whenever the listing or a download's phase changes: a finished,
        // cancelled or deleted download moves the numbers. The walk runs off the UI thread.
        let mut hasher = DefaultHasher::new();
        for model in &stored {
            model.id.to_string().hash(&mut hasher);
            model.size_bytes.hash(&mut hasher);
        }
        for row in &services.rows {
            discriminant(&row.tracker.state().get()).hash(&mut hasher);
        }
        let fingerprint = hasher.finish();
        if state.measured != Some(fingerprint) {
            state.measured = Some(fingerprint);
            services.refresh_disk();
        }

        let mut page = column()
            .gap(12.0)
            .child(text(storage_line(disk.total, disk.partial_total())));
        if let Some(note) = services.memory_total.and_then(low_memory_note) {
            page = page.child(text(note));
        }

        for (index, entry) in services.rows.iter().enumerate() {
            let model = entry.model;
            let id = (model.id)();
            let phase = entry.tracker.state().get();
            let progress = entry.tracker.progress().get();
            let installed = stored.iter().find(|m| m.id == id);
            let partial = disk.partial_for(&id);
            let busy = matches!(phase, DownloadState::Running | DownloadState::Verifying);

            let status = if busy {
                progress_line(&phase, &progress).unwrap_or_default()
            } else if let Some(local) = installed {
                let verified = local.manifest.as_ref().is_some_and(|m| m.verified);
                format!(
                    "Installed, {}{}",
                    bytes(local.size_bytes),
                    if verified { ", verified" } else { "" }
                )
            } else if matches!(phase, DownloadState::Failed(_)) {
                progress_line(&phase, &progress).unwrap_or_default()
            } else if partial > 0 {
                partial_line(partial, model.size_bytes)
            } else {
                match progress_line(&phase, &progress) {
                    Some(line) if !matches!(phase, DownloadState::Done) => line,
                    _ => "Not downloaded".to_owned(),
                }
            };

            let mut card = column()
                .gap(4.0)
                .child(text(model.display).size(18.0))
                .child(text(format!(
                    "{} · {} · {} · input {}x{}",
                    task_kind(model.task),
                    bytes(model.size_bytes),
                    model.licence,
                    model.input.0,
                    model.input.1
                )))
                .child(text(status));
            if busy {
                let value = match progress.total {
                    Some(total) if total > 0 => {
                        ProgressValue::Determinate(progress.bytes as f64 / total as f64)
                    }
                    _ => ProgressValue::Indeterminate,
                };
                card = card.child(linear_progress(value));
            }

            let resume = partial > 0 || matches!(phase, DownloadState::Cancelled);
            let mut actions = row().gap(8.0);
            if busy {
                actions = actions.child(outlined_button("Cancel", move |s: &mut ModelsState| {
                    s.services.rows[index].tracker.cancel();
                }));
            } else if installed.is_some() {
                actions = actions.child(outlined_button("Delete", move |s: &mut ModelsState| {
                    s.services.delete(s.services.rows[index].model);
                }));
            } else {
                let label = if resume { "Resume" } else { "Download" };
                actions = actions.child(filled_button(label, move |s: &mut ModelsState| {
                    let services = &s.services;
                    let row = &services.rows[index];
                    let started = match &services.hub {
                        Ok(hub) => row
                            .tracker
                            .start(hub, &services.store, row.model.download_request())
                            .map_err(|err| err.to_string()),
                        Err(err) => Err(format!("no HTTP client: {err}")),
                    };
                    row.problem.set(started.err().unwrap_or_default());
                }));
                if partial > 0 {
                    actions = actions.child(text_button("Delete", move |s: &mut ModelsState| {
                        s.services.delete(s.services.rows[index].model);
                    }));
                }
            }
            card = card.child(actions);
            let problem = entry.problem.get();
            if !problem.is_empty() {
                card = card.child(text(problem));
            }
            page = page.child(card);
        }

        scroll_view(Padding(EdgeInsets::all(16.0), page))
    }
}
