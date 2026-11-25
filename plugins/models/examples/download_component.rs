//! A model list with a download button and a progress line, in one `Component`.
//!
//! The component owns a `ModelList` and a `DownloadTracker`. `build` only reads their signals;
//! the buttons write them through the handles. No handler waits on the network:
//! `DownloadTracker::start_hub` fetches the repository listing on `frust::spawn_blocking` and
//! reports a failure through the state signal, which, like every signal of the two handles, is
//! written on the UI thread. Run it inside a Frust app by passing
//! `ModelsScreen` to `frust::app!`; here it only has to compile.

use frust::{Component, Get, RwSignal, Set, View, button, column, text};
use frust_models::api::{DownloadState, DownloadTracker, ModelList};
use frust_models::{HubClient, ModelStore};

#[derive(Default)]
struct ModelsScreen;

struct ScreenState {
    list: ModelList,
    tracker: DownloadTracker,
    hub: HubClient,
    /// Why the last download could not start (another one still runs), shown under the buttons.
    problem: RwSignal<String>,
}

impl Component for ModelsScreen {
    type State = ScreenState;

    fn init(&self) -> ScreenState {
        let store = ModelStore::open().expect("the app data directory is available");
        let list = ModelList::new(store);
        list.refresh();
        ScreenState {
            tracker: DownloadTracker::new().with_list(list.clone()),
            list,
            hub: HubClient::new()
                .expect("the TLS configuration builds")
                .with_token(std::env::var("HF_TOKEN").ok()),
            problem: RwSignal::new(String::new()),
        }
    }

    fn build(&self, state: &mut ScreenState) -> impl View<ScreenState> {
        let progress = state.tracker.progress().get();
        let line = match state.tracker.state().get() {
            DownloadState::Idle => "No download".to_owned(),
            DownloadState::Running => match progress.total {
                Some(total) => format!(
                    "File {} of {}: {} of {total} bytes",
                    progress.file_index + 1,
                    progress.files,
                    progress.bytes
                ),
                None => format!("{} bytes", progress.bytes),
            },
            DownloadState::Verifying => "Checking the file".to_owned(),
            DownloadState::Done => "Done".to_owned(),
            DownloadState::Cancelled => "Cancelled; starting again resumes".to_owned(),
            DownloadState::Failed(message) => format!("Failed: {message}"),
        };

        let mut models = column();
        for model in state.list.models().get() {
            models = models.child(text(format!("{} ({} bytes)", model.id, model.size_bytes)));
        }

        column()
            .child(models)
            .child(button("Download", |state: &mut ScreenState| {
                // A model with its config and tokenizer, downloaded as one job. The listing is
                // fetched off the UI thread; a failure shows up as `DownloadState::Failed`.
                let started = state.tracker.start_hub(
                    &state.hub,
                    state.list.store(),
                    "acme/tiny",
                    "main",
                    &["model.onnx", "config.json", "tokenizer.json"],
                );
                state
                    .problem
                    .set(started.err().map(|e| e.to_string()).unwrap_or_default());
            }))
            .child(button("Cancel", |state: &mut ScreenState| {
                state.tracker.cancel();
            }))
            .child(text(line))
            .child(text(state.problem.get()))
    }
}

fn main() {
    // The example is compiled to prove the API shape; a real app starts it with
    // `frust::app!(ModelsScreen);`.
    let _ = ModelsScreen;
}
