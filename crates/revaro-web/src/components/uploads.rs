//! Browser upload queue. This module owns file selection, block geometry,
//! progress and resume records. The shared transport owns all network recovery.
//! Cancellation aborts transfers before abandoning the remote upload session.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use futures_channel::oneshot;
use futures_util::future::join_all;
use js_sys::Math;
use leptos::prelude::*;
use revaro_core::api::files::CreateDirectoryRequest;
use revaro_core::api::uploads::{CompleteUploadRequest, CreateUploadRequest, PartUrl};
use revaro_core::limits;
use revaro_core::model::{
    File as ModelFile, FileKind, UploadMode, UploadStatus as UploadLifecycle,
};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{AbortController, Blob, DragEvent, File as BrowserFile, FileList, HtmlInputElement};

use crate::api::{self, RequestError};
use crate::logic::feedback::Feedback;
use crate::logic::upload::{directory_paths, part_size, relative_path_parts, transfer_progress};

const FILE_CONCURRENCY: usize = 3;
const MULTIPART_CONCURRENCY: usize = 4;
const RESUME_KEY: &str = "revaro.uploads.v1";

/// The local lifecycle of a browser-selected file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UploadState {
    Queued,
    Retrying,
    Uploading,
    Verifying,
    Failed,
}

/// One file in the local queue.
#[derive(Clone)]
struct UploadItem {
    id: String,
    file: BrowserFile,
    parent_id: String,
    progress: u8,
    status: UploadState,
    error: String,
    upload_id: Option<String>,
    creation_key: String,
    run_id: u64,
}

/// The server contract resolved for one queue run.
struct ResolvedUpload {
    upload_id: String,
    mode: UploadMode,
    url: String,
    part_size: i64,
    part_count: usize,
    existing_parts: Vec<revaro_core::model::UploadPart>,
    already_completed: bool,
    finalizing: bool,
    data_received: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SavedUpload {
    #[serde(rename = "uploadId")]
    upload_id: String,
    #[serde(rename = "parentId")]
    parent_id: String,
    name: String,
    size: i64,
    #[serde(rename = "lastModified")]
    last_modified: f64,
    #[serde(default, rename = "idempotencyKey")]
    idempotency_key: String,
}

/// Runtime state shared by the queue's asynchronous operations.
struct UploadRuntime {
    disposed: Cell<bool>,
    next_item_id: Cell<u64>,
    next_run_id: Cell<u64>,
    active_count: Cell<usize>,
    active: RefCell<HashMap<(String, u64), Rc<ActiveUpload>>>,
    refresh_timer: Cell<Option<i32>>,
}

/// The requests and cancellation signal belonging to one queue run.
struct ActiveUpload {
    cancelled: Cell<bool>,
    abandon_remote: Cell<bool>,
    remote_upload_id: RefCell<Option<String>>,
    remote_abort_sent: Cell<bool>,
    transfer: AbortController,
    verifier: RefCell<Option<AbortController>>,
}

impl ActiveUpload {
    fn new() -> Self {
        Self {
            cancelled: Cell::new(false),
            abandon_remote: Cell::new(false),
            remote_upload_id: RefCell::new(None),
            remote_abort_sent: Cell::new(false),
            transfer: AbortController::new().expect("browser abort controller"),
            verifier: RefCell::new(None),
        }
    }

    fn remember_upload(&self, upload_id: &str) {
        let mut current = self.remote_upload_id.borrow_mut();
        if current.as_deref() != Some(upload_id) {
            *current = Some(upload_id.to_owned());
            self.remote_abort_sent.set(false);
        }
    }

    fn take_remote_upload_for_abort(&self) -> Option<String> {
        if self.remote_abort_sent.get() {
            return None;
        }
        let upload_id = self.remote_upload_id.borrow().clone();
        if upload_id.is_some() {
            self.remote_abort_sent.set(true);
        }
        upload_id
    }

    fn abort(&self) {
        self.cancelled.set(true);
        if let Some(controller) = self.verifier.borrow().as_ref() {
            controller.abort();
        }
        self.transfer.abort();
    }

    fn abandon(&self) {
        self.abandon_remote.set(true);
        self.abort();
    }
}

/// State and controls shared by the browser's upload controls and panel.
#[derive(Clone)]
pub struct UploadController {
    items: RwSignal<Vec<UploadItem>>,
    drag_active: RwSignal<bool>,
    current_id: Signal<String>,
    current_folder: RwSignal<Option<ModelFile>>,
    trash_mode: Signal<bool>,
    file_input: NodeRef<leptos::html::Input>,
    folder_input: NodeRef<leptos::html::Input>,
    runtime: Rc<UploadRuntime>,
    refresh_folder: Callback<UploadRefresh>,
    feedback: Callback<Feedback>,
    on_logout: Callback<()>,
}

pub(crate) type UiUploadController =
    leptos::__reexports::send_wrapper::SendWrapper<UploadController>;

/// A refresh request whose completion is observed by a folder upload before
/// it emits its success feedback.
pub(crate) struct UploadRefresh {
    pub(crate) parent_id: String,
    pub(crate) completion: Option<oneshot::Sender<()>>,
}

impl UploadRefresh {
    pub(crate) fn finish(self) {
        if let Some(sender) = self.completion {
            let _ = sender.send(());
        }
    }
}

impl UploadController {
    /// Create a queue bound to the current file-browser signals.
    pub fn new(
        current_id: Signal<String>,
        current_folder: RwSignal<Option<ModelFile>>,
        trash_mode: Signal<bool>,
        file_input: NodeRef<leptos::html::Input>,
        folder_input: NodeRef<leptos::html::Input>,
        refresh_folder: Callback<UploadRefresh>,
        feedback: Callback<Feedback>,
        on_logout: Callback<()>,
    ) -> Self {
        Self {
            items: RwSignal::new(Vec::new()),
            drag_active: RwSignal::new(false),
            current_id,
            current_folder,
            trash_mode,
            file_input,
            folder_input,
            runtime: Rc::new(UploadRuntime {
                disposed: Cell::new(false),
                next_item_id: Cell::new(0),
                next_run_id: Cell::new(0),
                active_count: Cell::new(0),
                active: RefCell::new(HashMap::new()),
                refresh_timer: Cell::new(None),
            }),
            refresh_folder,
            feedback,
            on_logout,
        }
    }

    /// Open the regular file chooser.
    pub fn choose_files(&self) {
        if let Some(input) = self.file_input.get() {
            input.click();
        }
    }

    /// Open the directory chooser.
    pub fn choose_folder(&self) {
        if let Some(input) = self.folder_input.get() {
            input.click();
        }
    }

    /// Queue all files from a regular file input or a drop event.
    pub fn accept_files(&self, files: Vec<BrowserFile>) {
        if self.trash_mode.get_untracked() || files.is_empty() {
            return;
        }
        let parent_id = self.current_id.get_untracked();
        self.queue_files(
            files
                .into_iter()
                .map(|file| (file, String::new()))
                .collect(),
            parent_id,
        );
        self.pump();
    }

    /// Resolve a folder selection into server directories before queuing files.
    pub fn accept_folder(&self, files: Vec<BrowserFile>) {
        if self.trash_mode.get_untracked() || files.is_empty() {
            return;
        }

        let destination = self.current_id.get_untracked();
        let mut records = Vec::with_capacity(files.len());
        for file in files {
            let raw = webkit_relative_path(&file).unwrap_or_else(|| file.name());
            let parts = match relative_path_parts(&raw) {
                Ok(parts) => parts,
                Err(message) => {
                    self.feedback.run(Feedback::error(message));
                    return;
                }
            };
            if parts.last().map(String::as_str) != Some(file.name().as_str()) {
                self.feedback
                    .run(Feedback::error("文件夹中包含与文件名不一致的路径"));
                return;
            }
            records.push((file, parts, raw.replace('\\', "/")));
        }

        let controller = self.clone();
        leptos::task::spawn_local(async move {
            let paths: Vec<Vec<String>> =
                records.iter().map(|(_, parts, _)| parts.clone()).collect();
            let mut folder_ids = HashMap::from([(String::new(), destination.clone())]);

            for path in directory_paths(&paths) {
                let split = path.rfind('/');
                let (parent_path, name) = match split {
                    Some(index) => (&path[..index], &path[index + 1..]),
                    None => ("", path.as_str()),
                };
                let Some(parent_id) = folder_ids.get(parent_path).cloned() else {
                    controller
                        .feedback
                        .run(Feedback::error(format!("无法解析目录“{path}”")));
                    return;
                };
                match ensure_upload_directory(&parent_id, name).await {
                    Ok(folder) => {
                        folder_ids.insert(path, folder.id);
                    }
                    Err(error) => {
                        controller.handle_request_error(&error);
                        return;
                    }
                }
            }

            let mut queued = Vec::with_capacity(records.len());
            for (file, parts, relative_path) in records {
                let directory = parts[..parts.len() - 1].join("/");
                let Some(parent_id) = folder_ids.get(&directory).cloned() else {
                    controller.feedback.run(Feedback::error(format!(
                        "无法解析“{relative_path}”的上传位置"
                    )));
                    return;
                };
                queued.push((file, relative_path, parent_id));
            }
            controller.queue_files_with_parents(queued);
            let (sender, receiver) = oneshot::channel();
            controller.refresh_if_current(destination, Some(sender));
            let _ = receiver.await;
            controller.feedback.run(Feedback::success(format!(
                "已保留目录结构，开始上传 {} 个文件",
                paths.len()
            )));
            controller.pump();
        });
    }

    /// Handle the hidden regular file input.
    pub fn accept_file_list(&self, list: Option<FileList>) {
        self.accept_files(file_list_items(list));
    }

    /// Handle the hidden directory input.
    pub fn accept_folder_list(&self, list: Option<FileList>) {
        self.accept_folder(file_list_items(list));
    }

    /// Mark the drop surface active while a file is dragged over the shell.
    pub fn on_drag_over(&self, event: DragEvent) {
        event.prevent_default();
        if !self.trash_mode.get_untracked() {
            self.drag_active.set(true);
        }
    }

    /// Hide the drop surface when the pointer leaves the shell.
    pub fn on_drag_leave(&self, event: DragEvent) {
        // The reference listener uses Vue's `.self` modifier: crossing a
        // child while dragging must not hide the shell-wide drop surface.
        let same_target = match (event.target(), event.current_target()) {
            (Some(target), Some(current)) => js_sys::Object::is(target.as_ref(), current.as_ref()),
            _ => false,
        };
        if !same_target {
            return;
        }
        self.drag_active.set(false);
    }

    /// Accept dropped files into the folder that was current at drop time.
    pub fn on_drop(&self, event: DragEvent) {
        event.prevent_default();
        self.drag_active.set(false);
        if self.trash_mode.get_untracked() {
            return;
        }
        let files = event.data_transfer().and_then(|transfer| transfer.files());
        self.accept_file_list(files);
    }

    /// Cancel a queued or active upload and abandon its server session.
    pub fn cancel(&self, item_id: String) {
        let item = self
            .items
            .get_untracked()
            .into_iter()
            .find(|item| item.id == item_id);
        let Some(item) = item else {
            return;
        };
        self.forget_creation(&item.creation_key);

        let active_runs: Vec<Rc<ActiveUpload>> = self
            .runtime
            .active
            .borrow()
            .iter()
            .filter(|((id, _), _)| id == &item_id)
            .map(|(_, active)| Rc::clone(active))
            .collect();
        for active in active_runs {
            active.abandon();
            if let Some(upload_id) = active.take_remote_upload_for_abort() {
                self.forget_resume(Some(&upload_id));
                spawn_abort(upload_id);
            }
        }
        if let Some(upload_id) = item.upload_id.clone() {
            let active_owns_upload = self
                .runtime
                .active
                .borrow()
                .iter()
                .filter(|((id, _), _)| id == &item_id)
                .any(|(_, active)| active.remote_upload_id.borrow().as_deref() == Some(&upload_id));
            if !active_owns_upload {
                self.forget_resume(Some(&upload_id));
                spawn_abort(upload_id);
            }
        }
        self.remove_item(&item_id, item.run_id);
    }

    /// Retry a failed upload, reconciling a committed session first.
    pub fn retry(&self, item_id: String) {
        let Some(item) = self
            .items
            .get_untracked()
            .into_iter()
            .find(|item| item.id == item_id)
        else {
            return;
        };
        if item.status != UploadState::Failed {
            return;
        }

        let previous_upload = item.upload_id.clone();
        self.update_item_if_current(&item_id, item.run_id, |item| {
            item.status = UploadState::Retrying;
            item.progress = 0;
            item.error.clear();
        });
        let controller = self.clone();
        let expected_run_id = item.run_id;
        leptos::task::spawn_local(async move {
            let mut reuse_completed = false;
            if let Some(upload_id) = previous_upload.as_deref() {
                match api::fetch_upload(upload_id).await {
                    Ok(status)
                        if matches!(
                            status.status,
                            UploadLifecycle::Completed | UploadLifecycle::Pending
                        ) =>
                    {
                        // The completion response may have been lost after the
                        // server committed. Let resolve_upload reconcile it
                        // instead of creating a conflicting sibling.
                        reuse_completed = true;
                    }
                    Ok(_)
                    | Err(RequestError {
                        status: 404 | 410, ..
                    }) => {
                        if let Err(error) = api::abort_upload(upload_id).await
                            && error.status != 404
                        {
                            controller.handle_request_error(&error);
                            controller.update_item_if_current(&item_id, expected_run_id, |item| {
                                item.status = UploadState::Failed;
                                item.error = error.message.clone();
                            });
                            return;
                        }
                    }
                    Err(error) => {
                        controller.handle_request_error(&error);
                        controller.update_item_if_current(&item_id, expected_run_id, |item| {
                            item.status = UploadState::Failed;
                            item.error = error.message.clone();
                        });
                        return;
                    }
                }
            }
            controller.update_item_if_current(&item_id, expected_run_id, |item| {
                if item.status == UploadState::Retrying {
                    item.status = UploadState::Queued;
                    item.upload_id = if reuse_completed {
                        previous_upload.clone()
                    } else {
                        None
                    };
                }
            });
            controller.pump();
        });
    }

    /// Abort browser requests when the authenticated shell is unmounted.
    pub fn dispose(&self) {
        if self.runtime.disposed.replace(true) {
            return;
        }
        if let Some(timer) = self.runtime.refresh_timer.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer);
        }
        for active in self.runtime.active.borrow().values() {
            active.abort();
        }
        self.items.set(Vec::new());
    }

    fn queue_files(&self, files: Vec<(BrowserFile, String)>, parent_id: String) {
        let entries = files
            .into_iter()
            .map(|(file, relative_path)| (file, relative_path, parent_id.clone()))
            .collect();
        self.queue_files_with_parents(entries);
    }

    fn queue_files_with_parents(&self, files: Vec<(BrowserFile, String, String)>) {
        let saved = saved_uploads();
        self.items.update(|items| {
            for (file, _relative_path, parent_id) in files.iter().cloned() {
                let size = file_size(&file);
                let resume = saved.iter().find(|entry| {
                    entry.parent_id == parent_id
                        && entry.name == file.name()
                        && entry.size == size
                        && entry.last_modified == file.last_modified()
                });
                items.push(UploadItem {
                    id: self.next_item_id(),
                    file,
                    parent_id,
                    progress: 0,
                    status: UploadState::Queued,
                    error: String::new(),
                    upload_id: resume
                        .filter(|entry| !entry.upload_id.is_empty())
                        .map(|entry| entry.upload_id.clone()),
                    creation_key: resume
                        .filter(|entry| !entry.idempotency_key.is_empty())
                        .map(|entry| entry.idempotency_key.clone())
                        .unwrap_or_else(new_creation_key),
                    run_id: 0,
                });
            }
        });
    }

    fn next_item_id(&self) -> String {
        let id = self.runtime.next_item_id.get().wrapping_add(1);
        self.runtime.next_item_id.set(id);
        format!("upload-{id}")
    }

    fn next_run_id(&self) -> u64 {
        let id = self.runtime.next_run_id.get().wrapping_add(1);
        self.runtime.next_run_id.set(id);
        id
    }

    fn pump(&self) {
        if self.runtime.disposed.get() {
            return;
        }
        while self.runtime.active_count.get() < FILE_CONCURRENCY {
            let Some(item) = self
                .items
                .get_untracked()
                .into_iter()
                .find(|item| item.status == UploadState::Queued)
            else {
                break;
            };
            let item_id = item.id.clone();
            let run_id = self.next_run_id();
            let active = Rc::new(ActiveUpload::new());
            if let Some(upload_id) = item.upload_id.as_deref() {
                active.remember_upload(upload_id);
            }
            self.update_item_if_current(&item_id, 0, |item| {
                item.status = UploadState::Uploading;
                item.error.clear();
                item.run_id = run_id;
            });
            self.runtime
                .active
                .borrow_mut()
                .insert((item_id.clone(), run_id), Rc::clone(&active));
            self.runtime
                .active_count
                .set(self.runtime.active_count.get() + 1);
            let controller = self.clone();
            leptos::task::spawn_local(async move {
                controller.run_upload(item_id.clone(), run_id, active).await;
                controller
                    .runtime
                    .active
                    .borrow_mut()
                    .remove(&(item_id, run_id));
                controller
                    .runtime
                    .active_count
                    .set(controller.runtime.active_count.get().saturating_sub(1));
                controller.pump();
            });
        }
    }

    async fn run_upload(&self, item_id: String, run_id: u64, active: Rc<ActiveUpload>) {
        let Some(item) = self
            .items
            .get_untracked()
            .into_iter()
            .find(|item| item.id == item_id && item.run_id == run_id)
        else {
            return;
        };
        let result = self
            .run_upload_inner(&item_id, run_id, &item, &active)
            .await;
        if active.cancelled.get() || self.runtime.disposed.get() {
            if active.abandon_remote.get()
                && let Some(upload_id) = active.take_remote_upload_for_abort()
            {
                self.forget_resume(Some(&upload_id));
                spawn_abort(upload_id);
            }
            self.remove_item(&item_id, run_id);
            return;
        }
        match result {
            Ok(()) => {
                if let Some(upload_id) = active.remote_upload_id.borrow().clone() {
                    self.forget_resume(Some(&upload_id));
                }
                self.remove_item(&item_id, run_id);
                self.schedule_refresh(item.parent_id);
            }
            Err(error) => {
                if error.is_unauthorized() {
                    self.on_logout.run(());
                }
                self.update_item_if_current(&item_id, run_id, |item| {
                    item.status = UploadState::Failed;
                    item.error = error.message.clone();
                });
            }
        }
    }

    async fn run_upload_inner(
        &self,
        item_id: &str,
        run_id: u64,
        item: &UploadItem,
        active: &Rc<ActiveUpload>,
    ) -> Result<(), RequestError> {
        let size = file_size(&item.file);
        self.save_resume(item.upload_id.as_deref(), item);
        let resolved = self.resolve_upload(item, size, active).await?;
        active.remember_upload(&resolved.upload_id);
        self.update_item_if_current(item_id, run_id, |item| {
            item.upload_id = Some(resolved.upload_id.clone());
        });
        self.save_resume(Some(&resolved.upload_id), item);
        ensure_not_cancelled(active)?;
        if resolved.already_completed {
            return Ok(());
        }

        let completed_parts = if resolved.finalizing || resolved.data_received {
            Vec::new()
        } else {
            match resolved.mode {
                UploadMode::Single => {
                    if resolved.url.is_empty() {
                        return Err(local_error("服务端没有返回上传地址"));
                    }
                    let body = file_blob(&item.file).clone();
                    let progress = self.progress_callback(
                        item_id.to_owned(),
                        run_id,
                        size,
                        Rc::new(RefCell::new(vec![0_i64])),
                        0,
                    );
                    crate::transport::put_blob(
                        &resolved.url,
                        &body,
                        Some(&file_mime(&item.file)),
                        &active.transfer.signal(),
                        progress,
                    )
                    .await?;
                    Vec::new()
                }
                UploadMode::Multipart => {
                    self.upload_multipart(
                        item_id,
                        run_id,
                        &item.file,
                        &resolved.upload_id,
                        size,
                        resolved.part_size,
                        resolved.part_count,
                        resolved.existing_parts,
                        active,
                    )
                    .await?
                }
            }
        };

        ensure_not_cancelled(active)?;
        self.update_item_if_current(item_id, run_id, |item| {
            item.status = UploadState::Verifying;
            item.progress = 99;
        });
        let verifier =
            AbortController::new().map_err(|error| js_error("无法创建提交控制器", error))?;
        let signal = verifier.signal();
        active.verifier.borrow_mut().replace(verifier);
        let result = api::complete_upload(
            &resolved.upload_id,
            &CompleteUploadRequest {
                parts: completed_parts,
            },
            Some(&signal),
        )
        .await;
        active.verifier.borrow_mut().take();
        result?;
        Ok(())
    }

    async fn resolve_upload(
        &self,
        item: &UploadItem,
        size: i64,
        active: &Rc<ActiveUpload>,
    ) -> Result<ResolvedUpload, RequestError> {
        if let Some(upload_id) = item.upload_id.as_deref() {
            match api::fetch_upload(upload_id).await {
                Ok(status) if status.status == UploadLifecycle::Completed => {
                    // The commit endpoint is idempotent. Completing here also
                    // validates that the saved session still belongs to this
                    // file before the item is shown as done.
                    api::complete_upload(
                        upload_id,
                        &CompleteUploadRequest { parts: Vec::new() },
                        None,
                    )
                    .await?;
                    return Ok(ResolvedUpload {
                        upload_id: upload_id.to_owned(),
                        mode: status.mode,
                        url: status.url,
                        part_size: status.part_size,
                        part_count: status.part_count,
                        existing_parts: status.parts,
                        already_completed: true,
                        finalizing: status.finalizing,
                        data_received: status.data_received,
                    });
                }
                Ok(status) if status.status == UploadLifecycle::Pending => {
                    validate_upload_shape(status.mode, status.part_size, status.part_count, size)?;
                    if status.expected_size != 0 && status.expected_size != size {
                        return Err(local_error("本地文件大小与断点上传不一致"));
                    }
                    return Ok(ResolvedUpload {
                        upload_id: status.upload_id,
                        mode: status.mode,
                        url: status.url,
                        part_size: status.part_size,
                        part_count: status.part_count,
                        existing_parts: status.parts,
                        already_completed: false,
                        finalizing: status.finalizing,
                        data_received: status.data_received,
                    });
                }
                Ok(_) => {
                    self.forget_resume(Some(upload_id));
                }
                Err(error) if error.status == 404 || error.status == 410 => {
                    if error.status == 410 {
                        match api::abort_upload(upload_id).await {
                            Ok(()) => {}
                            Err(error) if error.status == 404 => {}
                            Err(error) => return Err(error),
                        }
                    }
                    self.forget_resume(Some(upload_id));
                }
                Err(error) => return Err(error),
            }
        }

        let request = CreateUploadRequest {
            parent_id: item.parent_id.clone(),
            name: item.file.name(),
            size,
            mime_type: file_mime(&item.file),
            idempotency_key: item.creation_key.clone(),
        };
        let created = api::create_upload(&request).await?;
        // Replaying creation can find an already transferred or committed file.
        active.remember_upload(&created.upload_id);
        self.update_item_if_current(&item.id, item.run_id, |item| {
            item.upload_id = Some(created.upload_id.clone())
        });
        self.save_resume(Some(&created.upload_id), item);
        let status = api::fetch_upload(&created.upload_id).await?;
        validate_upload_shape(status.mode, status.part_size, status.part_count, size)?;
        Ok(ResolvedUpload {
            upload_id: status.upload_id,
            mode: status.mode,
            url: created.url,
            part_size: status.part_size,
            part_count: status.part_count,
            existing_parts: status.parts,
            already_completed: status.status == UploadLifecycle::Completed,
            finalizing: status.finalizing,
            data_received: status.data_received,
        })
    }

    async fn upload_multipart(
        &self,
        item_id: &str,
        run_id: u64,
        file: &BrowserFile,
        upload_id: &str,
        total_size: i64,
        part_size_value: i64,
        part_count: usize,
        existing: Vec<revaro_core::model::UploadPart>,
        active: &Rc<ActiveUpload>,
    ) -> Result<Vec<revaro_core::storage::CompletedPart>, RequestError> {
        let mut sent = vec![0_i64; part_count];
        let mut completed = vec![None; part_count];
        for part in existing {
            let number = usize::try_from(part.part_number).ok();
            let Some(number) = number else {
                continue;
            };
            let Some(index) = number.checked_sub(1) else {
                continue;
            };
            let Some(expected) = part_size(total_size, part_size_value, number) else {
                continue;
            };
            let size_matches = part.size.is_none_or(|size| size == expected);
            if index < part_count && size_matches && !part.etag.trim().is_empty() {
                if let Some(expected_hash) = part.content_hash.as_deref().filter(|h| !h.is_empty())
                {
                    let start = index as i64 * part_size_value;
                    let body = Blob::slice_with_f64_and_f64(
                        file_blob(file),
                        start as f64,
                        (start + expected) as f64,
                    )
                    .map_err(|e| js_error("无法读取续传分片", e))?;
                    if crate::transport::blob_hash(&body).await? != expected_hash {
                        return Err(local_error("本地文件与已上传分片不一致，请取消后重新上传"));
                    }
                } else {
                    // Legacy acknowledgements lack a content hash: resend this
                    // block so the server compares it with its durable bytes.
                    continue;
                }
                sent[index] = expected;
                completed[index] = Some(revaro_core::storage::CompletedPart {
                    part_number: part.part_number,
                    etag: part.etag,
                    size: part.size,
                    content_hash: part.content_hash,
                });
            }
        }
        let missing: Vec<i32> = completed
            .iter()
            .enumerate()
            .filter_map(|(index, part)| part.is_none().then_some(index as i32 + 1))
            .collect();
        let sent = Rc::new(RefCell::new(sent));
        let completed = Rc::new(RefCell::new(completed));

        for page in missing.chunks(limits::MAX_UPLOAD_PART_BATCH) {
            ensure_not_cancelled(active)?;
            let urls = page
                .iter()
                .map(|number| PartUrl {
                    part_number: *number,
                    url: format!("/api/uploads/{upload_id}/data/{number}"),
                })
                .collect::<Vec<_>>();

            let cursor = Rc::new(Cell::new(0_usize));
            let workers = urls.len().min(MULTIPART_CONCURRENCY);
            let mut futures = Vec::with_capacity(workers);
            for _ in 0..workers {
                let controller = self.clone();
                let active = Rc::clone(active);
                let file = file.clone();
                let urls = urls.clone();
                let cursor = Rc::clone(&cursor);
                let sent_for_progress = Rc::clone(&sent);
                let completed = Rc::clone(&completed);
                futures.push(async move {
                    loop {
                        let index = cursor.get();
                        cursor.set(index + 1);
                        let Some(part) = urls.get(index).cloned() else {
                            return Ok::<(), RequestError>(());
                        };
                        ensure_not_cancelled(&active)?;
                        let number = usize::try_from(part.part_number)
                            .map_err(|_| local_error("服务端返回了无效的分片编号"))?;
                        let index = number
                            .checked_sub(1)
                            .ok_or_else(|| local_error("服务端返回了无效的分片编号"))?;
                        let expected = part_size(total_size, part_size_value, number)
                            .ok_or_else(|| local_error("服务端返回了无效的分片编号"))?;
                        let start = index as i64 * part_size_value;
                        let end = start + expected;
                        let body = Blob::slice_with_f64_and_f64(
                            file_blob(&file),
                            start as f64,
                            end as f64,
                        )
                        .map_err(|error| js_error("无法读取上传分片", error))?;
                        if body.size() as i64 != expected {
                            return Err(local_error("本地文件分片大小无效"));
                        }
                        let progress = {
                            let sent = Rc::clone(&sent_for_progress);
                            let controller = controller.clone();
                            let item_id = item_id.to_owned();
                            Rc::new(move |loaded: u64| {
                                let loaded = (loaded as i64).clamp(0, expected);
                                sent.borrow_mut()[index] = loaded;
                                let total = sent.borrow().iter().sum();
                                controller.set_item_progress(
                                    &item_id,
                                    run_id,
                                    transfer_progress(total, total_size),
                                );
                            })
                        };
                        let etag = crate::transport::put_blob(
                            &part.url,
                            &body,
                            None,
                            &active.transfer.signal(),
                            progress,
                        )
                        .await?;
                        if etag.trim().is_empty() {
                            return Err(local_error("服务器没有返回分片校验信息"));
                        }
                        completed.borrow_mut()[index] = Some(revaro_core::storage::CompletedPart {
                            part_number: part.part_number,
                            etag: etag.clone(),
                            ..Default::default()
                        });
                    }
                });
            }
            let results = join_all(futures).await;
            if let Some(error) = results.into_iter().find_map(Result::err) {
                return Err(error);
            }
        }

        completed
            .borrow()
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, part)| {
                part.ok_or_else(|| local_error(format!("分片 {} 尚未完成", index + 1)))
            })
            .collect()
    }

    fn progress_callback(
        &self,
        item_id: String,
        run_id: u64,
        total_size: i64,
        sent: Rc<RefCell<Vec<i64>>>,
        index: usize,
    ) -> Rc<dyn Fn(u64)> {
        let controller = self.clone();
        Rc::new(move |loaded: u64| {
            sent.borrow_mut()[index] = loaded as i64;
            let done = sent.borrow().iter().sum();
            controller.set_item_progress(&item_id, run_id, transfer_progress(done, total_size));
        })
    }

    fn set_item_progress(&self, item_id: &str, run_id: u64, progress: u8) {
        self.update_item_if_current(item_id, run_id, |item| item.progress = progress.min(98));
    }

    fn update_item_if_current<F>(&self, item_id: &str, run_id: u64, update: F)
    where
        F: FnOnce(&mut UploadItem),
    {
        self.items.update(|items| {
            if let Some(item) = items
                .iter_mut()
                .find(|item| item.id == item_id && (run_id == 0 || item.run_id == run_id))
            {
                update(item);
            }
        });
    }

    fn remove_item(&self, item_id: &str, run_id: u64) {
        self.items.update(|items| {
            items.retain(|item| item.id != item_id || item.run_id != run_id);
        });
    }

    fn save_resume(&self, upload_id: Option<&str>, item: &UploadItem) {
        let mut saved = saved_uploads();
        saved.retain(|entry| entry.idempotency_key != item.creation_key);
        saved.push(SavedUpload {
            upload_id: upload_id.unwrap_or_default().to_owned(),
            parent_id: item.parent_id.clone(),
            name: item.file.name(),
            size: file_size(&item.file),
            last_modified: item.file.last_modified(),
            idempotency_key: item.creation_key.clone(),
        });
        persist_saved_uploads(&saved);
    }

    fn forget_creation(&self, key: &str) {
        let mut saved = saved_uploads();
        saved.retain(|entry| entry.idempotency_key != key);
        persist_saved_uploads(&saved);
    }

    fn forget_resume(&self, upload_id: Option<&str>) {
        let Some(upload_id) = upload_id else {
            return;
        };
        let mut saved = saved_uploads();
        saved.retain(|entry| entry.upload_id != upload_id);
        persist_saved_uploads(&saved);
    }

    fn refresh_if_current(&self, parent_id: String, completion: Option<oneshot::Sender<()>>) {
        if !self.trash_mode.get_untracked() && self.current_id.get_untracked() == parent_id {
            self.refresh_folder.run(UploadRefresh {
                parent_id,
                completion,
            });
        } else if let Some(sender) = completion {
            let _ = sender.send(());
        }
    }

    fn schedule_refresh(&self, parent_id: String) {
        let Some(window) = web_sys::window() else {
            return;
        };
        if let Some(timer) = self.runtime.refresh_timer.take() {
            window.clear_timeout_with_handle(timer);
        }
        let controller = self.clone();
        let callback =
            Closure::once(move || controller.refresh_if_current(parent_id, None)).into_js_value();
        if let Ok(timer) = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 250)
        {
            self.runtime.refresh_timer.set(Some(timer));
        }
    }

    fn handle_request_error(&self, error: &RequestError) {
        if error.is_unauthorized() {
            self.on_logout.run(());
        } else {
            self.feedback.run(Feedback::error(error.message.clone()));
        }
    }
}

/// Render the hidden chooser inputs and the drag/drop surface.
#[component]
pub fn UploadSurface(controller: UiUploadController) -> impl IntoView {
    let file_input = controller.file_input;
    let folder_input = controller.folder_input;

    folder_input.on_load(|input| {
        let _ = input.set_attribute("webkitdirectory", "");
        let _ = input.set_attribute("directory", "");
    });

    let file_controller = controller.clone();
    let folder_controller = controller.clone();
    let drag_active = controller.drag_active;
    let current_folder = controller.current_folder;

    view! {
        <input
            node_ref=file_input
            class="upload-input"
            hidden
            type="file"
            multiple
            aria-label="选择文件上传"
            on:change=move |event| {
                let input = event_target::<HtmlInputElement>(&event);
                file_controller.accept_file_list(input.files());
                input.set_value("");
            }
        />
        <input
            node_ref=folder_input
            class="upload-input"
            hidden
            type="file"
            multiple
            aria-label="选择文件夹上传"
            on:change=move |event| {
                let input = event_target::<HtmlInputElement>(&event);
                folder_controller.accept_folder_list(input.files());
                input.set_value("");
            }
        />
        <Show when=move || drag_active.get() fallback=|| ()>
            <div class="drop-zone" role="status">
                <div>
                    <span aria-hidden="true">"↓"</span>
                    <h2>
                        "释放以上传到 "
                        {move || current_folder
                            .get()
                            .map(|folder| if folder.name.is_empty() { "我的文件".to_owned() } else { folder.name })
                            .unwrap_or_else(|| "我的文件".to_owned())}
                    </h2>
                    <p>"文件将保存到服务器本地磁盘"</p>
                </div>
            </div>
        </Show>
    }
}

/// Only current uploads and failures are rendered; successful entries are removed by the queue.
#[component]
pub fn UploadProgress(controller: UiUploadController) -> impl IntoView {
    let items = controller.items;
    view! {
        <Show when=move || !items.get().is_empty() fallback=|| ()>
            <section class="upload-progress" aria-label="上传进度">
                <header><strong>"上传进度"</strong><small>{move || format!("{} 个文件", items.get().len())}</small></header>
                <div class="upload-progress-list" tabindex="0" aria-label="上传文件进度，可上下滚动">
                    <For
                        each=move || items.get()
                        key=|item| item.id.clone()
                        children={
                            let controller = controller.clone();
                            move |item| {
                                let id = item.id.clone();
                                let name = item.file.name();
                                let current = Signal::derive({
                                    let id = id.clone();
                                    move || items.get().into_iter().find(|item| item.id == id)
                                });
                                let failed = move || current.get().is_some_and(|item| item.status == UploadState::Failed);
                                let progress = move || current.get().map_or(0, |item| item.progress);
                                let retry = controller.clone();
                                let retry_id = id.clone();
                                let on_retry = Callback::new(move |()| retry.retry(retry_id.clone()));
                                let cancel = controller.clone();
                                let cancel_id = id.clone();
                                view! {
                                    <article class="upload-progress-item" class:failed=failed>
                                        <strong title=name.clone()>{name.clone()}</strong>
                                        <span class="upload-progress-state" title=move || current.get().map_or(String::new(), |item| item.error)>
                                            {move || current.get().map(|item| match item.status {
                                                UploadState::Queued => "排队中".to_owned(),
                                                UploadState::Retrying => "重试中".to_owned(),
                                                UploadState::Verifying => "处理中".to_owned(),
                                                UploadState::Uploading => format!("{}%", item.progress),
                                                UploadState::Failed => "上传失败".to_owned(),
                                            })}
                                        </span>
                                        <progress max="100" value=progress aria-label="文件上传进度"></progress>
                                        <div class="upload-progress-actions">
                                            <Show when=failed fallback=|| ()>
                                                <button type="button" aria-label="重试上传" title="重试上传"
                                                    on:click=move |_| on_retry.run(())>
                                                    {super::icons::rotate_cw()}<span>"重试"</span>
                                                </button>
                                            </Show>
                                            <button type="button" aria-label=move || if failed() { "移除失败上传" } else { "取消上传" }
                                                title=move || if failed() { "移除失败上传" } else { "取消上传" }
                                                on:click=move |_| cancel.cancel(cancel_id.clone())>
                                                {super::icons::x()}
                                            </button>
                                        </div>
                                    </article>
                                }
                            }
                        }
                    />
                </div>
            </section>
        </Show>
    }
}

async fn ensure_upload_directory(parent_id: &str, name: &str) -> Result<ModelFile, RequestError> {
    match api::create_directory(&CreateDirectoryRequest {
        parent_id: parent_id.to_owned(),
        name: name.to_owned(),
    })
    .await
    {
        Ok(folder) => Ok(folder),
        Err(error) if error.status == 409 => {
            let children = api::fetch_child_items(parent_id).await?;
            if let Some(folder) = children
                .into_iter()
                .find(|item| item.name == name && item.kind == FileKind::Directory)
            {
                Ok(folder)
            } else {
                Err(local_error(format!(
                    "“{name}”与已有文件重名，无法创建上传目录"
                )))
            }
        }
        Err(error) => Err(error),
    }
}

fn validate_upload_shape(
    mode: UploadMode,
    part_size_value: i64,
    part_count: usize,
    total_size: i64,
) -> Result<(), RequestError> {
    match mode {
        // Existing single-request sessions remain resumable after the global
        // multipart migration. New sessions are always assigned by the server.
        UploadMode::Single => Ok(()),
        UploadMode::Multipart => {
            let expected_count = limits::multipart_part_count(total_size, part_size_value)
                .map_err(|message| local_error(message.to_owned()))?;
            if !limits::uses_multipart_upload(total_size)
                || expected_count != part_count
                || part_size_value <= 0
            {
                return Err(local_error("服务端返回了无效的分片参数"));
            }
            Ok(())
        }
    }
}

fn file_list_items(list: Option<FileList>) -> Vec<BrowserFile> {
    let Some(list) = list else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|index| list.item(index))
        .collect()
}

fn webkit_relative_path(file: &BrowserFile) -> Option<String> {
    js_sys::Reflect::get(file.as_ref(), &JsValue::from_str("webkitRelativePath"))
        .ok()
        .and_then(|value| value.as_string())
        .filter(|path| !path.is_empty())
}

fn file_blob(file: &BrowserFile) -> &Blob {
    file.unchecked_ref()
}

fn file_size(file: &BrowserFile) -> i64 {
    file_blob(file).size().clamp(0.0, i64::MAX as f64) as i64
}

fn file_mime(file: &BrowserFile) -> String {
    let mime_type = file_blob(file).type_();
    if mime_type.is_empty() {
        "application/octet-stream".to_owned()
    } else {
        mime_type
    }
}

fn saved_uploads() -> Vec<SavedUpload> {
    let Some(storage) = web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    else {
        return Vec::new();
    };
    let Some(raw) = storage.get_item(RESUME_KEY).ok().flatten() else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_str(&raw) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter_map(|entry| {
            let serde_json::Value::Object(entry) = entry else {
                return None;
            };
            let upload_id = entry.get("uploadId")?.as_str()?;
            let parent_id = entry.get("parentId")?.as_str()?;
            let name = entry.get("name")?.as_str()?;
            let size = entry.get("size")?.as_f64()?;
            let last_modified = entry.get("lastModified")?.as_f64()?;
            // Match the reference's Number.isSafeInteger/Number.isFinite
            // guards per entry; one malformed record must not discard valid
            // resume records beside it.
            let idempotency_key = entry
                .get("idempotencyKey")
                .and_then(|key| key.as_str())
                .unwrap_or_default();
            if (upload_id.is_empty() && idempotency_key.is_empty())
                || parent_id.is_empty()
                || name.is_empty()
                || !size.is_finite()
                || size < 0.0
                || size.fract() != 0.0
                || size > 9_007_199_254_740_991.0
                || !last_modified.is_finite()
            {
                return None;
            }
            Some(SavedUpload {
                upload_id: upload_id.to_owned(),
                parent_id: parent_id.to_owned(),
                name: name.to_owned(),
                size: size as i64,
                last_modified,
                idempotency_key: idempotency_key.to_owned(),
            })
        })
        .collect()
}

fn new_creation_key() -> String {
    format!(
        "upload-{:x}-{:x}",
        js_sys::Date::now() as u64,
        (Math::random() * 9_007_199_254_740_991.0) as u64
    )
}

fn persist_saved_uploads(saved: &[SavedUpload]) {
    let Ok(raw) = serde_json::to_string(saved) else {
        return;
    };
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(RESUME_KEY, &raw);
    }
}

fn spawn_abort(upload_id: String) {
    leptos::task::spawn_local(async move {
        let _ = api::abort_upload(&upload_id).await;
    });
}

fn ensure_not_cancelled(active: &ActiveUpload) -> Result<(), RequestError> {
    if active.cancelled.get() {
        Err(cancelled_error())
    } else {
        Ok(())
    }
}

fn cancelled_error() -> RequestError {
    RequestError {
        status: 0,
        code: None,
        message: "上传已取消".to_owned(),
    }
}

fn local_error(message: impl Into<String>) -> RequestError {
    RequestError {
        status: 0,
        code: None,
        message: message.into(),
    }
}

fn js_error(context: &str, error: JsValue) -> RequestError {
    local_error(match error.as_string() {
        Some(detail) if !detail.is_empty() => format!("{context}: {detail}"),
        _ => context.to_owned(),
    })
}
