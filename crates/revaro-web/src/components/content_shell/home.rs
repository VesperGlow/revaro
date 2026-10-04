//! Home sections and recent-content loading.

use super::*;

#[component]
pub(super) fn HomeDashboard(
    refresh: RwSignal<u64>,
    on_open: Callback<File>,
    on_navigate: Callback<LibraryPage>,
    on_import: Callback<()>,
    on_unauthorized: Callback<()>,
) -> impl IntoView {
    let sections = RwSignal::new(Vec::<(LibraryPage, Vec<LibraryItem>)>::new());
    let selection = expect_context::<ShellContext>().selection;
    let error = RwSignal::new(String::new());
    let generation = RwSignal::new(0_u64);
    Effect::new(move |_| {
        let _ = refresh.get();
        generation.update(|g| *g += 1);
        let gen_id = generation.get_untracked();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let mut list = Vec::new();
            for page in [
                LibraryPage::Books,
                LibraryPage::Music,
                LibraryPage::Gallery,
                LibraryPage::Videos,
            ] {
                let base = LibraryQuery {
                    kind: page.kind().to_owned(),
                    ..Default::default()
                };
                match api::fetch_library(&base).await {
                    Ok(all) => {
                        let recent = if matches!(page, LibraryPage::Books | LibraryPage::Music) {
                            api::fetch_library(&LibraryQuery {
                                recent: true,
                                ..base
                            })
                            .await
                            .ok()
                            .filter(|r| !r.items.is_empty())
                            .map(|r| r.items)
                        } else {
                            None
                        };
                        list.push((
                            page,
                            recent
                                .unwrap_or(all.items)
                                .into_iter()
                                .take(6)
                                .collect::<Vec<_>>(),
                        ));
                    }
                    Err(e) => {
                        if e.is_unauthorized() {
                            on_unauthorized.run(());
                        } else {
                            error.set(e.message);
                        }
                        return;
                    }
                }
            }
            if generation.try_get_untracked() == Some(gen_id) {
                error.set(String::new());
                selection.set_library_items(
                    list.iter()
                        .flat_map(|(_, items)| items.iter().map(|item| item.file.clone()))
                        .collect(),
                );
                sections.set(list);
            }
        });
    });
    view! {


        <For each=move ||sections.get() key=move |(p,_)|(p.path(),refresh.get_untracked()) children=move |(p,list)|view!{
            <section class="home-section"><header><div><h2>{match p{LibraryPage::Books=>"继续阅读",LibraryPage::Music=>"最近播放与收藏",LibraryPage::Videos=>"最近添加的视频",_=>"最近添加的图片"}}</h2></div><button on:click=move |_|on_navigate.run(p)>"查看全部"<span>"→"</span></button></header>
                <div class="home-content-row" class:selection-mode=move || selection.enabled.get() class:home-books=p==LibraryPage::Books class:home-images=matches!(p,LibraryPage::Gallery | LibraryPage::Videos)>
                    {if list.is_empty(){view!{<button class="home-empty" on:click=move |_|on_import.run(())>"还没有内容，导入你的第一份收藏 →"</button>}.into_any()}else{list.into_iter().map(|item|{let file=item.file.clone();let selected_id=file.id.clone();let card_background_id=file.id.clone();view!{<article class="home-card" data-selection-ids=serde_json::to_string(&vec![item.file.id.clone()]).unwrap_or_default() on:click=move |event| selection.toggle_from_card_background(event, &card_background_id) class:selected=move || selection.ids.with(|ids| ids.contains(&selected_id))><SelectionCheckbox id=item.file.id.clone() name=item.file.name.clone() selection=selection /><button class="home-item" aria-label=format!("打开 {}",item.file.name) on:click=move |_|on_open.run(file.clone())><LibraryCover item=item.clone()/><CardInfo name=display_title(&item.file.name) detail=Signal::derive(move ||if item.last_opened.is_some(){"继续打开".to_owned()}else{"新加入你的内容库".to_owned()}) /></button></article>}}).collect_view().into_any()}}
                </div>
            </section>
        } />
        <Show when=move ||!error.get().is_empty() fallback=|| ()><div class="library-error" role="alert">{move ||error.get()}<button on:click=move |_|refresh.update(|r|*r+=1)>"重试"</button></div></Show>
    }
}
