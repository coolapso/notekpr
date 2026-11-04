//! Questions the app asks in its own dialog, never the browser's boxes
//! (`window.confirm`, `window.prompt`): a yes or no before an action, or a
//! line of text.
//!
//! Build one, then await the answer:
//!
//! ```ignore
//! if !ask::confirm("Remove this person?")
//!     .text("They keep anything they already downloaded, but can't read anything new.")
//!     .button("Remove")
//!     .danger()
//!     .await
//! {
//!     return;
//! }
//! ```
//!
//! The dialog (`views::ask::AskHost`) has a layer of its own, over the
//! modals and menus, so it can ask about something in an open dialog (a
//! person's role in the Share dialog). Awaiting never holds the app state:
//! take it with `state::step` before or after, as anywhere else. Event
//! handlers ask in a task (`leptos::task::spawn_local`).
//!
//! One question shows at a time: asking another while one is open answers
//! the first with No (Cancel).

use std::cell::{Cell, RefCell};
use std::future::{Future, IntoFuture};
use std::marker::PhantomData;
use std::pin::Pin;

use futures::channel::oneshot;
use leptos::prelude::*;

use crate::state::ui;

/// The question on screen, for the dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    /// Tells this question from the one before (an answer is for one).
    pub id: u64,
    /// The question itself, short: the dialog's title.
    pub title: String,
    /// What it means, under the title; paragraphs are separated by a blank
    /// line.
    pub text: String,
    /// The confirm button, named after what it does ("Remove", "Make
    /// owner"…).
    pub button: String,
    /// It deletes or takes something away: the button shows it.
    pub danger: bool,
    /// A line of text to type, for [`input`].
    pub field: Option<Field>,
}

/// The text field of an [`input`] question.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Field {
    pub label: String,
    /// What it holds at first.
    pub value: String,
    pub placeholder: String,
    /// The confirm button waits for something typed.
    pub required: bool,
    /// The confirm button waits for exactly this text (e.g. a vault's name
    /// before deleting it).
    pub must_be: Option<String>,
}

impl Field {
    /// Whether `typed` may be confirmed.
    pub fn accepts(&self, typed: &str) -> bool {
        match &self.must_be {
            Some(m) => typed == m,
            None => !self.required || !typed.trim().is_empty(),
        }
    }
}

/// A question being put together; await it for the answer: `bool` for
/// [`confirm`] (Yes), `Option<String>` for [`input`] (what was typed, or
/// `None` for Cancel).
#[must_use = "a question is asked when awaited"]
pub struct Ask<A> {
    q: Question,
    answer: PhantomData<A>,
}

/// A yes-or-no question before an action.
pub fn confirm(title: impl Into<String>) -> Ask<bool> {
    Ask::new(title, None)
}

/// A question answered with a line of text.
pub fn input(title: impl Into<String>) -> Ask<Option<String>> {
    Ask::new(title, Some(Field::default()))
}

impl<A> Ask<A> {
    fn new(title: impl Into<String>, field: Option<Field>) -> Self {
        Ask {
            q: Question {
                id: 0,
                title: title.into(),
                text: String::new(),
                button: "OK".into(),
                danger: false,
                field,
            },
            answer: PhantomData,
        }
    }

    /// What it means, under the question.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.q.text = text.into();
        self
    }

    /// The confirm button's name: what it does.
    pub fn button(mut self, label: impl Into<String>) -> Self {
        self.q.button = label.into();
        self
    }

    /// It deletes or takes something away: the button shows it, and Cancel
    /// has the focus (Enter cancels).
    pub fn danger(mut self) -> Self {
        self.q.danger = true;
        self
    }
}

impl Ask<Option<String>> {
    fn field(mut self, f: impl FnOnce(&mut Field)) -> Self {
        f(self.q.field.get_or_insert_with(Field::default));
        self
    }

    /// The field's label.
    pub fn label(self, label: impl Into<String>) -> Self {
        let label = label.into();
        self.field(|f| f.label = label)
    }

    /// What the field holds at first.
    pub fn value(self, value: impl Into<String>) -> Self {
        let value = value.into();
        self.field(|f| f.value = value)
    }

    pub fn placeholder(self, placeholder: impl Into<String>) -> Self {
        let placeholder = placeholder.into();
        self.field(|f| f.placeholder = placeholder)
    }

    /// Only something typed can be confirmed.
    pub fn required(self) -> Self {
        self.field(|f| f.required = true)
    }

    /// Only exactly `text` can be confirmed.
    pub fn must_be(self, text: impl Into<String>) -> Self {
        let text = text.into();
        self.field(|f| f.must_be = Some(text))
    }
}

type Answer<T> = Pin<Box<dyn Future<Output = T>>>;

impl IntoFuture for Ask<bool> {
    type Output = bool;
    type IntoFuture = Answer<bool>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { put(self.q).await.is_some() })
    }
}

impl IntoFuture for Ask<Option<String>> {
    type Output = Option<String>;
    type IntoFuture = Answer<Option<String>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(put(self.q))
    }
}

thread_local! {
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    /// Who waits for the answer to the question on screen.
    static WAITING: RefCell<Option<(u64, oneshot::Sender<Option<String>>)>> = const { RefCell::new(None) };
}

/// Show `q` and wait for the answer: `Some` (what was typed; empty for a
/// yes-or-no question) or `None` for Cancel.
async fn put(mut q: Question) -> Option<String> {
    let id = NEXT_ID.with(|n| n.replace(n.get() + 1));
    q.id = id;
    let (tx, rx) = oneshot::channel();
    if let Some((_, before)) = WAITING.with(|w| w.borrow_mut().replace((id, tx))) {
        let _ = before.send(None);
    }
    ui().asking.set(Some(q));
    rx.await.ok().flatten()
}

/// The dialog's answer to question `id` (`None`: Cancel). Nothing if that
/// question was already answered.
pub fn answer(id: u64, answer: Option<String>) {
    let waiting = WAITING.with(|w| {
        let mut w = w.borrow_mut();
        if w.as_ref().is_some_and(|(at, _)| *at == id) {
            w.take()
        } else {
            None
        }
    });
    if let Some((_, tx)) = waiting {
        ui().asking.set(None);
        let _ = tx.send(answer);
    }
}

/// Answer the question on screen, if any, with Cancel (e.g. signed out
/// while it was open).
pub fn cancel() {
    if let Some(id) = ui().asking.with_untracked(|q| q.as_ref().map(|q| q.id)) {
        answer(id, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_accepts_what_it_is_told_to() {
        let free = Field::default();
        assert!(free.accepts("") && free.accepts("x"));
        let required = Field {
            required: true,
            ..Field::default()
        };
        assert!(!required.accepts("  ") && required.accepts("Laptop"));
        let exact = Field {
            must_be: Some("Family".into()),
            ..Field::default()
        };
        assert!(!exact.accepts("family") && !exact.accepts("Family ") && exact.accepts("Family"));
    }
}
