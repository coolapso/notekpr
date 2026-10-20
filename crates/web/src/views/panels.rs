//! Side panel for the open note: backlinks, links, outline and tags.

use leptos::prelude::*;

use crate::knowledge;
use crate::state::{self, Modal, Route, step, ui};
use crate::views::icons::Icon;

#[component]
pub fn RightPanel(vault: String, note: String) -> impl IntoView {
    let ui = ui();
    let text = {
        let (v, n) = (vault.clone(), note.clone());
        move || {
            ui.content_rev.get();
            step(|c| c.replicas.get(&v).map(|r| r.note_text(&n))).unwrap_or_default()
        }
    };
    let path = {
        let (v, n) = (vault.clone(), note.clone());
        move || {
            ui.tree_rev.get();
            state::step(|c| c.entry(&v, &n))
                .map(|e| e.path)
                .unwrap_or_default()
        }
    };
    let (v1, n1) = (vault.clone(), note.clone());
    let backlinks = Memo::new(move |_| {
        ui.content_rev.get();
        ui.tree_rev.get();
        knowledge::backlinks(&v1, &n1)
            .into_iter()
            .map(|b| (b.id, b.path, b.snippet))
            .collect::<Vec<_>>()
    });
    let (v2, n2) = (vault.clone(), note.clone());
    let text2 = text.clone();
    let path2 = path.clone();
    let outgoing = Memo::new(move |_| knowledge::outgoing(&v2, &n2, &path2(), &text2()));
    let (v3, n3) = (vault.clone(), note.clone());
    let text3 = text.clone();
    let path3 = path.clone();
    let broken = Memo::new(move |_| knowledge::broken_links(&v3, &n3, &path3(), &text3()));
    let (v4, n4) = (vault.clone(), note.clone());
    let text4 = text.clone();
    let outline = Memo::new(move |_| knowledge::outline(&v4, &n4, &text4()));
    let (v5, n5) = (vault.clone(), note.clone());
    let text5 = text.clone();
    let tags = Memo::new(move |_| knowledge::analysis(&v5, &n5, &text5()).tags.clone());
    let vb = vault.clone();
    let vo = vault.clone();
    let vbr = vault.clone();
    let pbr = path.clone();

    view! {
        <aside class="panel" aria-label="Note details">
            <section>
                <h3><Icon name="link"/>" Backlinks " <span class="count">{move || backlinks.get().len()}</span></h3>
                {move || {
                    let b = backlinks.get();
                    if b.is_empty() {
                        view! { <p class="muted small">"No other notes link here yet. Type " <code>"[[" </code>" in a note to link."</p> }.into_any()
                    } else {
                        b.into_iter().map(|(id, path, snippet)| {
                            let v = vb.clone();
                            view! {
                                <button class="panel-item" on:click=move |_| state::navigate(Route::Note(v.clone(), id.clone()))>
                                    <span class="pi-title">{coolpsonotes_core::paths::title(&path).to_string()}</span>
                                    <span class="pi-snip">{snippet}</span>
                                </button>
                            }
                        }).collect_view().into_any()
                    }
                }}
            </section>
            <Show when=move || !outgoing.get().is_empty()>
                <section>
                    <h3><Icon name="file"/>" Links in this note"</h3>
                    {
                        let vo = vo.clone();
                        move || outgoing.get().into_iter().map(|(id, path)| {
                            let v = vo.clone();
                            view! { <button class="panel-item" on:click=move |_| state::navigate(Route::Note(v.clone(), id.clone()))>
                                <span class="pi-title">{coolpsonotes_core::paths::title(&path).to_string()}</span></button> }
                        }).collect_view()
                    }
                </section>
            </Show>
            <Show when=move || !broken.get().is_empty()>
                <section>
                    <h3 class="warn"><Icon name="alert"/>" Unresolved links"</h3>
                    {
                        let vbr = vbr.clone();
                        let pbr = pbr.clone();
                        move || broken.get().into_iter().map(|t| {
                            let (v, p) = (vbr.clone(), pbr.clone());
                            let t2 = t.clone();
                            view! { <button class="panel-item" title="Create this note" aria-label="Create this note" on:click=move |_| crate::actions::open_wiki(&v, &t2, &p())>
                                <span class="pi-title broken">{t}</span><span class="pi-snip">"click to create"</span></button> }
                        }).collect_view()
                    }
                </section>
            </Show>
            <Show when=move || !outline.get().is_empty()>
                <section>
                    <h3><Icon name="list"/>" Outline"</h3>
                    {move || outline.get().into_iter().map(|(lvl, text, off)| {
                        view! {
                            <button class="panel-item outline" style=format!("padding-left:{}px", 6 + (lvl as usize - 1) * 12)
                                on:click=move |_| {
                                    if let Some(ed) = step(|c| c.editor.as_ref().map(|(_, _, e)| e.clone())) {
                                        ed.focus();
                                        ed.set_selection(off, off);
                                    }
                                }>{text}</button>
                        }
                    }).collect_view()}
                </section>
            </Show>
            <Show when=move || !tags.get().is_empty()>
                <section>
                    <h3><Icon name="hash"/>" Tags"</h3>
                    <div class="tags">
                        {move || tags.get().into_iter().map(|t| {
                            let q = format!("tag:{t}");
                            view! { <button class="chip" on:click=move |_| ui.modal.set(Some(Modal::Search(q.clone())))>{format!("#{t}")}</button> }
                        }).collect_view()}
                    </div>
                </section>
            </Show>
        </aside>
    }
}
