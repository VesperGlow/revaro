//! Touch shell with its own home screen, app navigation and recent tasks.
use super::{
    desktop::{APPS, AppLauncher, OsState, RecentTasks, SystemClock, app_path},
    icons,
};
use leptos::prelude::*;
#[component]
pub(super) fn MobileShell(state: OsState) -> impl IntoView {
    view! {
        <div class="mobile-shell" data-shell="mobile">
            <header class="mobile-status-bar"><SystemClock/><span class="mobile-status-icons" aria-hidden="true">{icons::volume_2()}<span class="mobile-battery"></span></span></header>
            <Show when=move ||state.active.get().is_none()><main class="mobile-home" aria-label="手机主屏幕"><div class="mobile-clock-widget"><SystemClock widget=true/><p>"给喜欢的事物，留一点时间。"</p></div><div class="mobile-app-grid" role="group" aria-label="手机应用"><For each=||APPS key=|app|app_path(*app) children=move |app|view!{<AppLauncher app=app state=state/>}/></div></main><nav class="mobile-home-dock" aria-label="移动端导航"><For each=||APPS[..4].to_vec() key=|app|app_path(*app) children=move |app|view!{<AppLauncher app=app state=state dock=true/>}/></nav></Show>
            <Show when=move ||state.active.get().is_some()><nav class="mobile-app-controls" aria-label="应用导航"><button aria-label="返回上一级" on:click=move |_|{if let Some(back)=state.back.get_untracked(){back.run(());}else{state.home(true);}}>{icons::chevron_left()}</button><button class="mobile-home-button" aria-label="返回主屏幕" on:click=move |_|state.home(true)><span></span></button><button aria-label="最近任务" on:click=move |_|state.recents.set(true)>{icons::gallery_horizontal_end()}</button></nav></Show>
            <Show when=move ||state.active.get().is_none()><button class="mobile-recent-button" aria-label="最近任务" on:click=move |_|state.recents.set(true)>{icons::gallery_horizontal_end()}</button></Show>
        </div>
        <Show when=move ||state.recents.get()><RecentTasks state=state/></Show>
    }
}
