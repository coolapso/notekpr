//! The mentions inbox (docs/comments.md §1): comments that mention us,
//! newest first, with how many are unread; one opens its thread.

use leptos::prelude::*;

use super::avatar;
use super::icons::Icon;
use crate::state::{self, step, ui};
use crate::util;

/// The sidebar's mentions button and its list.
#[component]
pub fn MentionsButton() -> impl IntoView {
    let ui = ui();
    let open = RwSignal::new(false);
    super::close_on_outside(open);
    let list = Memo::new(move |_| {
        ui.mentions_rev.track();
        ui.comments_rev.track();
        ui.prefs_rev.track();
        ui.tree_rev.track();
        step(|c| state::my_mentions(c))
    });
    let unread = Memo::new(move |_| list.get().iter().filter(|m| m.unread).count());
    // Who mentioned us, by name.
    Effect::new(move |_| {
        if list.with(|l| l.iter().any(|m| !m.name_known)) {
            leptos::task::spawn_local(state::learn_mention_authors());
        }
    });
    view! {
        <div class="tb-drop sb-mentions">
            <button class="icon-btn cm-count" class:has={move || unread.get() > 0}
                title=move || { match unread.get() { 0 => "Mentions".to_string(), n => util::count(n, "new mention", "new mentions") } }
                aria-label="Mentions" aria-haspopup="true" on:click=move |_| open.update(|o| *o = !*o)>
                <Icon name="at"/>{move || (unread.get() > 0).then(|| view! { <span class="badge">{unread.get()}</span> })}
            </button>
            <Show when=move || open.get()>
                <div class="menu mentions-menu" role="dialog" aria-label="Mentions">
                    <div class="mentions-head">
                        <b class="grow">"Mentions"</b>
                        {move || (unread.get() > 0).then(|| view! {
                            <button class="linkish small" on:click=move |_| step(state::mark_all_mentions_read)>"Mark all as read"</button>
                        })}
                    </div>
                    {move || {
                        let all = list.get();
                        if all.is_empty() {
                            return view! {
                                <p class="muted small mentions-none">"Nobody has mentioned you yet. When someone writes @ and your name in a comment, it shows up here."</p>
                            }.into_any();
                        }
                        all.into_iter().map(|m| {
                            let (v, n, t) = (m.vault.clone(), m.note.clone(), m.mention.thread.clone());
                            let snippet: String = m.mention.body.split_whitespace().collect::<Vec<_>>().join(" ");
                            view! {
                                <button class="mention-item" class:unread=m.unread on:click=move |_| {
                                    open.set(false);
                                    step(|c| state::mark_thread_read(c, &v, &n, &t));
                                    state::open_mention(&v, &n, &t);
                                }>
                                    {avatar(&m.name, &m.mention.author)}
                                    <span class="mention-text">
                                        <span class="mention-title"><b>{m.name.clone()}</b>" in "<b>{m.note_title.clone()}</b></span>
                                        <span class="mention-snip">{snippet}</span>
                                        <span class="muted small">{format!("{} · {}{}", m.vault_name, util::relative_time(m.mention.created), if m.mention.resolved { " · resolved" } else { "" })}</span>
                                    </span>
                                    {m.unread.then(|| view! { <span class="dot" aria-label="new"></span> })}
                                </button>
                            }
                        }).collect_view().into_any()
                    }}
                </div>
            </Show>
        </div>
    }
}
