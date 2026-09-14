//! Browser upload queue.
//!
//! The queue owns the browser `File` handles and the lifetime of every raw
//! byte request. JSON session operations are kept in [`crate::api`], while
//! this module deals with progress, cancellation, retries, local resume
//! records and directory selection. A cancelled request is aborted before the
//! remote upload session is deleted, so a late XHR callback cannot resurrect a
//! task in the visible queue.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::rc::Rc;

use futures_channel::oneshot;
use futures_util::future::join_all;
use leptos::prelude::*;
use revaro_core::api::files::CreateDirectoryRequest;
use revaro_core::api::uploads::{
    CompleteUploadRequest, CreateUploadRequest, RecordUploadPartRequest, UploadPartsRequest,
};
use revaro_core::limits;
use revaro_core::model::{
    File as ModelFile, FileKind, FileStatus, UploadMode, UploadStatus as UploadLifecycle,
};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    AbortController, Blob, DragEvent, File as BrowserFile, FileList, HtmlInputElement,
    ProgressEvent, XmlHttpRequest,
};

use crate::api::{self, RequestError};
use crate::logic::feedback::Feedback;
use crate::logic::upload::{directory_paths, part_size, relative_path_parts, transfer_progress};

const FILE_CONCURRENCY: usize = 3;
const MULTIPART_CONCURRENCY: usize = 4;
const UPLOAD_RETRIES: usize = 5;
const RESUME_KEY: &str = "revaro.uploads.v1";

/// The local lifecycle of a browser-selected file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UploadTaskStatus {
    Queued,
    Retrying,
    Uploading,
    Done,
    Failed,
    Cancelled,
}

/// One file in the local queue.
#[derive(Clone)]
struct UploadTask {
    id: String,
    file: BrowserFile,
    parent_id: String,
    progress: u8,
    status: UploadTaskStatus,
    error: String,
    upload_id: Option<String>,
    run_id: u64,
}

/// The server contract resolved for one queue run.
struct ResolvedUpload {
    upload_id: String,
    file_id: String,
    mode: UploadMode,
    url: String,
    part_size: i64,
    part_count: usize,
    existing_parts: Vec<revaro_core::model::UploadPart>,
    already_completed: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SavedUpload {
    upload_id: String,
    parent_id: String,
    name: String,
    size: i64,
    last_modified: f64,
}

/// Runtime state shared by the queue's asynchronous operations.
struct UploadRuntime {
    disposed: Cell<bool>,
    next_task_id: Cell<u64>,
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
    requests: RefCell<Vec<XmlHttpRequest>>,
    verifier: RefCell<Option<AbortController>>,
}

impl ActiveUpload {
    fn new() -> Self {
        Self {
            cancelled: Cell::new(false),
            abandon_remote: Cell::new(false),
            remote_upload_id: RefCell::new(None),
            remote_abort_sent: Cell::new(false),
            requests: RefCell::new(Vec::new()),
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
        for request in self.requests.borrow().iter() {
            let _ = request.abort();
        }
    }

    fn abandon(&self) {
        self.abandon_remote.set(true);
        self.abort();
    }
}

/// State and controls shared by the browser's upload controls and panel.
#[derive(Clone)]
pub struct UploadController {
    tasks: RwSignal<Vec<UploadTask>>,
    drag_active: RwSignal<bool>,
    current_id: RwSignal<String>,
    current_folder: RwSignal<Option<ModelFile>>,
    trash_mode: RwSignal<bool>,
    file_input: NodeRef<leptos::html::Input>,
    folder_input: NodeRef<leptos::html::Input>,
    runtime: Rc<UploadRuntime>,
    refresh_folder: Callback<UploadRefresh>,
    feedback: Callback<Feedback>,
    on_logout: Callback<()>,
}

type UiUploadController = leptos::__reexports::send_wrapper::SendWrapper<UploadController>;

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
        current_id: RwSignal<String>,
        current_folder: RwSignal<Option<ModelFile>>,
        trash_mode: RwSignal<bool>,
        file_input: NodeRef<leptos::html::Input>,
        folder_input: NodeRef<leptos::html::Input>,
        refresh_folder: Callback<UploadRefresh>,
        feedback: Callback<Feedback>,
        on_logout: Callback<()>,
    ) -> Self {
        Self {
            tasks: RwSignal::new(Vec::new()),
            drag_active: RwSignal::new(false),
            current_id,
            current_folder,
            trash_mode,
            file_input,
            folder_input,
            runtime: Rc::new(UploadRuntime {
                disposed: Cell::new(false),
                next_task_id: Cell::new(0),
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
        event.prevent_default();
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
    pub fn cancel(&self, task_id: String) {
        let task = self
            .tasks
            .get_untracked()
            .into_iter()
            .find(|task| task.id == task_id);
        let Some(task) = task else {
            return;
        };
        if matches!(
            task.status,
            UploadTaskStatus::Done | UploadTaskStatus::Cancelled
        ) {
            return;
        }

        let active_runs: Vec<Rc<ActiveUpload>> = self
            .runtime
            .active
            .borrow()
            .iter()
            .filter(|((id, _), _)| id == &task_id)
            .map(|(_, active)| Rc::clone(active))
            .collect();
        for active in active_runs {
            active.abandon();
            if let Some(upload_id) = active.take_remote_upload_for_abort() {
                self.forget_resume(Some(&upload_id));
                spawn_abort(upload_id);
            }
        }
        if let Some(upload_id) = task.upload_id.clone() {
            let active_owns_upload = self
                .runtime
                .active
                .borrow()
                .iter()
                .filter(|((id, _), _)| id == &task_id)
                .any(|(_, active)| active.remote_upload_id.borrow().as_deref() == Some(&upload_id));
            if !active_owns_upload {
                self.forget_resume(Some(&upload_id));
                spawn_abort(upload_id);
            }
        }
        self.set_task_if_current(&task_id, task.run_id, |task| {
            task.status = UploadTaskStatus::Cancelled;
            task.error.clear();
        });
    }

    /// Retry a failed or cancelled task, reconciling a committed session first.
    pub fn retry(&self, task_id: String) {
        let Some(task) = self
            .tasks
            .get_untracked()
            .into_iter()
            .find(|task| task.id == task_id)
        else {
            return;
        };
        if !matches!(
            task.status,
            UploadTaskStatus::Failed | UploadTaskStatus::Cancelled
        ) {
            return;
        }

        let previous_upload = task.upload_id.clone();
        if let Some(upload_id) = previous_upload.as_deref() {
            self.forget_resume(Some(upload_id));
        }
        self.set_task_if_current(&task_id, task.run_id, |task| {
            task.status = UploadTaskStatus::Retrying;
            task.progress = 0;
            task.error.clear();
        });
        let controller = self.clone();
        let expected_run_id = task.run_id;
        leptos::task::spawn_local(async move {
            let mut reuse_completed = false;
            if let Some(upload_id) = previous_upload.as_deref() {
                match api::fetch_upload(upload_id).await {
                    Ok(status) if status.status == UploadLifecycle::Completed => {
                        // The completion response may have been lost after the
                        // server committed. Let resolve_upload reconcile it
                        // instead of creating a conflicting sibling.
                        reuse_completed = true;
                    }
                    Ok(_) | Err(RequestError { status: 404, .. }) => {
                        if let Err(error) = api::abort_upload(upload_id).await
                            && error.status != 404
                        {
                            controller.handle_request_error(&error);
                            controller.set_task_if_current(&task_id, expected_run_id, |task| {
                                task.status = UploadTaskStatus::Failed;
                                task.error = error.message.clone();
                            });
                            return;
                        }
                    }
                    Err(error) => {
                        controller.handle_request_error(&error);
                        controller.set_task_if_current(&task_id, expected_run_id, |task| {
                            task.status = UploadTaskStatus::Failed;
                            task.error = error.message.clone();
                        });
                        return;
                    }
                }
            }
            controller.set_task_if_current(&task_id, expected_run_id, |task| {
                if task.status == UploadTaskStatus::Retrying {
                    task.status = UploadTaskStatus::Queued;
                    task.upload_id = if reuse_completed {
                        previous_upload.clone()
                    } else {
                        None
                    };
                }
            });
            controller.pump();
        });
    }

    /// Route a task-centre action to the local upload task that owns the
    /// server-side upload session.
    pub fn cancel_by_upload_id(&self, upload_id: String) {
        let task_id = self
            .tasks
            .get_untracked()
            .into_iter()
            .find(|task| task.upload_id.as_deref() == Some(upload_id.as_str()))
            .map(|task| task.id);
        if let Some(task_id) = task_id {
            self.cancel(task_id);
            return;
        }
        let controller = self.clone();
        leptos::task::spawn_local(async move {
            if let Err(error) = api::abort_upload(&upload_id).await
                && error.status != 404
            {
                controller.handle_request_error(&error);
            }
        });
    }

    /// Retry a task-centre upload through the browser queue when its file
    /// handle is still available after the upload failed.
    pub fn retry_by_upload_id(&self, upload_id: String) -> bool {
        let task_id = self
            .tasks
            .get_untracked()
            .into_iter()
            .find(|task| task.upload_id.as_deref() == Some(upload_id.as_str()))
            .map(|task| task.id);
        if let Some(task_id) = task_id {
            self.retry(task_id);
            true
        } else {
            // The local File handle can disappear when the page is reloaded,
            // but the durable task still supports the reference client's
            // server-side retry path. Let TaskController fall through to it.
            false
        }
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
        self.tasks.update(|tasks| {
            for task in tasks {
                if matches!(
                    task.status,
                    UploadTaskStatus::Queued | UploadTaskStatus::Uploading
                ) {
                    task.status = UploadTaskStatus::Cancelled;
                }
            }
        });
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
        self.tasks.update(|tasks| {
            for (file, _relative_path, parent_id) in files.iter().cloned() {
                let size = file_size(&file);
                let resume = saved.iter().find(|entry| {
                    entry.parent_id == parent_id
                        && entry.name == file.name()
                        && entry.size == size
                        && entry.last_modified == file.last_modified()
                });
                tasks.push(UploadTask {
                    id: self.next_task_id(),
                    file,
                    parent_id,
                    progress: 0,
                    status: UploadTaskStatus::Queued,
                    error: String::new(),
                    upload_id: resume.map(|entry| entry.upload_id.clone()),
                    run_id: 0,
                });
            }
        });
    }

    fn next_task_id(&self) -> String {
        let id = self.runtime.next_task_id.get().wrapping_add(1);
        self.runtime.next_task_id.set(id);
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
            let Some(task) = self
                .tasks
                .get_untracked()
                .into_iter()
                .find(|task| task.status == UploadTaskStatus::Queued)
            else {
                break;
            };
            let task_id = task.id.clone();
            let run_id = self.next_run_id();
            let active = Rc::new(ActiveUpload::new());
            if let Some(upload_id) = task.upload_id.as_deref() {
                active.remember_upload(upload_id);
            }
            self.set_task_if_current(&task_id, 0, |task| {
                task.status = UploadTaskStatus::Uploading;
                task.error.clear();
                task.run_id = run_id;
            });
            self.runtime
                .active
                .borrow_mut()
                .insert((task_id.clone(), run_id), Rc::clone(&active));
            self.runtime
                .active_count
                .set(self.runtime.active_count.get() + 1);
            let controller = self.clone();
            leptos::task::spawn_local(async move {
                controller.run_upload(task_id.clone(), run_id, active).await;
                controller
                    .runtime
                    .active
                    .borrow_mut()
                    .remove(&(task_id, run_id));
                controller
                    .runtime
                    .active_count
                    .set(controller.runtime.active_count.get().saturating_sub(1));
                controller.pump();
            });
        }
    }

    async fn run_upload(&self, task_id: String, run_id: u64, active: Rc<ActiveUpload>) {
        let Some(task) = self
            .tasks
            .get_untracked()
            .into_iter()
            .find(|task| task.id == task_id && task.run_id == run_id)
        else {
            return;
        };
        let result = self
            .run_upload_inner(&task_id, run_id, &task, &active)
            .await;
        if active.cancelled.get() || self.runtime.disposed.get() {
            if active.abandon_remote.get()
                && let Some(upload_id) = active.take_remote_upload_for_abort()
            {
                self.forget_resume(Some(&upload_id));
                spawn_abort(upload_id);
            }
            self.set_task_if_current(&task_id, run_id, |task| {
                task.status = UploadTaskStatus::Cancelled;
                task.error.clear();
            });
            return;
        }
        match result {
            Ok(()) => {
                if let Some(upload_id) = active.remote_upload_id.borrow().clone() {
                    self.forget_resume(Some(&upload_id));
                }
                self.set_task_if_current(&task_id, run_id, |task| {
                    task.progress = 100;
                    task.status = UploadTaskStatus::Done;
                    task.error.clear();
                });
                self.schedule_refresh(task.parent_id);
            }
            Err(error) => {
                if error.is_unauthorized() {
                    self.on_logout.run(());
                }
                self.set_task_if_current(&task_id, run_id, |task| {
                    task.status = UploadTaskStatus::Failed;
                    task.error = error.message.clone();
                });
            }
        }
    }

    async fn run_upload_inner(
        &self,
        task_id: &str,
        run_id: u64,
        task: &UploadTask,
        active: &Rc<ActiveUpload>,
    ) -> Result<(), RequestError> {
        let size = file_size(&task.file);
        let resolved = self.resolve_upload(task, size).await?;
        active.remember_upload(&resolved.upload_id);
        self.set_task_if_current(task_id, run_id, |task| {
            task.upload_id = Some(resolved.upload_id.clone());
        });
        self.save_resume(&resolved.upload_id, &task.parent_id, &task.file);
        ensure_not_cancelled(active)?;
        if resolved.already_completed {
            return Ok(());
        }

        let completed_parts = match resolved.mode {
            UploadMode::Single => {
                if resolved.url.is_empty() {
                    return Err(local_error("服务端没有返回上传地址"));
                }
                let body = file_blob(&task.file).clone();
                let progress = self.progress_callback(
                    task_id.to_owned(),
                    run_id,
                    size,
                    Rc::new(RefCell::new(vec![0_i64])),
                    0,
                );
                self.retrying(Rc::clone(active), || {
                    let active = Rc::clone(active);
                    let body = body.clone();
                    let url = resolved.url.clone();
                    let progress = Rc::clone(&progress);
                    let mime_type = file_mime(&task.file);
                    async move {
                        let etag = xhr_put(active, url, body, mime_type, progress).await?;
                        if etag.trim().is_empty() {
                            return Err(local_error("服务器没有返回上传校验信息"));
                        }
                        Ok(etag)
                    }
                })
                .await?;
                Vec::new()
            }
            UploadMode::Multipart => {
                self.upload_multipart(
                    task_id,
                    run_id,
                    &task.file,
                    &resolved.upload_id,
                    size,
                    resolved.part_size,
                    resolved.part_count,
                    resolved.existing_parts,
                    active,
                )
                .await?
            }
        };

        ensure_not_cancelled(active)?;
        let verifier =
            AbortController::new().map_err(|error| js_error("无法创建提交控制器", error))?;
        let signal = verifier.signal();
        active.verifier.borrow_mut().replace(verifier);
        let result = self
            .retrying(Rc::clone(active), || {
                let signal = signal.clone();
                let request = CompleteUploadRequest {
                    parts: completed_parts.clone(),
                };
                let upload_id = resolved.upload_id.clone();
                async move { api::complete_upload(&upload_id, &request, Some(&signal)).await }
            })
            .await;
        active.verifier.borrow_mut().take();
        let committed = result?;
        if committed.id != resolved.file_id || committed.status != FileStatus::Ready {
            return Err(local_error("服务端返回的文件状态无效"));
        }
        Ok(())
    }

    async fn resolve_upload(
        &self,
        task: &UploadTask,
        size: i64,
    ) -> Result<ResolvedUpload, RequestError> {
        if let Some(upload_id) = task.upload_id.as_deref() {
            match api::fetch_upload(upload_id).await {
                Ok(status) if status.status == UploadLifecycle::Completed => {
                    // The commit endpoint is idempotent. Completing here also
                    // validates that the saved session still belongs to this
                    // file before the task is shown as done.
                    let committed = api::complete_upload(
                        upload_id,
                        &CompleteUploadRequest { parts: Vec::new() },
                        None,
                    )
                    .await?;
                    if committed.id != status.file_id || committed.size != size {
                        return Err(local_error("断点上传对应的文件已发生变化"));
                    }
                    return Ok(ResolvedUpload {
                        upload_id: upload_id.to_owned(),
                        file_id: status.file_id,
                        mode: status.mode,
                        url: status.url,
                        part_size: status.part_size,
                        part_count: status.part_count,
                        existing_parts: status.parts,
                        already_completed: true,
                    });
                }
                Ok(status) if status.status == UploadLifecycle::Pending => {
                    validate_upload_shape(status.mode, status.part_size, status.part_count, size)?;
                    if status.expected_size != size {
                        return Err(local_error("本地文件大小与断点上传不一致"));
                    }
                    return Ok(ResolvedUpload {
                        upload_id: status.upload_id,
                        file_id: status.file_id,
                        mode: status.mode,
                        url: status.url,
                        part_size: status.part_size,
                        part_count: status.part_count,
                        existing_parts: status.parts,
                        already_completed: false,
                    });
                }
                Ok(_) => {
                    self.forget_resume(Some(upload_id));
                }
                Err(error) if error.status == 404 => {
                    self.forget_resume(Some(upload_id));
                }
                Err(error) => return Err(error),
            }
        }

        let created = api::create_upload(&CreateUploadRequest {
            parent_id: task.parent_id.clone(),
            name: task.file.name(),
            size,
            mime_type: file_mime(&task.file),
        })
        .await?;
        validate_upload_shape(created.mode, created.part_size, created.part_count, size)?;
        Ok(ResolvedUpload {
            upload_id: created.upload_id,
            file_id: created.file_id,
            mode: created.mode,
            url: created.url,
            part_size: created.part_size,
            part_count: created.part_count,
            existing_parts: Vec::new(),
            already_completed: false,
        })
    }

    async fn upload_multipart(
        &self,
        task_id: &str,
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
            if index < part_count && part.size == expected && !part.etag.trim().is_empty() {
                sent[index] = expected;
                completed[index] = Some(revaro_core::storage::CompletedPart {
                    part_number: part.part_number,
                    etag: part.etag,
                });
            }
        }
        self.set_task_progress(
            task_id,
            run_id,
            transfer_progress(sent.iter().sum(), total_size),
        );

        let missing: Vec<i32> = completed
            .iter()
            .enumerate()
            .filter_map(|(index, part)| part.is_none().then_some(index as i32 + 1))
            .collect();
        let sent = Rc::new(RefCell::new(sent));
        let completed = Rc::new(RefCell::new(completed));

        for page in missing.chunks(limits::MAX_UPLOAD_PART_BATCH) {
            ensure_not_cancelled(active)?;
            let response = api::fetch_upload_parts(
                upload_id,
                &UploadPartsRequest {
                    part_numbers: page.to_vec(),
                },
            )
            .await?;
            let mut expected = page.iter().copied().collect::<HashSet<_>>();
            for part in &response.parts {
                if part.url.is_empty() || !expected.remove(&part.part_number) {
                    return Err(local_error("服务端返回了无效的分片地址"));
                }
            }
            if !expected.is_empty() {
                return Err(local_error("服务端没有返回全部分片地址"));
            }

            let cursor = Rc::new(Cell::new(0_usize));
            let workers = response.parts.len().min(MULTIPART_CONCURRENCY);
            let mut futures = Vec::with_capacity(workers);
            for _ in 0..workers {
                let controller = self.clone();
                let active = Rc::clone(active);
                let file = file.clone();
                let upload_id = upload_id.to_owned();
                let urls = response.parts.clone();
                let cursor = Rc::clone(&cursor);
                let sent_for_progress = Rc::clone(&sent);
                let sent_for_retry = Rc::clone(&sent);
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
                            let task_id = task_id.to_owned();
                            Rc::new(move |loaded: u64| {
                                let loaded = (loaded as i64).clamp(0, expected);
                                sent.borrow_mut()[index] = loaded;
                                let total = sent.borrow().iter().sum();
                                controller.set_task_progress(
                                    &task_id,
                                    run_id,
                                    transfer_progress(total, total_size),
                                );
                            })
                        };
                        let mime_type = file_mime(&file);
                        let etag = controller
                            .retrying(Rc::clone(&active), || {
                                let active = Rc::clone(&active);
                                let body = body.clone();
                                let url = part.url.clone();
                                let sent = Rc::clone(&sent_for_retry);
                                let progress = Rc::clone(&progress);
                                let mime_type = mime_type.clone();
                                async move {
                                    sent.borrow_mut()[index] = 0;
                                    xhr_put(active, url, body, mime_type, progress).await
                                }
                            })
                            .await?;
                        if etag.trim().is_empty() {
                            return Err(local_error("服务器没有返回分片校验信息"));
                        }
                        controller
                            .retrying(Rc::clone(&active), || {
                                let upload_id = upload_id.clone();
                                let request = RecordUploadPartRequest {
                                    etag: etag.clone(),
                                    size: expected,
                                    content_hash: String::new(),
                                };
                                async move {
                                    api::record_upload_part(&upload_id, part.part_number, &request)
                                        .await
                                }
                            })
                            .await?;
                        completed.borrow_mut()[index] = Some(revaro_core::storage::CompletedPart {
                            part_number: part.part_number,
                            etag,
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
        task_id: String,
        run_id: u64,
        total_size: i64,
        sent: Rc<RefCell<Vec<i64>>>,
        index: usize,
    ) -> Rc<dyn Fn(u64)> {
        let controller = self.clone();
        Rc::new(move |loaded: u64| {
            sent.borrow_mut()[index] = loaded as i64;
            let done = sent.borrow().iter().sum();
            controller.set_task_progress(&task_id, run_id, transfer_progress(done, total_size));
        })
    }

    fn retrying<T, F, Fut>(
        &self,
        active: Rc<ActiveUpload>,
        mut operation: F,
    ) -> impl Future<Output = Result<T, RequestError>>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, RequestError>>,
    {
        async move {
            let mut last = local_error("上传失败");
            for attempt in 0..UPLOAD_RETRIES {
                ensure_not_cancelled(&active)?;
                match operation().await {
                    Ok(value) => return Ok(value),
                    Err(error) => {
                        if active.cancelled.get() {
                            return Err(cancelled_error());
                        }
                        if !retryable(&error) || attempt + 1 == UPLOAD_RETRIES {
                            return Err(error);
                        }
                        last = error;
                        sleep_ms(500_u32.saturating_mul(1_u32 << attempt.min(4))).await?;
                    }
                }
            }
            Err(last)
        }
    }

    fn set_task_progress(&self, task_id: &str, run_id: u64, progress: u8) {
        self.set_task_if_current(task_id, run_id, |task| task.progress = progress.min(98));
    }

    fn set_task_if_current<F>(&self, task_id: &str, run_id: u64, update: F)
    where
        F: FnOnce(&mut UploadTask),
    {
        self.tasks.update(|tasks| {
            if let Some(task) = tasks
                .iter_mut()
                .find(|task| task.id == task_id && (run_id == 0 || task.run_id == run_id))
            {
                update(task);
            }
        });
    }

    fn save_resume(&self, upload_id: &str, parent_id: &str, file: &BrowserFile) {
        let mut saved = saved_uploads();
        saved.retain(|entry| entry.upload_id != upload_id);
        saved.push(SavedUpload {
            upload_id: upload_id.to_owned(),
            parent_id: parent_id.to_owned(),
            name: file.name(),
            size: file_size(file),
            last_modified: file.last_modified(),
        });
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
            Closure::once_into_js(move || controller.refresh_if_current(parent_id, None));
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
///
/// The reference client does not render a second foreground upload queue in
/// the file browser. Upload progress is represented by the durable task
/// centre; the browser-local queue remains an implementation detail so that
/// it can cancel, retry and resume byte transfers without changing the shell
/// layout.
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

async fn ensure_upload_directory(parent_id: &str, name: &str) -> Result<ModelFile, RequestError> {
    match api::create_directory(&CreateDirectoryRequest {
        parent_id: parent_id.to_owned(),
        name: name.to_owned(),
    })
    .await
    {
        Ok(folder) => Ok(folder),
        Err(error) if error.status == 409 => {
            let children = api::fetch_children(parent_id).await?;
            if let Some(folder) = children
                .items
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
        UploadMode::Single if limits::uses_multipart_upload(total_size) => {
            Err(local_error("服务端返回了错误的上传模式"))
        }
        UploadMode::Single if part_count != 0 || part_size_value <= 0 => {
            Err(local_error("服务端返回了无效的单文件上传参数"))
        }
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
    serde_json::from_str::<Vec<SavedUpload>>(&raw)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| {
            !entry.upload_id.is_empty()
                && !entry.parent_id.is_empty()
                && !entry.name.is_empty()
                && entry.size >= 0
                && entry.last_modified.is_finite()
        })
        .collect()
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

fn retryable(error: &RequestError) -> bool {
    error.status == 0 || error.status == 408 || error.status == 429 || error.status >= 500
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

async fn sleep_ms(milliseconds: u32) -> Result<(), RequestError> {
    let Some(window) = web_sys::window() else {
        return Err(local_error("浏览器计时器不可用"));
    };
    let (sender, receiver) = oneshot::channel();
    let callback = Closure::once_into_js(move || {
        let _ = sender.send(());
    });
    window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.unchecked_ref(),
            i32::try_from(milliseconds).unwrap_or(i32::MAX),
        )
        .map_err(|error| js_error("无法安排上传重试", error))?;
    receiver
        .await
        .map_err(|_| local_error("上传重试计时器已关闭"))
}

async fn xhr_put(
    active: Rc<ActiveUpload>,
    url: String,
    body: Blob,
    content_type: String,
    on_progress: Rc<dyn Fn(u64)>,
) -> Result<String, RequestError> {
    let xhr = XmlHttpRequest::new().map_err(|error| js_error("无法创建上传请求", error))?;
    xhr.open_with_async("PUT", &url, true)
        .map_err(|error| js_error("无法打开上传请求", error))?;
    xhr.set_with_credentials(true);
    xhr.set_request_header("Content-Type", &content_type)
        .map_err(|error| js_error("无法设置上传请求头", error))?;
    let upload = xhr
        .upload()
        .map_err(|error| js_error("无法监听上传进度", error))?;

    let (sender, receiver) = oneshot::channel::<Result<String, RequestError>>();
    let sender = Rc::new(RefCell::new(Some(sender)));
    let onload = {
        let sender = Rc::clone(&sender);
        let xhr = xhr.clone();
        Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let result = match xhr.status() {
                Ok(status) if (200..300).contains(&status) => xhr
                    .get_response_header("ETag")
                    .map(|etag| etag.unwrap_or_default())
                    .map_err(|error| js_error("无法读取上传校验信息", error)),
                Ok(status) => Err(xhr_error(&xhr, status)),
                Err(error) => Err(js_error("无法读取上传状态", error)),
            };
            finish_xhr(&sender, result);
        })
    };
    let onerror = {
        let sender = Rc::clone(&sender);
        Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            finish_xhr(&sender, Err(local_error("无法连接服务器，请检查网络")));
        })
    };
    let onabort = {
        let sender = Rc::clone(&sender);
        Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            finish_xhr(&sender, Err(cancelled_error()));
        })
    };
    let onprogress = {
        let on_progress = Rc::clone(&on_progress);
        Closure::<dyn FnMut(ProgressEvent)>::new(move |event: ProgressEvent| {
            if event.length_computable() {
                on_progress(event.loaded().max(0.0) as u64);
            }
        })
    };
    xhr.set_onload(Some(onload.as_ref().unchecked_ref()));
    xhr.set_onerror(Some(onerror.as_ref().unchecked_ref()));
    xhr.set_onabort(Some(onabort.as_ref().unchecked_ref()));
    upload.set_onprogress(Some(onprogress.as_ref().unchecked_ref()));
    active.requests.borrow_mut().push(xhr.clone());

    let send_result = xhr
        .send_with_opt_blob(Some(&body))
        .map_err(|error| js_error("无法发送上传数据", error));
    if let Err(error) = send_result {
        active
            .requests
            .borrow_mut()
            .retain(|request| request != &xhr);
        return Err(error);
    }
    let result = receiver.await.map_err(|_| local_error("上传请求已关闭"))?;
    upload.set_onprogress(None);
    xhr.set_onload(None);
    xhr.set_onerror(None);
    xhr.set_onabort(None);
    active
        .requests
        .borrow_mut()
        .retain(|request| request != &xhr);
    result
}

fn finish_xhr(
    sender: &Rc<RefCell<Option<oneshot::Sender<Result<String, RequestError>>>>>,
    result: Result<String, RequestError>,
) {
    if let Some(sender) = sender.borrow_mut().take() {
        let _ = sender.send(result);
    }
}

fn xhr_error(xhr: &XmlHttpRequest, status: u16) -> RequestError {
    let body = xhr.response_text().ok().flatten().unwrap_or_default();
    if let Ok(envelope) = serde_json::from_str::<revaro_core::ErrorEnvelope>(&body) {
        return RequestError {
            status,
            code: envelope.error.code,
            message: envelope.error.message,
        };
    }
    RequestError {
        status,
        code: None,
        message: format!("上传失败 ({status})"),
    }
}
