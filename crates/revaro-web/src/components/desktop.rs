//! Desktop window manager. Mounted app workspaces survive task and viewport changes.
use super::icons;
use crate::{browser, logic::library::LibraryPage};
use leptos::prelude::*;
use revaro_core::model::File;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};

pub(super) const APPS: [LibraryPage; 7] = [
    LibraryPage::Files,
    LibraryPage::Books,
    LibraryPage::Music,
    LibraryPage::Gallery,
    LibraryPage::Videos,
    LibraryPage::Home,
    LibraryPage::Trash,
];

pub(super) fn app_path(app: LibraryPage) -> &'static str {
    if app == LibraryPage::Home {
        "/recent"
    } else {
        app.path()
    }
}
pub(super) fn pathname() -> String {
    window().location().pathname().unwrap_or_default()
}
fn set_route(path: &str, push: bool) {
    if let Ok(history) = window().history() {
        if push && pathname() != path {
            let _ = history.push_state_with_url(&JsValue::NULL, "", Some(path));
        } else if !push {
            let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(path));
        }
    }
}
fn viewport() -> (f64, f64) {
    (
        window()
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(1280.),
        window()
            .inner_height()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(800.),
    )
}
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AppTask {
    pub app: LibraryPage,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub minimized: bool,
    pub closed: bool,
    pub maximized: bool,
    pub z: u32,
    pub path: String,
}
#[derive(Clone, Copy)]
pub(super) struct OsState {
    pub tasks: RwSignal<Vec<AppTask>>,
    pub active: RwSignal<Option<LibraryPage>>,
    pub mobile: RwSignal<bool>,
    pub recents: RwSignal<bool>,
    pub refresh: RwSignal<u64>,
    pub back: RwSignal<Option<Callback<()>>>,
    pub pending_book: RwSignal<Option<File>>,
}
impl OsState {
    pub fn new() -> Self {
        let mobile = window()
            .match_media("(max-width: 850px)")
            .ok()
            .flatten()
            .is_some_and(|q| q.matches());
        let state = Self {
            tasks: RwSignal::new(Vec::new()),
            active: RwSignal::new(None),
            mobile: RwSignal::new(mobile),
            recents: RwSignal::new(false),
            refresh: RwSignal::new(0),
            back: RwSignal::new(None),
            pending_book: RwSignal::new(None),
        };
        let mut resize = browser::on_resize(move |_| {
            state.mobile.set(
                window()
                    .match_media("(max-width: 850px)")
                    .ok()
                    .flatten()
                    .is_some_and(|q| q.matches()),
            );
            if !state.mobile.get_untracked() {
                let (w, h) = viewport();
                state.tasks.update(|tasks| {
                    for task in tasks {
                        task.width = task.width.min((w - 32.).max(360.));
                        task.height = task.height.min((h - 140.).max(240.));
                        task.x = task.x.clamp(8., (w - task.width - 8.).max(8.));
                        task.y = task.y.clamp(48., (h - task.height - 88.).max(48.));
                    }
                });
            }
        });
        on_cleanup(move || resize.release());
        let mut popstate = browser::on_popstate(move |_| {
            let path = pathname();
            if path == "/" {
                state.home(false);
            } else {
                state.open(LibraryPage::from_path(&path), false);
            }
        });
        on_cleanup(move || popstate.release());
        if pathname() != "/" {
            state.open(LibraryPage::from_path(&pathname()), false);
        }
        state
    }
    fn remember(self) {
        if let Some(app) = self.active.get_untracked() {
            let path = pathname();
            self.tasks.update(|tasks| {
                if let Some(task) = tasks.iter_mut().find(|t| t.app == app) {
                    task.path = path;
                }
            });
        }
    }
    pub fn open(self, app: LibraryPage, push: bool) {
        let was_active = self.active.get_untracked() == Some(app);
        if push {
            self.remember();
        }
        let (vw, vh) = viewport();
        let mut path = app_path(app).to_owned();
        self.tasks.update(|tasks| {
            tasks.sort_by_key(|t| t.z);
            for (index, task) in tasks.iter_mut().enumerate() {
                task.z = index as u32 + 1;
            }
            let z = tasks.len() as u32 + 1;
            if let Some(task) = tasks.iter_mut().find(|t| t.app == app) {
                task.minimized = false;
                task.closed = false;
                task.z = z;
                path = task.path.clone();
                if !push {
                    task.path = pathname();
                }
            } else {
                let width = (if app == LibraryPage::Music {
                    680_f64
                } else {
                    820_f64
                })
                .min((vw - 100.).max(360.));
                let height = 560_f64.min((vh - 170.).max(240.));
                let offset = tasks.len() as f64 * 26.;
                tasks.push(AppTask {
                    app,
                    width,
                    height,
                    x: ((vw - width) / 2. - 55. + offset).clamp(8., (vw - width - 8.).max(8.)),
                    y: (90. + offset).min((vh - height - 95.).max(48.)),
                    minimized: false,
                    closed: false,
                    maximized: false,
                    z,
                    path: if push { path.clone() } else { pathname() },
                });
            }
        });
        self.active.set(Some(app));
        self.recents.set(false);
        if !was_active {
            // Viewport-based readers need their visible dimensions after a task resumes.
            let callback = Closure::once(move || {
                if let Ok(event) = web_sys::Event::new("resize") {
                    let _ = window().dispatch_event(&event);
                }
            })
            .into_js_value();
            let _ = window().request_animation_frame(callback.unchecked_ref());
        }
        if push {
            set_route(&path, true);
        }
    }
    pub fn home(self, push: bool) {
        if push {
            self.remember();
        }
        self.active.set(None);
        self.recents.set(false);
        if !self.mobile.get_untracked() {
            self.tasks.update(|tasks| {
                for task in tasks {
                    task.minimized = true;
                }
            });
        }
        if push {
            set_route("/", true);
        }
    }
    pub fn hide(self, app: LibraryPage, close: bool) {
        self.remember();
        self.tasks.update(|tasks| {
            if let Some(task) = tasks.iter_mut().find(|t| t.app == app) {
                task.minimized = true;
                task.closed = close;
            }
        });
        let next = self.tasks.with_untracked(|tasks| {
            tasks
                .iter()
                .filter(|t| !t.minimized && !t.closed)
                .max_by_key(|t| t.z)
                .map(|t| t.app)
        });
        if let Some(next) = next {
            self.open(next, true);
        } else {
            self.active.set(None);
            set_route("/", true);
        }
    }
    pub fn maximize(self, app: LibraryPage) {
        self.tasks.update(|tasks| {
            if let Some(t) = tasks.iter_mut().find(|t| t.app == app) {
                t.maximized = !t.maximized;
            }
        });
    }
    pub fn recent_tasks(self) -> Vec<AppTask> {
        let mut tasks = self
            .tasks
            .get()
            .into_iter()
            .filter(|t| !t.closed)
            .collect::<Vec<_>>();
        tasks.sort_by_key(|t| std::cmp::Reverse(t.z));
        tasks
    }
}

pub(super) fn app_bounds(state: OsState, app: LibraryPage) -> String {
    if state.mobile.get() {
        return "--app-left:0px;--app-top:calc(44px + env(safe-area-inset-top,0px));--app-width:100vw;--app-height:calc(100dvh - 101px - env(safe-area-inset-top,0px) - env(safe-area-inset-bottom,0px));--app-z:30".to_owned();
    }
    state.tasks.with(|tasks| tasks.iter().find(|task|task.app==app).map(|task| {
        if task.maximized { format!("--app-left:12px;--app-top:87px;--app-width:calc(100vw - 24px);--app-height:calc(100dvh - 201px);--app-z:{}",task.z+20) }
        else {format!("--app-left:{}px;--app-top:{}px;--app-width:{}px;--app-height:{}px;--app-z:{}",task.x,task.y+39.,task.width,task.height-59.,task.z+20)}
    }).unwrap_or_default())
}

pub(super) fn app_icon(page: LibraryPage) -> AnyView {
    match page {
        LibraryPage::Home => icons::rotate_ccw().into_any(),
        LibraryPage::Books => icons::book_open().into_any(),
        LibraryPage::Music => icons::music_2().into_any(),
        LibraryPage::Gallery => icons::image().into_any(),
        LibraryPage::Videos => icons::video().into_any(),
        LibraryPage::Files => icons::folder().into_any(),
        LibraryPage::Trash => icons::trash().into_any(),
    }
}
pub(super) fn app_name(page: LibraryPage) -> &'static str {
    match page {
        LibraryPage::Home => "最近使用",
        LibraryPage::Books => "阅读",
        LibraryPage::Music => "音乐",
        LibraryPage::Gallery => "图库",
        LibraryPage::Videos => "视频",
        LibraryPage::Files => "文件",
        LibraryPage::Trash => "回收站",
    }
}
#[component]
pub(super) fn SystemClock(#[prop(optional)] widget: bool) -> impl IntoView {
    let now = RwSignal::new(js_sys::Date::now());
    let tick = Closure::<dyn FnMut()>::new(move || {
        let _ = now.try_set(js_sys::Date::now());
    });
    let timer = window()
        .set_interval_with_callback_and_timeout_and_arguments_0(tick.as_ref().unchecked_ref(), 1000)
        .ok();
    let cleanup = leptos::__reexports::send_wrapper::SendWrapper::new(tick);
    on_cleanup(move || {
        if let Some(id) = timer {
            window().clear_interval_with_handle(id);
        }
        drop(cleanup);
    });
    view! {<div class="os-clock" class:clock-widget=widget><time>{move ||{let d=js_sys::Date::new(&now.get().into());format!("{:02}:{:02}",d.get_hours(),d.get_minutes())}}</time><span>{move ||{let d=js_sys::Date::new(&now.get().into());format!("{}月{}日  星期{}",d.get_month()+1,d.get_date(),["日","一","二","三","四","五","六"][d.get_day() as usize])}}</span></div>}
}
#[component]
pub(super) fn AppLauncher(
    app: LibraryPage,
    state: OsState,
    #[prop(optional)] dock: bool,
) -> impl IntoView {
    view! {<a class="os-app-launcher" class:dock-launcher=dock href=app_path(app) aria-label=app_name(app) title=app_name(app)
    class:running=move ||state.tasks.with(|tasks|tasks.iter().any(|t|t.app==app&&!t.closed)) class:active=move ||state.active.get()==Some(app) aria-current=move ||(state.active.get()==Some(app)).then_some("page")
    on:click=move |event:web_sys::MouseEvent|{if event.button()==0&&!event.ctrl_key()&&!event.meta_key()&&!event.shift_key()&&!event.alt_key(){event.prevent_default();state.open(app,true);}}>
    <span class="os-app-icon" data-app=app_path(app)>{app_icon(app)}</span><span class="os-app-label">{app_name(app)}</span></a>}
}
#[component]
pub(super) fn DesktopShell(state: OsState) -> impl IntoView {
    view! {
        <div class="desktop-shell" data-shell="desktop">
            <header class="os-system-bar"><button class="os-system-brand" aria-label="显示桌面" on:click=move |_|state.home(true)><img src="/revaro-logo.svg" alt=""/>"revaro"<small>"OS"</small></button><span class="os-system-app">{move ||state.active.get().map(app_name).unwrap_or("桌面")}</span><div class="os-system-right"><button class="os-task-button" aria-label="切换任务" on:click=move |_|state.recents.update(|v|*v=!*v)>{icons::layout_grid()}</button><SystemClock /></div></header>
            <main class="os-desktop-home" aria-label="桌面主屏幕"><div class="desktop-widget"><span class="widget-eyebrow">"你的私人空间"</span><SystemClock widget=true/><p>"书、音乐，还有值得留下的瞬间。"</p></div><div class="desktop-app-grid" role="group" aria-label="桌面应用"><For each=||APPS key=|app|app_path(*app) children=move |app|view!{<AppLauncher app=app state=state/>}/></div></main>
            <nav class="os-dock" aria-label="主导航"><button class="dock-home" aria-label="显示桌面" title="显示桌面" on:click=move |_|state.home(true)>{icons::home()}</button><For each=||APPS key=|app|app_path(*app) children=move |app|view!{<AppLauncher app=app state=state dock=true/>}/></nav>
        </div>
        <Show when=move ||state.recents.get()><RecentTasks state=state/></Show>
    }
}
#[component]
pub(super) fn RecentTasks(state: OsState) -> impl IntoView {
    view! {<div class="os-task-backdrop" on:click=move |_|state.recents.set(false)><section class="os-task-switcher" role="dialog" aria-label="最近任务" aria-modal="true" on:click=move |e|e.stop_propagation()><header><h2>"最近任务"</h2><button aria-label="关闭最近任务" on:click=move |_|state.recents.set(false)>{icons::x()}</button></header><div class="os-task-grid"><For each=move ||state.recent_tasks() key=|t|app_path(t.app) children=move |task|view!{<button class="os-task-card" aria-label=format!("切换到{}",app_name(task.app)) on:click=move |_|state.open(task.app,true)><span class="os-app-icon" data-app=app_path(task.app)>{app_icon(task.app)}</span><strong>{app_name(task.app)}</strong><small>{if task.minimized{"已最小化"}else{"继续使用"}}</small></button>}/><Show when=move ||state.recent_tasks().is_empty()><p class="os-no-tasks">"打开的应用会出现在这里"</p></Show></div><button class="os-return-home" on:click=move |_|state.home(true)>"返回主屏幕"</button></section></div>}
}
#[component]
pub(super) fn AppWindow(app: LibraryPage, state: OsState, children: Children) -> impl IntoView {
    let frame = NodeRef::<leptos::html::Section>::new();
    let task = Memo::new(move |_| {
        state
            .tasks
            .with(|tasks| tasks.iter().find(|t| t.app == app).cloned())
            .expect("mounted task")
    });
    let gesture = RwSignal::new(None::<(bool, f64, f64, f64, f64, f64, f64)>);
    let begin = Callback::new(move |(event, resize): (web_sys::PointerEvent, bool)| {
        if event.button() != 0 || state.mobile.get_untracked() || task.get_untracked().maximized {
            return;
        }
        if !resize
            && event
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                .is_some_and(|t| t.closest("button").ok().flatten().is_some())
        {
            return;
        }
        event.prevent_default();
        state.open(app, true);
        let t = task.get_untracked();
        gesture.set(Some((
            resize,
            f64::from(event.client_x()),
            f64::from(event.client_y()),
            t.x,
            t.y,
            t.width,
            t.height,
        )));
    });
    let moving = window_event_listener(leptos::ev::pointermove, move |event| {
        if let Some((resize, sx, sy, x, y, w, h)) = gesture.get_untracked() {
            let (vw, vh) = viewport();
            let dx = f64::from(event.client_x()) - sx;
            let dy = f64::from(event.client_y()) - sy;
            state.tasks.update(|tasks| {
                if let Some(t) = tasks.iter_mut().find(|t| t.app == app) {
                    if resize {
                        t.width = (w + dx).clamp(360., (vw - x - 8.).max(360.));
                        t.height = (h + dy).clamp(240., (vh - y - 88.).max(240.));
                    } else {
                        t.x = (x + dx).clamp(8., (vw - w - 8.).max(8.));
                        t.y = (y + dy).clamp(48., (vh - h - 88.).max(48.));
                    }
                }
            });
        }
    });
    let end = window_event_listener(leptos::ev::pointerup, move |_| gesture.set(None));
    on_cleanup(move || {
        moving.remove();
        end.remove();
    });
    view! {<section node_ref=frame class="os-app-window" class:focused=move ||state.active.get()==Some(app) class:is-maximized=move ||task.get().maximized
        data-app=app_path(app) aria-label=format!("{}应用",app_name(app)) aria-hidden=move ||(state.active.get()!=Some(app)).to_string()
        style=move ||{let t=task.get();if state.mobile.get(){if state.active.get()!=Some(app){"display:none".to_owned()}else{"".to_owned()}}else if t.minimized||t.closed{"display:none".to_owned()}else if t.maximized{format!("left:12px;top:48px;width:calc(100vw - 24px);height:calc(100dvh - 142px);z-index:{}",t.z+10)}else{format!("left:{}px;top:{}px;width:{}px;height:{}px;z-index:{}",t.x,t.y,t.width,t.height,t.z+10)}}
        on:pointerdown=move |_|{if state.active.get_untracked()!=Some(app){state.open(app,true);}}>
        <header class="os-window-chrome" on:pointerdown=move |event|begin.run((event,false)) on:dblclick=move |_|state.maximize(app)><div class="os-window-controls" on:dblclick=move |event|event.stop_propagation()><button class="os-window-dot close" aria-label="关闭窗口" on:click=move |_|state.hide(app,true)>{icons::x()}</button><button class="os-window-dot minimize" aria-label="最小化窗口" on:click=move |_|state.hide(app,false)>{icons::minus()}</button><button class="os-window-dot maximize" aria-label=move ||if task.get().maximized{"还原窗口"}else{"最大化窗口"} on:click=move |_|state.maximize(app)>{icons::maximize()}</button></div><div class="os-window-title">{app_icon(app)}<span>{app_name(app)}</span></div><span class="os-window-hint">"revaro"</span></header>
        {children()}<button class="os-window-resize" aria-label="调整窗口大小" on:pointerdown=move |event|begin.run((event,true)) on:keydown=move |event|{if !matches!(event.key().as_str(),"ArrowRight"|"ArrowLeft"|"ArrowDown"|"ArrowUp"){return;}event.prevent_default();let delta=if event.shift_key(){40.}else{10.};let(vw,vh)=viewport();state.tasks.update(|tasks|if let Some(t)=tasks.iter_mut().find(|t|t.app==app){match event.key().as_str(){"ArrowRight"=>t.width+=delta,"ArrowLeft"=>t.width-=delta,"ArrowDown"=>t.height+=delta,"ArrowUp"=>t.height-=delta,_=>()}t.width=t.width.clamp(360.,(vw-t.x-8.).max(360.));t.height=t.height.clamp(240.,(vh-t.y-88.).max(240.));});}/>
    </section>}
}
