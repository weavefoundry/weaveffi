//! Contacts sample: a small CRUD address book written as plain, safe Rust.
//!
//! `#[weaveffi::module]` reads the annotated items and generates the
//! `#[no_mangle] extern "C"` thunks that back the stable C ABI (and every
//! generated language binding). The address book is exported as an interface:
//! each `ContactBook` object owns its contacts directly (methods take `&self`
//! and guard the state with a `Mutex`, because the object is shared across the
//! FFI boundary), and fallible methods report typed `ContactsError` codes
//! through the ABI's error channel. The producer writes no `unsafe` glue.

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
    // through the interface constructor (interfaces still cross as opaque
    // pointers with `_new`/`_destroy`), drive its methods (including the
    // typed error path), and decode the buffered `Contact` returns. The
    // optional email parameter and the record results cross as value buffers.
    #[test]
    fn ffi_surface_smoke() {
        use super::contacts::{
            weaveffi_contacts_ContactBook_add, weaveffi_contacts_ContactBook_count,
            weaveffi_contacts_ContactBook_destroy, weaveffi_contacts_ContactBook_get,
            weaveffi_contacts_ContactBook_new, weaveffi_contacts_ContactBook_remove, Contact,
            ContactType,
        };
        use std::ffi::CString;
        use weaveffi::abi::{self, weaveffi_error};

        fn decode_and_free(ptr: *const u8, len: usize) -> Contact {
            assert!(!ptr.is_null());
            let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
            let contact = abi::decode_value::<Contact>(bytes).expect("well-formed Contact buffer");
            abi::free_bytes(ptr as *mut u8, len);
            contact
        }

        let mut err = weaveffi_error::default();
        let book = weaveffi_contacts_ContactBook_new(&mut err);
        assert_eq!(err.code, 0);
        assert!(!book.is_null());

        let first = CString::new("Zoe").unwrap();
        let last = CString::new("Quinn").unwrap();
        let email = abi::encode_value(&Some("zoe@example.com".to_string()));
        let mut added_len: usize = 0;
        let added_ptr = weaveffi_contacts_ContactBook_add(
            book,
            first.as_ptr(),
            last.as_ptr(),
            email.as_ptr(),
            email.len(),
            ContactType::Work as i32,
            &mut added_len,
            &mut err,
        );
        assert_eq!(err.code, 0);
        let added = decode_and_free(added_ptr, added_len);
        assert!(added.id > 0);
        assert_eq!(added.first_name, "Zoe");
        assert_eq!(added.email.as_deref(), Some("zoe@example.com"));

        // An empty first name reports the InvalidName domain code.
        let empty = CString::new("").unwrap();
        let no_email = abi::encode_value(&None::<String>);
        let mut rejected_len: usize = 0;
        let rejected = weaveffi_contacts_ContactBook_add(
            book,
            empty.as_ptr(),
            last.as_ptr(),
            no_email.as_ptr(),
            no_email.len(),
            ContactType::Work as i32,
            &mut rejected_len,
            &mut err,
        );
        assert!(rejected.is_null());
        assert_eq!(err.code, 1);
        abi::error_clear(&mut err);

        let mut fetched_len: usize = 0;
        let fetched_ptr =
            weaveffi_contacts_ContactBook_get(book, added.id, &mut fetched_len, &mut err);
        assert_eq!(err.code, 0);
        let fetched = decode_and_free(fetched_ptr, fetched_len);
        assert_eq!(fetched.first_name, "Zoe");
        assert_eq!(fetched.contact_type, ContactType::Work);

        assert_eq!(weaveffi_contacts_ContactBook_count(book, &mut err), 1);
        assert!(weaveffi_contacts_ContactBook_remove(
            book, added.id, &mut err
        ));

        // A missing id reports the NotFound domain code.
        let mut missing_len: usize = 0;
        let missing = weaveffi_contacts_ContactBook_get(book, added.id, &mut missing_len, &mut err);
        assert!(missing.is_null());
        assert_eq!(err.code, 2);
        abi::error_clear(&mut err);

        weaveffi_contacts_ContactBook_destroy(book);
    }
}
