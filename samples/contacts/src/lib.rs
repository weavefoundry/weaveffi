//! Contacts sample: a small CRUD address book written as plain, safe Rust.
//!
//! `#[weaveffi::module]` reads the annotated items and generates the
//! `#[no_mangle] extern "C"` thunks that back the stable C ABI (and every
//! generated language binding). The producer keeps its own in-memory store in
//! safe Rust and writes no `unsafe` FFI glue: opaque `u64` handles are just
//! integer keys into the store, and `Result` returns surface as the ABI's
//! `out_err` channel.

#[weaveffi::module]
pub mod contacts {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

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

    static STORE: Mutex<Vec<Contact>> = Mutex::new(Vec::new());
    static NEXT_ID: AtomicI64 = AtomicI64::new(1);

    /// Create a contact, returning its opaque handle.
    #[weaveffi::export]
    pub fn create_contact(
        first_name: String,
        last_name: String,
        email: Option<String>,
        contact_type: ContactType,
    ) -> u64 {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        STORE.lock().unwrap().push(Contact {
            id,
            first_name,
            last_name,
            email,
            contact_type,
        });
        id as u64
    }

    /// Look up a contact by handle, erroring if none exists.
    #[weaveffi::export]
    pub fn get_contact(id: u64) -> Result<Contact, String> {
        STORE
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.id == id as i64)
            .cloned()
            .ok_or_else(|| format!("contact {id} not found"))
    }

    /// List every stored contact.
    #[weaveffi::export]
    pub fn list_contacts() -> Vec<Contact> {
        STORE.lock().unwrap().clone()
    }

    /// Delete a contact by handle, returning whether it existed.
    #[weaveffi::export]
    pub fn delete_contact(id: u64) -> bool {
        let mut store = STORE.lock().unwrap();
        let before = store.len();
        store.retain(|c| c.id != id as i64);
        store.len() < before
    }

    /// Count the stored contacts.
    #[weaveffi::export]
    pub fn count_contacts() -> i32 {
        STORE.lock().unwrap().len() as i32
    }

    /// Reset the in-memory store. Test-only helper (not part of the ABI).
    #[cfg(test)]
    pub(crate) fn reset() {
        STORE.lock().unwrap().clear();
        NEXT_ID.store(1, Ordering::Relaxed);
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
mod tests {
    use super::contacts::{
        count_contacts, create_contact, delete_contact, get_contact, list_contacts, reset,
        ContactType,
    };
    use std::sync::Mutex;

    // The store is process-global, so serialize the tests that touch it.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        g
    }

    #[test]
    fn create_and_get() {
        let _g = guard();
        let id = create_contact(
            "Alice".into(),
            "Smith".into(),
            Some("alice@example.com".into()),
            ContactType::Work,
        );
        assert!(id > 0);
        let c = get_contact(id).expect("contact exists");
        assert_eq!(c.first_name, "Alice");
        assert_eq!(c.last_name, "Smith");
        assert_eq!(c.email.as_deref(), Some("alice@example.com"));
        assert_eq!(c.contact_type, ContactType::Work);
    }

    #[test]
    fn create_without_email() {
        let _g = guard();
        let id = create_contact("Bob".into(), "Jones".into(), None, ContactType::Personal);
        assert_eq!(get_contact(id).unwrap().email, None);
    }

    #[test]
    fn get_missing_is_err() {
        let _g = guard();
        assert!(get_contact(999).is_err());
    }

    #[test]
    fn count_and_list() {
        let _g = guard();
        assert_eq!(count_contacts(), 0);
        create_contact("A".into(), "B".into(), None, ContactType::Personal);
        create_contact("C".into(), "D".into(), None, ContactType::Work);
        assert_eq!(count_contacts(), 2);
        assert_eq!(list_contacts().len(), 2);
    }

    #[test]
    fn delete_removes() {
        let _g = guard();
        let id = create_contact("Del".into(), "Me".into(), None, ContactType::Other);
        assert_eq!(count_contacts(), 1);
        assert!(delete_contact(id));
        assert_eq!(count_contacts(), 0);
        assert!(!delete_contact(id));
    }

    // A direct exercise of the generated C ABI thunks: create through the
    // `extern "C"` entry point, read a field getter, and free the result.
    #[test]
    fn ffi_surface_smoke() {
        use super::contacts::{
            weaveffi_contacts_Contact_destroy, weaveffi_contacts_Contact_get_first_name,
            weaveffi_contacts_create_contact, weaveffi_contacts_get_contact,
        };
        use std::ffi::CString;
        use weaveffi::abi::{c_ptr_to_string, free_string, weaveffi_error};

        let _g = guard();
        let mut err = weaveffi_error::default();
        let first = CString::new("Zoe").unwrap();
        let last = CString::new("Quinn").unwrap();
        let handle = weaveffi_contacts_create_contact(
            first.as_ptr(),
            last.as_ptr(),
            std::ptr::null(),
            ContactType::Work as i32,
            &mut err,
        );
        assert_eq!(err.code, 0);
        assert!(handle > 0);

        let c = weaveffi_contacts_get_contact(handle, &mut err);
        assert_eq!(err.code, 0);
        assert!(!c.is_null());

        let fname = weaveffi_contacts_Contact_get_first_name(c);
        assert_eq!(c_ptr_to_string(fname).unwrap(), "Zoe");
        free_string(fname);

        weaveffi_contacts_Contact_destroy(c);
    }
}
