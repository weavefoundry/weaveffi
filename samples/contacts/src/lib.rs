//! Contacts sample: a small CRUD address book written as plain, safe Rust.
//!
//! `#[weaveffi::module]` reads the annotated items and generates the
//! `extern "C"` thunks that back the stable C ABI (and every generated
//! language binding). The address book is exported as an interface: each
//! `ContactBook` object owns its contacts directly (methods take `&self` and
//! guard the state with a `Mutex`, because the object is shared across the
//! FFI boundary), and fallible methods report typed `ContactsError` codes
//! through the ABI's error channel. The producer writes no `unsafe` glue.
//!
//! The sample also shows the two ways modules relate:
//!
//! * `contacts::groups` is nested under the `contacts` root, so it may use
//!   any of the parent's types, including the `ContactType` C-style enum and
//!   the `ContactBook` interface, and it inherits the parent's error domain.
//! * `directory` is a sibling root. A module tree only sees its own
//!   declarations, so across roots only records and rich enums (value
//!   types) may be shared; it uses the `Contact` record.

#[weaveffi::module]
pub mod contacts {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

    /// The address book's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum ContactsError {
        /// name must not be empty
        InvalidName = 1,
        /// contact not found
        NotFound = 2,
    }

    impl std::fmt::Display for ContactsError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(match self {
                Self::InvalidName => "name must not be empty",
                Self::NotFound => "contact not found",
            })
        }
    }

    /// How a contact is classified.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ContactType {
        /// A personal contact.
        Personal = 0,
        /// A work contact.
        Work = 1,
        /// Any other classification.
        Other = 2,
    }

    /// A single address-book entry.
    #[weaveffi::record]
    #[derive(Clone, Debug)]
    pub struct Contact {
        /// Stable identifier assigned on creation.
        pub id: i64,
        /// Given name.
        pub first_name: String,
        /// Family name.
        pub last_name: String,
        /// Optional email address.
        pub email: Option<String>,
        /// How the contact is classified.
        pub contact_type: ContactType,
    }

    /// An in-memory address book exported as an interface. Each book owns its
    /// contacts and id counter directly; destroying the object (via the
    /// generated destroy symbol) releases that state.
    #[weaveffi::interface]
    pub struct ContactBook {
        contacts: Mutex<Vec<Contact>>,
        next_id: AtomicI64,
    }

    impl Default for ContactBook {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ContactBook {
        /// Create an empty address book.
        pub fn new() -> Self {
            ContactBook {
                contacts: Mutex::new(Vec::new()),
                next_id: AtomicI64::new(1),
            }
        }

        /// Add a contact, returning the stored record with its assigned id.
        /// An empty first or last name is rejected with
        /// [`ContactsError::InvalidName`].
        pub fn add(
            &self,
            first_name: String,
            last_name: String,
            email: Option<String>,
            contact_type: ContactType,
        ) -> Result<Contact, ContactsError> {
            if first_name.is_empty() || last_name.is_empty() {
                return Err(ContactsError::InvalidName);
            }
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let contact = Contact {
                id,
                first_name,
                last_name,
                email,
                contact_type,
            };
            self.contacts.lock().unwrap().push(contact.clone());
            Ok(contact)
        }

        /// Look up a contact by id, failing with [`ContactsError::NotFound`]
        /// when none exists.
        pub fn get(&self, id: i64) -> Result<Contact, ContactsError> {
            self.contacts
                .lock()
                .unwrap()
                .iter()
                .find(|c| c.id == id)
                .cloned()
                .ok_or(ContactsError::NotFound)
        }

        /// List every stored contact.
        pub fn list(&self) -> Vec<Contact> {
            self.contacts.lock().unwrap().clone()
        }

        /// Remove a contact by id, returning whether it existed.
        pub fn remove(&self, id: i64) -> bool {
            let mut contacts = self.contacts.lock().unwrap();
            let before = contacts.len();
            contacts.retain(|c| c.id != id);
            contacts.len() < before
        }

        /// Count the stored contacts.
        pub fn count(&self) -> i32 {
            self.contacts.lock().unwrap().len() as i32
        }
    }

    /// Group queries over a book. Nested under the `contacts` root, so its
    /// functions take and return the parent's `ContactType` enum and
    /// `ContactBook` interface, and report the parent's `ContactsError`.
    #[weaveffi::module]
    pub mod groups {
        use super::{Contact, ContactBook, ContactType, ContactsError};

        /// Count the contacts of one type.
        #[weaveffi::export]
        pub fn count_of_type(book: &ContactBook, contact_type: ContactType) -> i32 {
            book.list()
                .iter()
                .filter(|c| c.contact_type == contact_type)
                .count() as i32
        }

        /// The most common contact type in a book (`Personal` when empty or
        /// tied).
        #[weaveffi::export]
        pub fn dominant_type(book: &ContactBook) -> ContactType {
            let count = |t| count_of_type(book, t);
            [ContactType::Work, ContactType::Other]
                .into_iter()
                .filter(|t| count(*t) > count(ContactType::Personal))
                .max_by_key(|t| count(*t))
                .unwrap_or(ContactType::Personal)
        }

        /// Copy every contact of one type into a new book.
        #[weaveffi::export]
        pub fn split_by_type(book: &ContactBook, contact_type: ContactType) -> ContactBook {
            let out = ContactBook::new();
            for c in book
                .list()
                .into_iter()
                .filter(|c| c.contact_type == contact_type)
            {
                // The names were validated when the contact was first added.
                let _ = out.add(c.first_name, c.last_name, c.email, c.contact_type);
            }
            out
        }

        /// The first contact of one type, failing with the parent domain's
        /// [`ContactsError::NotFound`] when there is none.
        #[weaveffi::export]
        pub fn first_of_type(
            book: &ContactBook,
            contact_type: ContactType,
        ) -> Result<Contact, ContactsError> {
            book.list()
                .into_iter()
                .find(|c| c.contact_type == contact_type)
                .ok_or(ContactsError::NotFound)
        }
    }
}

/// Directory formatting. A sibling root of `contacts`, so it shares only the
/// `Contact` record (a value type) with it.
#[weaveffi::module]
pub mod directory {
    use super::contacts::Contact;

    /// The directory's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum DirectoryError {
        /// no contacts to list
        Empty = 1,
    }

    impl std::fmt::Display for DirectoryError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("no contacts to list")
        }
    }

    /// A contact formatted for display.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
    pub struct Card {
        /// `Last, First`.
        pub display_name: String,
        /// The first letters of the first and last names.
        pub initials: String,
        /// Whether the contact has an email address.
        pub has_email: bool,
    }

    /// Format one contact as a card.
    #[weaveffi::export]
    pub fn card(contact: &Contact) -> Card {
        let initial = |s: &str| s.chars().next().map(String::from).unwrap_or_default();
        Card {
            display_name: format!("{}, {}", contact.last_name, contact.first_name),
            initials: initial(&contact.first_name) + &initial(&contact.last_name),
            has_email: contact.email.is_some(),
        }
    }

    /// Sort contacts by last name, then first name, failing with
    /// [`DirectoryError::Empty`] for an empty list.
    #[weaveffi::export]
    pub fn sorted(mut contacts: Vec<Contact>) -> Result<Vec<Contact>, DirectoryError> {
        if contacts.is_empty() {
            return Err(DirectoryError::Empty);
        }
        contacts.sort_by(|a, b| (&a.last_name, &a.first_name).cmp(&(&b.last_name, &b.first_name)));
        Ok(contacts)
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::contacts::{ContactBook, ContactType, ContactsError};

    #[test]
    fn create_and_get() {
        let book = ContactBook::new();
        let added = book
            .add(
                "Alice".into(),
                "Smith".into(),
                Some("alice@example.com".into()),
                ContactType::Work,
            )
            .expect("valid contact");
        assert!(added.id > 0);
        let c = book.get(added.id).expect("contact exists");
        assert_eq!(c.first_name, "Alice");
        assert_eq!(c.last_name, "Smith");
        assert_eq!(c.email.as_deref(), Some("alice@example.com"));
        assert_eq!(c.contact_type, ContactType::Work);
    }

    #[test]
    fn create_without_email() {
        let book = ContactBook::new();
        let added = book
            .add("Bob".into(), "Jones".into(), None, ContactType::Personal)
            .unwrap();
        assert_eq!(book.get(added.id).unwrap().email, None);
    }

    #[test]
    fn add_empty_name_is_invalid() {
        let book = ContactBook::new();
        assert!(matches!(
            book.add("".into(), "Smith".into(), None, ContactType::Personal),
            Err(ContactsError::InvalidName)
        ));
        assert!(matches!(
            book.add("Ada".into(), "".into(), None, ContactType::Personal),
            Err(ContactsError::InvalidName)
        ));
        assert_eq!(book.count(), 0);
    }

    #[test]
    fn get_missing_is_not_found() {
        let book = ContactBook::new();
        assert!(matches!(book.get(999), Err(ContactsError::NotFound)));
    }

    #[test]
    fn count_and_list() {
        let book = ContactBook::new();
        assert_eq!(book.count(), 0);
        book.add("A".into(), "B".into(), None, ContactType::Personal)
            .unwrap();
        book.add("C".into(), "D".into(), None, ContactType::Work)
            .unwrap();
        assert_eq!(book.count(), 2);
        assert_eq!(book.list().len(), 2);
    }

    #[test]
    fn remove_deletes() {
        let book = ContactBook::new();
        let added = book
            .add("Del".into(), "Me".into(), None, ContactType::Other)
            .unwrap();
        assert_eq!(book.count(), 1);
        assert!(book.remove(added.id));
        assert_eq!(book.count(), 0);
        assert!(!book.remove(added.id));
    }

    // A direct exercise of the generated C ABI thunks: construct a book
    // through the interface constructor, drive its methods (including the
    // typed error path), and decode the buffered `Contact` returns. Strings
    // cross as `(ptr, len)`; the optional email and the records cross as
    // value buffers.
    mod ffi {
        use super::super::contacts::groups::*;
        use super::super::contacts::*;
        use super::super::directory::*;
        use weaveffi::abi::{self, FfiError};

        fn decode<T: abi::BufferValue>(ptr: *const u8, len: usize) -> T {
            assert!(!ptr.is_null());
            let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
            let value = abi::decode_value::<T>(bytes).expect("well-formed value buffer");
            unsafe { abi::free_bytes(ptr.cast_mut(), len) };
            value
        }

        fn add(book: *mut ContactBook, first: &str, last: &str, t: ContactType) -> Contact {
            let mut err = FfiError::default();
            let email = abi::encode_value(&None::<String>);
            let mut len = 0usize;
            let ptr = unsafe {
                contacts_contacts_ContactBook_add(
                    book,
                    first.as_ptr(),
                    first.len(),
                    last.as_ptr(),
                    last.len(),
                    email.as_ptr(),
                    email.len(),
                    t as i32,
                    &mut len,
                    &mut err,
                )
            };
            assert_eq!(err.code, 0);
            decode(ptr, len)
        }

        #[test]
        fn book_surface() {
            let mut err = FfiError::default();
            let book = unsafe { contacts_contacts_ContactBook_new(&mut err) };
            assert_eq!(err.code, 0);
            assert!(!book.is_null());

            let (first, last) = ("Zoe", "Quinn");
            let email = abi::encode_value(&Some("zoe@example.com".to_string()));
            let mut len = 0usize;
            let ptr = unsafe {
                contacts_contacts_ContactBook_add(
                    book,
                    first.as_ptr(),
                    first.len(),
                    last.as_ptr(),
                    last.len(),
                    email.as_ptr(),
                    email.len(),
                    ContactType::Work as i32,
                    &mut len,
                    &mut err,
                )
            };
            assert_eq!(err.code, 0);
            let added: Contact = decode(ptr, len);
            assert!(added.id > 0);
            assert_eq!(added.first_name, "Zoe");
            assert_eq!(added.email.as_deref(), Some("zoe@example.com"));

            // An empty first name reports the InvalidName domain code.
            let rejected = unsafe {
                contacts_contacts_ContactBook_add(
                    book,
                    std::ptr::null(),
                    0,
                    last.as_ptr(),
                    last.len(),
                    email.as_ptr(),
                    email.len(),
                    ContactType::Work as i32,
                    &mut len,
                    &mut err,
                )
            };
            assert!(rejected.is_null());
            assert_eq!(err.code, 1);
            assert_eq!(unsafe { err.message_str() }, Some("name must not be empty"));

            let ptr =
                unsafe { contacts_contacts_ContactBook_get(book, added.id, &mut len, &mut err) };
            assert_eq!(err.code, 0);
            let fetched: Contact = decode(ptr, len);
            assert_eq!(fetched.contact_type, ContactType::Work);

            assert_eq!(
                unsafe { contacts_contacts_ContactBook_count(book, &mut err) },
                1
            );
            assert!(unsafe { contacts_contacts_ContactBook_remove(book, added.id, &mut err) });

            // A missing id reports the NotFound domain code.
            let missing =
                unsafe { contacts_contacts_ContactBook_get(book, added.id, &mut len, &mut err) };
            assert!(missing.is_null());
            assert_eq!(err.code, 2);

            unsafe { contacts_contacts_ContactBook_destroy(book) };
        }

        #[test]
        fn nested_module_uses_the_parent_enum_interface_and_errors() {
            let mut err = FfiError::default();
            let book = unsafe { contacts_contacts_ContactBook_new(&mut err) };
            add(book, "Ann", "Lee", ContactType::Work);
            add(book, "Bo", "Kim", ContactType::Work);
            add(book, "Cy", "Ng", ContactType::Personal);

            let work = ContactType::Work as i32;
            assert_eq!(
                unsafe { contacts_contacts_groups_count_of_type(book, work, &mut err) },
                2
            );
            assert_eq!(
                unsafe { contacts_contacts_groups_dominant_type(book, &mut err) },
                work
            );

            let split = unsafe { contacts_contacts_groups_split_by_type(book, work, &mut err) };
            assert_eq!(err.code, 0);
            assert_eq!(
                unsafe { contacts_contacts_ContactBook_count(split, &mut err) },
                2
            );
            unsafe { contacts_contacts_ContactBook_destroy(split) };

            let mut len = 0usize;
            let ptr =
                unsafe { contacts_contacts_groups_first_of_type(book, work, &mut len, &mut err) };
            assert_eq!(decode::<Contact>(ptr, len).first_name, "Ann");
            let other = ContactType::Other as i32;
            let none =
                unsafe { contacts_contacts_groups_first_of_type(book, other, &mut len, &mut err) };
            assert!(none.is_null());
            assert_eq!(err.code, 2, "the parent domain's NotFound code");

            // An out-of-range discriminant is a marshalling failure.
            assert_eq!(
                unsafe { contacts_contacts_groups_count_of_type(book, 9, &mut err) },
                0
            );
            assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
            unsafe { contacts_contacts_ContactBook_destroy(book) };
        }

        #[test]
        fn sibling_root_shares_the_contact_record() {
            let mut err = FfiError::default();
            let book = unsafe { contacts_contacts_ContactBook_new(&mut err) };
            let zoe = add(book, "Zoe", "Quinn", ContactType::Personal);
            let ada = add(book, "Ada", "Byron", ContactType::Work);
            unsafe { contacts_contacts_ContactBook_destroy(book) };

            let bytes = abi::encode_value(&zoe);
            let mut len = 0usize;
            let ptr =
                unsafe { contacts_directory_card(bytes.as_ptr(), bytes.len(), &mut len, &mut err) };
            assert_eq!(
                decode::<Card>(ptr, len),
                Card {
                    display_name: "Quinn, Zoe".into(),
                    initials: "ZQ".into(),
                    has_email: false,
                }
            );

            let list = abi::encode_value(&vec![zoe, ada]);
            let ptr =
                unsafe { contacts_directory_sorted(list.as_ptr(), list.len(), &mut len, &mut err) };
            let sorted: Vec<Contact> = decode(ptr, len);
            assert_eq!(sorted[0].last_name, "Byron");

            let empty = abi::encode_value(&Vec::<Contact>::new());
            let none = unsafe {
                contacts_directory_sorted(empty.as_ptr(), empty.len(), &mut len, &mut err)
            };
            assert!(none.is_null());
            assert_eq!(err.code, 1);
            assert_eq!(unsafe { err.message_str() }, Some("no contacts to list"));
        }

        #[test]
        fn each_root_exports_a_checksum() {
            assert_ne!(contacts_contacts_checksum(), contacts_directory_checksum());
        }
    }
}
