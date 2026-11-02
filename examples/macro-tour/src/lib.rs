//! A tour of the producer attributes: one of each, in a small address book.
//!
//! The producer macro guide and the async guide include this file region by
//! region (the `ANCHOR` comments), so every Rust snippet they show is
//! compiled, and `src/tests.rs` calls the C functions the attributes
//! generate the way a consumer does.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use weaveffi::{CancelToken, ForeignError};

// ANCHOR: functions
/// Add two integers, wrapping on overflow.
#[weaveffi::export]
pub fn add(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

/// The number of words in `text`.
#[weaveffi::export]
pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// The arithmetic mean of `values`, or none for an empty list.
#[weaveffi::export]
pub fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}
// ANCHOR_END: functions

// ANCHOR: errors
/// The address book's error domain. Each variant's value is its stable
/// code, and its fields travel with the error as its payload.
#[weaveffi::error]
#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(i32)]
pub enum BookError {
    /// contact not found
    #[weaveffi(message = "no contact with id {id}")]
    NotFound {
        /// The id that was looked up.
        id: u64,
    } = 1,
    /// the book is full
    #[weaveffi(message = "the book is full ({capacity} contacts)")]
    Full {
        /// The book's capacity.
        capacity: u32,
    } = 2,
    /// a contact needs a name
    EmptyName = 3,
    /// a listener rejected the contact
    #[weaveffi(message = "a listener rejected {name}")]
    Rejected {
        /// The rejected contact's name.
        name: String,
    } = 4,
    /// a listener failed
    #[weaveffi(message = "{message}")]
    ListenerFailed {
        /// The listener's message.
        message: String,
    } = 5,
}

/// Any listener failure that isn't a `BookError` code arrives as a
/// `ForeignError`; this keeps it typed for the original caller.
impl From<ForeignError> for BookError {
    fn from(e: ForeignError) -> Self {
        Self::ListenerFailed { message: e.message }
    }
}
// ANCHOR_END: errors

// ANCHOR: untyped
/// Parse a decimal integer. Any `std::error::Error` converts into
/// `weaveffi::Error` with `?`, so this function throws an untyped error
/// carrying the parse error's message.
#[weaveffi::export]
pub fn parse_number(text: &str) -> Result<i64, weaveffi::Error> {
    Ok(text.trim().parse()?)
}

/// The square root of `x`. A `String` error is untyped too.
#[weaveffi::export]
pub fn checked_sqrt(x: f64) -> Result<f64, String> {
    if x < 0.0 {
        return Err(format!("cannot take the square root of {x}"));
    }
    Ok(x.sqrt())
}
// ANCHOR_END: untyped

// ANCHOR: types
/// How a contact is filed: a C-style enum, which crosses as an `int32_t`.
#[weaveffi::enumeration]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Friends and family.
    Personal,
    /// Colleagues.
    Work = 10,
    /// Everyone else (`11`, one more than the previous variant).
    Other,
}

/// One contact: a record, which crosses by value.
#[weaveffi::record]
#[derive(Clone, Debug, PartialEq)]
pub struct Contact {
    /// The book-assigned id.
    pub id: u64,
    /// The display name.
    pub name: String,
    /// An email address, if known.
    pub email: Option<String>,
    /// How the contact is filed.
    pub kind: Kind,
    /// Free-form labels.
    pub tags: Vec<String>,
}

/// A change to a book: a rich enum, whose variants carry fields.
#[weaveffi::enumeration]
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// A contact was added.
    Added {
        /// The new contact.
        contact: Contact,
    },
    /// A contact was removed.
    Removed {
        /// The removed contact's id.
        id: u64,
    },
    /// Every contact was removed.
    Cleared,
}
// ANCHOR_END: types

// ANCHOR: custom
/// A timeout: a `std::time::Duration`, crossing as whole milliseconds. The
/// bindings see a `u64`.
#[weaveffi::custom(repr = u64, lift = timeout_from_millis, lower = timeout_to_millis)]
pub type Timeout = Duration;

fn timeout_from_millis(ms: u64) -> Result<Duration, std::convert::Infallible> {
    Ok(Duration::from_millis(ms))
}

fn timeout_to_millis(timeout: &Duration) -> u64 {
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
}

/// An email address, crossing as its string. `lift` rejects a string
/// without an `@`, which fails the call as a marshalling error.
#[weaveffi::custom(repr = String, lift = Email::parse, lower = Email::to_text)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Email(String);

impl Email {
    fn parse(text: String) -> Result<Email, String> {
        if text.contains('@') {
            Ok(Email(text))
        } else {
            Err(format!("{text:?} is not an email address"))
        }
    }

    fn to_text(&self) -> String {
        self.0.clone()
    }
}

/// The domain of an email address (what follows the `@`).
#[weaveffi::export]
pub fn email_domain(email: Email) -> String {
    email.0.rsplit('@').next().unwrap_or_default().to_string()
}
// ANCHOR_END: custom

// ANCHOR: callback
/// Approves the contacts a book adds and hears about its changes. The
/// consumer implements it; the book calls it, from any thread.
#[weaveffi::callback_interface]
pub trait Listener: Send + Sync {
    /// Whether `contact` may be added. A consumer may also fail with one of
    /// `BookError`'s codes, which `add` then fails with.
    fn approve(&self, contact: &Contact) -> Result<bool, BookError>;

    /// A change the book made.
    fn on_change(&self, change: &Change) -> Result<(), ForeignError>;
}
// ANCHOR_END: callback

// ANCHOR: interface
/// An address book: an interface, a reference-counted object the bindings
/// wrap in a class.
#[weaveffi::interface]
pub struct Book {
    capacity: u32,
    next_id: AtomicU64,
    contacts: Mutex<BTreeMap<u64, Contact>>,
    listeners: Mutex<Vec<Arc<dyn Listener>>>,
}

#[weaveffi::export]
impl Book {
    /// An empty book holding up to `capacity` contacts (a constructor: it
    /// returns `Self`).
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity,
            next_id: AtomicU64::new(1),
            contacts: Mutex::new(BTreeMap::new()),
            listeners: Mutex::new(Vec::new()),
        }
    }

    /// The capacity of a book made by `new_default` (a static: no receiver,
    /// and it doesn't return the interface).
    pub fn default_capacity() -> u32 {
        100
    }

    /// An empty book with the default capacity.
    pub fn new_default() -> Self {
        Self::new(Self::default_capacity())
    }

    /// Add a contact, returning it with its new id. Fails with `EmptyName`,
    /// `Full`, or `Rejected`, or with a listener's own error.
    pub fn add(
        &self,
        name: String,
        email: Option<Email>,
        kind: Kind,
    ) -> Result<Contact, BookError> {
        if name.trim().is_empty() {
            return Err(BookError::EmptyName);
        }
        let contact = Contact {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            name,
            email: email.map(|e| e.0),
            kind,
            tags: Vec::new(),
        };
        // Never hold a lock while a consumer's callback runs: it may call
        // back into the book.
        for listener in self.listeners() {
            if !listener.approve(&contact)? {
                return Err(BookError::Rejected { name: contact.name });
            }
        }
        {
            let mut contacts = lock(&self.contacts);
            if contacts.len() >= self.capacity as usize {
                return Err(BookError::Full {
                    capacity: self.capacity,
                });
            }
            contacts.insert(contact.id, contact.clone());
        }
        self.notify(&Change::Added {
            contact: contact.clone(),
        });
        Ok(contact)
    }

    /// The contact with `id`. Fails with `NotFound`.
    pub fn get(&self, id: u64) -> Result<Contact, BookError> {
        lock(&self.contacts)
            .get(&id)
            .cloned()
            .ok_or(BookError::NotFound { id })
    }

    /// Remove the contact with `id`, returning whether it existed.
    pub fn remove(&self, id: u64) -> bool {
        let removed = lock(&self.contacts).remove(&id).is_some();
        if removed {
            self.notify(&Change::Removed { id });
        }
        removed
    }

    /// The number of contacts.
    pub fn count(&self) -> usize {
        lock(&self.contacts).len()
    }

    /// The number of contacts.
    #[deprecated(note = "use `count`")]
    pub fn size(&self) -> u32 {
        u32::try_from(self.count()).unwrap_or(u32::MAX)
    }

    /// Another reference to this same book (a method taking
    /// `self: Arc<Self>`).
    pub fn share(self: Arc<Self>) -> Arc<Book> {
        self
    }

    /// Lock the contacts for Rust callers. Its return type can't cross the
    /// boundary, so `skip` leaves it out of the bindings.
    #[weaveffi(skip)]
    pub fn contacts_mut(&self) -> MutexGuard<'_, BTreeMap<u64, Contact>> {
        lock(&self.contacts)
    }

    /// Remove every contact (exported only when the crate is built with
    /// its `admin` feature).
    #[cfg(feature = "admin")]
    pub fn purge(&self) -> u32 {
        let removed = std::mem::take(&mut *lock(&self.contacts));
        self.notify(&Change::Cleared);
        u32::try_from(removed.len()).unwrap_or(u32::MAX)
    }
}
// ANCHOR_END: interface

// ANCHOR: iterator
#[weaveffi::export]
impl Book {
    /// Every contact's name in id order, pulled lazily from a snapshot.
    pub fn names(&self) -> weaveffi::Iter<String> {
        let names: Vec<String> = lock(&self.contacts)
            .values()
            .map(|c| c.name.clone())
            .collect();
        weaveffi::Iter::new(names)
    }
}
// ANCHOR_END: iterator

// ANCHOR: subscribe
#[weaveffi::export]
impl Book {
    /// Subscribe `listener` until the book is released.
    pub fn subscribe(&self, listener: Arc<dyn Listener>) {
        lock(&self.listeners).push(listener);
    }
}
// ANCHOR_END: subscribe

// ANCHOR: async
#[weaveffi::export]
impl Book {
    /// Every contact, in id order, collected on a producer thread.
    pub async fn export_all(&self) -> Vec<Contact> {
        lock(&self.contacts).values().cloned().collect()
    }
}

/// Greet `name` after `delay`. The trailing `CancelToken` makes the
/// function cancellable: cancelling drops the future (and the sleep), and
/// the call completes as cancelled.
#[weaveffi::export]
pub async fn greet_later(name: String, delay: Timeout, _cancel: CancelToken) -> String {
    tokio::time::sleep(delay).await;
    format!("Hello, {name}!")
}
// ANCHOR_END: async

// ANCHOR: spawner
/// Run every async export on `handle`'s runtime instead of the default
/// executor. Call it once, before the first async call.
pub fn run_async_exports_on(handle: tokio::runtime::Handle) {
    let installed = weaveffi::set_spawner(move |future| {
        handle.spawn(future);
    });
    // Only the first spawner installed in a process takes effect.
    let _ = installed;
}
// ANCHOR_END: spawner

impl Book {
    fn listeners(&self) -> Vec<Arc<dyn Listener>> {
        lock(&self.listeners).clone()
    }

    fn notify(&self, change: &Change) {
        for listener in self.listeners() {
            // A listener's failure to observe a change doesn't fail the
            // change.
            let _ = listener.on_change(change);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ANCHOR: runtime
weaveffi::export_runtime!();
// ANCHOR_END: runtime

#[cfg(test)]
mod tests;
